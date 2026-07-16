//! IPC Server — Daemon-Side Listener (Unix Datagram, UDP, Windows Named Pipe)
//!
//! The daemon binds a transport and listens for incoming request packets.
//! Each request is dispatched to the appropriate service, and responses
//! are streamed back to the CLI.
//!
//! Transports (ADR-021, ADR-038):
//!   - **Unix**: Unix domain datagram socket at `~/.peko/run/daemon.sock`
//!     (file mode 0600 — kernel-enforced peer identity).
//!   - **Windows**: Named pipe at `\\.\pipe\peko-{username}` (ADR-038) with
//!     a DACL that grants the current user Generic All — kernel-enforced
//!     peer identity analogous to Unix 0600.
//!   - **All platforms**: UDP on `127.0.0.1:11435` is the explicit-remote
//!     transport and the universal last-resort safety net.
//!
//! Response writes are abstracted over the [`ResponseSink`](super::response_sink::ResponseSink)
//! trait so the giant `handle_request` match is platform-agnostic: Unix/UDP
//! call sites construct a per-request sink that captures the peer address
//! returned by `recv_from`; the Windows call site constructs a per-connection
//! sink over a `&mut NamedPipeServer`. See `response_sink.rs` for the
//! full design.

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
#[cfg(unix)]
use tokio::net::UnixDatagram;
use tracing::{error, info, trace, warn};

use super::packet::{AuthenticatedRequest, RequestPacket, ResponsePacket};
use super::response_sink::{sink_for_unix_or_udp, ResponseSink};
#[cfg(windows)]
use super::{default_pipe_name, response_sink::sink_for_pipe, DAEMON_PIPE_ENV};
use super::{ensure_run_dir, DEFAULT_HOST, DEFAULT_PORT};
use crate::auth::caller::CallerContext;
#[cfg(not(windows))]
use crate::auth::config::enforce_auth_for_public_bind;
use crate::auth::permissions::AuthError;
use crate::daemon::state::AppState;

/// `SO_SNDBUF` applied to both the Unix datagram and UDP server sockets
/// at bind time.
///
/// Why this exists: macOS's default `SO_SNDBUF` for `AF_UNIX/SOCK_DGRAM`
/// is **2048 bytes**, which is *smaller* than the serialised
/// `provider_list` response once the catalog carries more than a
/// handful of providers (each `ProviderInfo` entry is ~150 B). The
/// runtime's `send_to` then returns `EMSGSIZE` ("Message too long",
/// os error 40), the handler logs the error, and the client silently
/// hangs waiting for a reply that never arrives. The UDP default
/// (~8 KiB on Linux, ~9 KiB on macOS) is also too small for the
/// combined `runtime_info` + `extension_list` payloads a single
/// `system_status` request can produce.
///
/// 256 KiB is generous enough that every response the runtime ships
/// today — and the larger composite ones future handlers will produce
/// — fits atomically, while staying well under any sensible per-socket
/// memory budget on a developer workstation.
const IPC_SEND_BUFFER_BYTES: usize = 256 * 1024;

/// Platform-specific server socket (wrapped in Arc for shared ownership)
#[derive(Clone)]
pub enum ServerSocket {
    #[cfg(unix)]
    Unix {
        socket: Arc<UnixDatagram>,
        #[allow(dead_code)]
        path: Arc<std::path::PathBuf>,
    },
    Udp {
        socket: Arc<UdpSocket>,
    },
    #[cfg(windows)]
    NamedPipe {
        // Tokio 1.49 unifies the listener and per-connection end under
        // `NamedPipeServer`. The variant holds the server end that is
        // currently waiting on `accept()`, plus a cloneable `ServerOptions`
        // for creating the next instance per connection.
        listener: Arc<tokio::net::windows::named_pipe::NamedPipeServer>,
        server_options: Arc<tokio::net::windows::named_pipe::ServerOptions>,
    },
}

/// Subject address returned by `ServerSocket::recv_from` and threaded through
/// the request handlers so they can `send_to` the response back. Unix domain
/// datagram sockets return the sender's filesystem path; UDP returns a
/// `std::net::SocketAddr`. Windows named pipes (ADR-038) are
/// connection-oriented and have no per-message peer address — the
/// `Local` variant represents a connection that is local by construction.
#[derive(Debug, Clone)]
pub enum PeerAddr {
    #[cfg(unix)]
    Unix(std::path::PathBuf),
    Ip(std::net::SocketAddr),
    #[cfg(windows)]
    Local,
}

impl PeerAddr {
    /// True for local connections: Unix domain sockets (always local),
    /// named-pipe connections (always local — kernel checks the SID at
    /// `CreateFileW` time), and UDP peers on a loopback address. `None`
    /// (no peer info) is treated as local — the same convention the
    /// previous `Option<SocketAddr>` path used via
    /// `addr.map_or(true, |a| a.ip().is_loopback())`.
    fn is_local(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            Self::Ip(addr) => addr.ip().is_loopback(),
            #[cfg(windows)]
            Self::Local => true,
        }
    }
}

/// Raise the kernel-side `SO_SNDBUF` of a server socket to
/// [`IPC_SEND_BUFFER_BYTES`].
///
/// Tokio deliberately does not expose `set_send_buffer_size` on its
/// async socket wrappers (it wants to keep the surface area small and
/// the platform matrix narrow), so we drop down to `libc::setsockopt`
/// via `AsRawFd`. The kernel clamps the requested size to its
/// configured maximum, so passing a value that's larger than the
/// `kern.ipc.maxsockbuf` ceiling is harmless — we just get the
/// ceiling back. We log the actual size we observed in the success
/// case at the callsite; this helper only signals pass/fail.
///
/// Returns `Err` only if `setsockopt` itself fails (e.g. invalid fd),
/// not when the kernel clamps the request.
fn bump_send_buffer<S: AsRawFd>(socket: &S) -> std::io::Result<()> {
    let fd = socket.as_raw_fd();
    let buf_len = IPC_SEND_BUFFER_BYTES as libc::c_int;
    let buf_len_ptr = std::ptr::addr_of!(buf_len);
    // SAFETY: `fd` is a live socket owned by `socket`, and `buf_len` is
    // a valid `c_int`. `SOL_SOCKET` / `SO_SNDBUF` are the kernel
    // constants we want. `setsockopt` does not take ownership of the
    // fd and writes the requested buffer size back via the same fd.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            buf_len_ptr.cast::<libc::c_void>(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Raise the kernel-side `SO_RCVBUF` of a socket to
/// [`IPC_SEND_BUFFER_BYTES`].
///
/// Mirrors [`bump_send_buffer`] for the receive side. The round-trip
/// test needs this so the client socket can queue the large response
/// the server sends without dropping it with `ENOBUFS` on macOS.
#[cfg(all(unix, test))]
fn bump_recv_buffer<S: AsRawFd>(socket: &S) -> std::io::Result<()> {
    let fd = socket.as_raw_fd();
    let buf_len = IPC_SEND_BUFFER_BYTES as libc::c_int;
    let buf_len_ptr = std::ptr::addr_of!(buf_len);
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            buf_len_ptr.cast::<libc::c_void>(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl ServerSocket {
    /// Receive a packet from the socket
    ///
    /// On Unix, `recv_from` returns the sender's path as a `tokio::net::unix::SocketAddr`
    /// (which we normalise to a `PathBuf`); on UDP, the peer's `SocketAddr`.
    /// Either way the result is wrapped in [`PeerAddr`] so callers can
    /// hand it back to the per-request sink without losing type info.
    ///
    /// Windows named pipes (ADR-038) are connection-oriented — this
    /// method returns an error on the pipe variant, because the accept
    /// loop drives reads from the per-connection `NamedPipeServer`,
    /// not from a shared listener.
    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, PeerAddr)> {
        match self {
            #[cfg(unix)]
            Self::Unix { socket, .. } => {
                let (len, addr) = socket.recv_from(buf).await?;
                let path = addr.as_pathname().map(|p| p.to_path_buf()).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Unix peer without a filesystem path (anonymous socket?)",
                    )
                })?;
                Ok((len, PeerAddr::Unix(path)))
            }
            Self::Udp { socket } => {
                let (len, addr) = socket.recv_from(buf).await?;
                Ok((len, PeerAddr::Ip(addr)))
            }
            #[cfg(windows)]
            Self::NamedPipe { .. } => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "recv_from is not used on the named-pipe transport (the per-connection \
                 task calls read directly on the NamedPipeServer)",
            )),
        }
    }
}

/// IPC server that handles CLI requests
pub struct IpcServer {
    socket: ServerSocket,
    app_state: AppState,
}

impl IpcServer {
    /// Create and bind the IPC server
    ///
    /// Tries the platform-preferred transport first (Unix socket on Unix,
    /// named pipe on Windows per ADR-038), then falls back to UDP as the
    /// universal last-resort. Enforces auth requirements for *public*
    /// binds — UDP only (ADR-034); the Unix socket relies on filesystem
    /// mode bits and the named pipe relies on its DACL for trust.
    ///
    /// # Errors
    /// Returns error if all transports fail to bind, or if a UDP bind to
    /// a non-loopback address is attempted without remote auth configured.
    pub(crate) async fn new(app_state: AppState) -> anyhow::Result<Self> {
        // 1. Try Unix socket on Unix platforms
        #[cfg(unix)]
        {
            let run_dir = ensure_run_dir()?;
            let sock_path = run_dir.join("daemon.sock");

            // Remove stale socket file
            let _ = std::fs::remove_file(&sock_path);

            match UnixDatagram::bind(&sock_path) {
                Ok(socket) => {
                    // macOS's default `SO_SNDBUF` for `AF_UNIX/SOCK_DGRAM`
                    // is 2048 bytes, which is *smaller* than the
                    // `provider_list` response once the catalog has more
                    // than a handful of providers (each `ProviderInfo`
                    // entry serialises to ~150 B). `send_to` then
                    // returns `EMSGSIZE` ("Message too long", os error 40)
                    // and the handler logs an error while the client
                    // silently hangs waiting for a reply it never gets.
                    // Bump the socket buffer so any response shape we
                    // ship today — and the larger ones a future handler
                    // is bound to produce — fits atomically.
                    if let Err(e) = bump_send_buffer(&socket) {
                        warn!(
                            "Failed to set Unix datagram SO_SNDBUF to {} bytes ({}); \
                             responses larger than the platform default may fail with EMSGSIZE",
                            IPC_SEND_BUFFER_BYTES, e
                        );
                    }
                    info!("IPC server bound to Unix socket: {}", sock_path.display());
                    return Ok(Self {
                        socket: ServerSocket::Unix {
                            socket: Arc::new(socket),
                            path: Arc::new(sock_path),
                        },
                        app_state,
                    });
                }
                Err(e) => {
                    warn!("Failed to bind Unix socket ({}), falling back to UDP", e);
                }
            }
        }

        // 2. Try Windows named pipe on Windows platforms (ADR-038)
        #[cfg(windows)]
        {
            let pipe_name = std::env::var(DAEMON_PIPE_ENV).unwrap_or_else(|_| default_pipe_name());
            match Self::bind_named_pipe(&pipe_name) {
                Ok((listener, server_options)) => {
                    info!("IPC server bound to Windows named pipe: {}", pipe_name);
                    return Ok(Self {
                        socket: ServerSocket::NamedPipe {
                            listener: Arc::new(listener),
                            server_options: Arc::new(server_options),
                        },
                        app_state,
                    });
                }
                Err(e) => {
                    warn!(
                        "Failed to bind Windows named pipe ({}), falling back to UDP",
                        e
                    );
                }
            }
        }

        // 3. Fall back to UDP — the universal last-resort safety net
        let addr_str = format!("{}:{}", DEFAULT_HOST, DEFAULT_PORT);
        let socket = UdpSocket::bind(&addr_str)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind UDP socket to {}: {}", addr_str, e))?;

        // Same rationale as the Unix datagram branch above: bump the
        // send buffer so handler responses that exceed the platform
        // default (8 KiB on Linux for UDP, 9 KiB on macOS) still fit
        // atomically. `provider_list` is the worst offender today.
        if let Err(e) = bump_send_buffer(&socket) {
            warn!(
                "Failed to set UDP SO_SNDBUF to {} bytes ({}); responses larger \
                 than the platform default may fail with EMSGSIZE",
                IPC_SEND_BUFFER_BYTES, e
            );
        }

        let bound_addr = socket.local_addr()?;

        // ADR-034: Enforce auth for public binds. Only fires for the
        // UDP transport — Unix sockets and named pipes have their own
        // transport-layer trust boundaries.
        #[cfg(not(windows))]
        {
            let auth_config = app_state.auth_config();
            enforce_auth_for_public_bind(&bound_addr, &auth_config)?;
        }

        info!("IPC server bound to UDP: {}", addr_str);
        Ok(Self {
            socket: ServerSocket::Udp {
                socket: Arc::new(socket),
            },
            app_state,
        })
    }

    /// Bind a Windows named pipe with the ADR-038 DACL.
    ///
    /// Returns the first `NamedPipeServer` (the server end that calls
    /// `connect()` to wait for a client) and the cloned `ServerOptions`
    /// to use for subsequent `create()` calls in the accept loop.
    ///
    /// Tokio 1.49's `ServerOptions` does not expose a
    /// `security_attributes` setter, so we drop down to raw FFI for the
    /// first instance: call `CreateNamedPipeW` directly with our
    /// `SECURITY_ATTRIBUTES`, then wrap the resulting `HANDLE` in
    /// Tokio's `NamedPipeServer::from_raw_handle`. The kernel keeps the
    /// pipe name bound and applies our DACL to every subsequent client
    /// connection — Tokio's high-level `ServerOptions::create` for the
    /// follow-up instances doesn't need its own DACL because the
    /// already-bound pipe carries the original one.
    #[cfg(windows)]
    fn bind_named_pipe(
        name: &str,
    ) -> anyhow::Result<(
        tokio::net::windows::named_pipe::NamedPipeServer,
        tokio::net::windows::named_pipe::ServerOptions,
    )> {
        use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Pipes::CreateNamedPipeW;

        // PIPE_ACCESS_DUPLEX (0x03)              — read & write
        // FILE_FLAG_FIRST_PIPE_INSTANCE (0x00080000)
        // FILE_FLAG_OVERLAPPED (0x40000000)      — required for async I/O
        let open_mode: u32 = 0x03 | 0x00080000 | 0x40000000;
        // PIPE_TYPE_MESSAGE (0x04) | PIPE_READMODE_MESSAGE (0x02) | PIPE_WAIT (0x00)
        let pipe_mode: u32 = 0x04 | 0x02;
        let max_instances: u32 = 64;
        let out_buffer: u32 = 65536;
        let in_buffer: u32 = 65536;
        let default_timeout: u32 = 0; // 50 ms default per Win32 docs

        let attrs = super::pipe_security::current_user_only()?;
        let sa = super::pipe_security::as_attributes(&attrs);

        // Convert name to UTF-16 NUL-terminated.
        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: the name is a NUL-terminated UTF-16 string, all
        // scalar parameters are correct, and `sa` is a valid
        // SECURITY_ATTRIBUTES for the duration of the call.
        // `CreateNamedPipeW` returns INVALID_HANDLE_VALUE on failure.
        let handle: HANDLE = unsafe {
            CreateNamedPipeW(
                name_w.as_ptr(),
                open_mode,
                pipe_mode,
                max_instances,
                out_buffer,
                in_buffer,
                default_timeout,
                &sa,
            )
        };
        if handle == 0 || handle == -1isize as HANDLE {
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            anyhow::bail!("CreateNamedPipeW({name}) failed: {err}");
        }

        // Wrap the raw HANDLE in Tokio's NamedPipeServer. `from_raw_handle`
        // is unsafe because the caller is asserting sole ownership of the
        // HANDLE returned by `CreateNamedPipeW`. Tokio will close the
        // handle when the `NamedPipeServer` is dropped.
        let listener = unsafe {
            tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(handle as _)
        }
        .map_err(|e| anyhow::anyhow!("NamedPipeServer::from_raw_handle: {e}"))?;

        // Subsequent instances use the high-level API — the kernel
        // reuses the DACL from the first bind, so we don't need to pass
        // SECURITY_ATTRIBUTES again.
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(false) // we are NOT the first instance on subsequent calls
            .max_instances(64)
            .pipe_mode(PipeMode::Message)
            .in_buffer_size(65536)
            .out_buffer_size(65536);
        Ok((listener, opts))
    }

    /// Run the IPC server loop
    ///
    /// Dispatches to the per-transport loop. Unix datagram and UDP share
    /// `run_datagram` (one bound socket, many peers, `recv_from` per
    /// request); Windows named pipes use `run_pipes` (one accept loop,
    /// one `NamedPipeServer` per connection, `write_all` per response).
    pub async fn run(
        &self,
        shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> anyhow::Result<()> {
        match &self.socket {
            #[cfg(unix)]
            ServerSocket::Unix { .. } => self.run_datagram(shutdown_rx).await,
            ServerSocket::Udp { .. } => self.run_datagram(shutdown_rx).await,
            #[cfg(windows)]
            ServerSocket::NamedPipe {
                listener,
                server_options,
            } => {
                let listener = Arc::clone(listener);
                let opts = Arc::clone(server_options);
                let pipe_name = std::env::var(super::DAEMON_PIPE_ENV)
                    .unwrap_or_else(|_| super::default_pipe_name());
                self.run_pipes(listener, opts, pipe_name, shutdown_rx).await
            }
        }
    }

    /// Existing Unix/UDP loop: one bound socket, many peers, `recv_from`
    /// per request, `send_to` per response. Unchanged behaviour from
    /// pre-ADR-038.
    async fn run_datagram(
        &self,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> anyhow::Result<()> {
        let mut buf = vec![0u8; 65536];

        info!("IPC server ready, waiting for requests...");

        loop {
            tokio::select! {
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, addr)) => {
                            if len == 0 {
                                continue;
                            }

                            match AuthenticatedRequest::from_bytes(&buf[..len]) {
                                Ok(envelope) => {
                                    trace!("Received request: {:?}", envelope.packet);
                                    let request_id = envelope.request_id();

                                    // Resolve caller identity
                                    let caller = match Self::resolve_caller(&envelope, &addr, &self.app_state).await {
                                        Ok(caller) => caller,
                                        Err(auth_err) => {
                                            warn!("Auth failed for request {}: {}", request_id, auth_err);
                                            let response = ResponsePacket::Error {
                                                request_id,
                                                message: format!("Authentication failed: {}", auth_err),
                                            };
                                            if let Ok(bytes) = response.to_bytes() {
                                                let sink = sink_for_unix_or_udp(&self.socket, &addr);
                                                if let Ok(sink) = sink {
                                                    let _ = sink.send_bytes(&bytes).await;
                                                }
                                            }
                                            continue;
                                        }
                                    };

                                    // Check rate limit
                                    if let Some(rate_limiter) = self.app_state.rate_limiter() {
                                        let is_jwt = matches!(envelope.auth.credential, super::packet::AuthCredential::Jwt(_));
                                        if !rate_limiter.check(&caller.rate_limit_bucket, is_jwt).await {
                                            warn!("Rate limit exceeded for {}", caller.rate_limit_bucket);
                                            let response = ResponsePacket::Error {
                                                request_id,
                                                message: "Rate limit exceeded. Try again later.".to_string(),
                                            };
                                            if let Ok(bytes) = response.to_bytes() {
                                                let sink = sink_for_unix_or_udp(&self.socket, &addr);
                                                if let Ok(sink) = sink {
                                                    let _ = sink.send_bytes(&bytes).await;
                                                }
                                            }
                                            continue;
                                        }
                                    }

                                    // Spawn a task to handle the request
                                    let state = self.app_state.clone();
                                    let socket = self.socket.clone();
                                    let peer = addr.clone();
                                    tokio::spawn(async move {
                                        let sink = match sink_for_unix_or_udp(&socket, &peer) {
                                            Ok(s) => s,
                                            Err(e) => {
                                                error!("Failed to build response sink: {e}");
                                                return;
                                            }
                                        };
                                        #[allow(clippy::large_futures)]
                                        let request_fut = Self::handle_request(
                                            envelope.packet,
                                            caller,
                                            state,
                                            &*sink,
                                            &peer,
                                        );
                                        if let Err(e) = Box::pin(request_fut).await {
                                            error!("Error handling request {}: {}", request_id, e);
                                        }
                                    });
                                }
                                Err(e) => {
                                    warn!("Failed to parse request packet: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Socket receive error: {}", e);
                            // Brief pause to avoid tight error loop
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("IPC server received shutdown signal, stopping...");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Windows named-pipe accept loop (ADR-038).
    ///
    /// Tokio 1.49's `NamedPipeServer` exposes `connect()` (an async
    /// `ConnectNamedPipe` wrapper) instead of a separate listener type.
    /// We keep one server end on the side, call `connect()` on it to
    /// wait for a client, hand the connected end to a per-connection
    /// task, and create the next server end via the stored
    /// `ServerOptions` to keep the pipe name bound across accepts.
    #[cfg(windows)]
    async fn run_pipes(
        &self,
        listener: Arc<tokio::net::windows::named_pipe::NamedPipeServer>,
        opts: Arc<tokio::net::windows::named_pipe::ServerOptions>,
        pipe_name: String,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> anyhow::Result<()> {
        info!("IPC server ready (Windows named pipe), waiting for connections...");

        // `listener` is the first instance, currently not yet connected.
        let mut current = listener;

        loop {
            tokio::select! {
                biased;
                connect = current.connect() => {
                    if let Err(e) = connect {
                        error!("Named pipe connect error: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    // Create the next instance BEFORE handing the current
                    // one off to a task — this keeps the pipe name bound
                    // and ready for the next client.
                    let server = Arc::clone(&current);
                    match opts.create(&pipe_name) {
                        Ok(next) => {
                            current = Arc::new(next);
                        }
                        Err(e) => {
                            error!("Failed to create next named-pipe instance: {e}");
                            // Keep `current` as the connected server; the
                            // next iteration will reuse it after the
                            // connection closes. The shutdown signal
                            // will eventually tear down the accept loop.
                        }
                    }
                    let state = self.app_state.clone();
                    let server = match Arc::try_unwrap(server) {
                        Ok(s) => s,
                        Err(arc) => {
                            // Should never happen — we just cloned above
                            // and never shared this Arc. Fall back to a
                            // fresh create to keep the pipe responsive.
                            error!("Unexpected: current Arc had >1 strong ref");
                            match opts.create(&pipe_name) {
                                Ok(s) => s,
                                Err(e) => {
                                    error!("Recovery create failed: {e}");
                                    continue;
                                }
                            }
                        }
                    };
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_pipe_connection(server, state).await {
                            error!("Pipe connection error: {e}");
                        }
                    });
                }
                _ = shutdown_rx.recv() => {
                    info!("IPC server (named pipe) received shutdown signal, stopping...");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle a single Windows named-pipe connection (ADR-038).
    ///
    /// Reads one message-mode packet (≤ 64KB per `MAX_PACKET_SIZE`),
    /// dispatches to `handle_request` with a `PipeSink` over the
    /// connection, then writes the response stream back. The connection
    /// is closed when this function returns (the `NamedPipeServer` is
    /// dropped), which matches the per-request model of the Unix/UDP
    /// path.
    #[cfg(windows)]
    async fn handle_pipe_connection(
        mut server: tokio::net::windows::named_pipe::NamedPipeServer,
        state: AppState,
    ) -> anyhow::Result<()> {
        use tokio::io::AsyncReadExt;

        let mut buf = vec![0u8; 65536];

        // Read one full request. With `PipeMode::Message` (set in
        // `bind_named_pipe`), `read` returns one full message at a time,
        // capped at 64KB. We read a single request and respond; if the
        // client wants another request on the same connection, that's a
        // future enhancement — today the protocol is one-request-per-
        // connection to match the existing Unix/UDP semantics.
        let len = match server.read(&mut buf).await {
            Ok(0) => return Ok(()), // client closed cleanly without sending
            Ok(n) => n,
            Err(e) => {
                warn!("Named pipe read error: {e}");
                return Ok(());
            }
        };

        let envelope = match AuthenticatedRequest::from_bytes(&buf[..len]) {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to parse request packet on named pipe: {e}");
                return Ok(());
            }
        };
        let request_id = envelope.request_id();
        trace!("Received request on named pipe: {:?}", envelope.packet);

        // Named-pipe connections are local by construction (the kernel
        // checked the client SID at CreateFileW time). Use the unit
        // `PeerAddr::Local` for the auth dispatch.
        let peer = PeerAddr::Local;
        let caller = match Self::resolve_caller(&envelope, &peer, &state).await {
            Ok(caller) => caller,
            Err(auth_err) => {
                warn!("Auth failed for request {}: {}", request_id, auth_err);
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!("Authentication failed: {}", auth_err),
                };
                if let Ok(bytes) = response.to_bytes() {
                    let sink = sink_for_pipe(&mut server);
                    let _ = sink.send_bytes(&bytes).await;
                }
                return Ok(());
            }
        };

        // Rate limit
        if let Some(rate_limiter) = state.rate_limiter() {
            let is_jwt = matches!(
                envelope.auth.credential,
                super::packet::AuthCredential::Jwt(_)
            );
            if !rate_limiter.check(&caller.rate_limit_bucket, is_jwt).await {
                warn!("Rate limit exceeded for {}", caller.rate_limit_bucket);
                let response = ResponsePacket::Error {
                    request_id,
                    message: "Rate limit exceeded. Try again later.".to_string(),
                };
                if let Ok(bytes) = response.to_bytes() {
                    let sink = sink_for_pipe(&mut server);
                    let _ = sink.send_bytes(&bytes).await;
                }
                return Ok(());
            }
        }

        // Dispatch. The sink borrows `server` mutably for the lifetime
        // of the call; once `handle_request` returns, the connection is
        // dropped and the kernel cleans up the per-connection handle.
        let sink = sink_for_pipe(&mut server);
        if let Err(e) = Self::handle_request(envelope.packet, caller, state, &*sink, &peer).await {
            error!("Error handling request {}: {}", request_id, e);
        }
        Ok(())
    }

    /// Resolve the caller identity from an authenticated request.
    async fn resolve_caller(
        envelope: &AuthenticatedRequest,
        peer: &PeerAddr,
        state: &AppState,
    ) -> Result<CallerContext, AuthError> {
        use super::packet::AuthCredential;

        let is_local_connection = peer.is_local();
        let auth_config = state.auth_config();

        match &envelope.auth.credential {
            AuthCredential::None => {
                // Local trust: only allowed for localhost/Unix socket
                if !is_local_connection && !auth_config.enable_local_trust() {
                    return Err(AuthError::LocalTrustDisabled);
                }
                // For non-local connections without credentials, reject
                if !is_local_connection {
                    return Err(AuthError::InvalidCredential);
                }
                Ok(CallerContext::local())
            }
            AuthCredential::Jwt(token) => {
                if !auth_config.enable_pekohub_jwt() {
                    return Err(AuthError::InvalidCredential);
                }
                if let Some(validator) = state.jwt_validator() {
                    match validator.validate(token).await {
                        Ok(validated) => Ok(crate::auth::jwt::JwtValidator::to_caller(validated)),
                        Err(e) => {
                            tracing::warn!("JWT validation failed: {}", e);
                            Err(AuthError::InvalidCredential)
                        }
                    }
                } else {
                    Err(AuthError::InvalidCredential)
                }
            }
            AuthCredential::ApiKey(key) => {
                if !auth_config.enable_api_key() {
                    return Err(AuthError::InvalidCredential);
                }
                if let Some(verifier) = state.api_key_verifier() {
                    match verifier.verify(key).await {
                        Some(entry) => {
                            let key_id = crate::auth::api_key::ApiKeyStore::extract_key_id(key);
                            Ok(CallerContext::from_api_key(key_id, entry.scopes))
                        }
                        None => Err(AuthError::InvalidCredential),
                    }
                } else {
                    Err(AuthError::InvalidCredential)
                }
            }
        }
    }

    /// Handle a single request.
    ///
    /// The legacy per-domain `match` is retired (F6 sweep): every
    /// `RequestPacket` variant is now owned by exactly one
    /// [`crate::ipc::handlers::RequestHandler`], and dispatch is the
    /// single-purpose [`crate::ipc::handlers::RequestDispatcher::dispatch`]
    /// call below. The sink abstraction (ADR-038) stays —
    /// `UnixDatagramSink` / `UdpSink` / `PipeSink` per platform.
    #[allow(clippy::large_futures)]
    async fn handle_request(
        request: RequestPacket,
        caller: CallerContext,
        state: AppState,
        sink: &dyn ResponseSink,
        peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        crate::ipc::handlers::RequestDispatcher::dispatch(state, request, &caller, sink, peer).await
    }

    // (handle_async_spawn / handle_async_cancel retired to
    // `ipc::handlers::tool::ToolHandler` under F6 step 3. The async
    // tool execution path now lives behind a narrow `ToolHost` port
    // and resolves capability grants server-side from the session's
    // owning Principal — see F8 / ADR-042.)

    // (handle_session_steer / handle_session_steer_list /
    // handle_session_steer_cancel retired under ADR-042 along with
    // their IPC variants. The internal `inbox_registry`,
    // `SteeringMessage`, and `run_session_on_inbox` plumbing remains
    // in use — the executor drains async completions locally — but
    // there is no longer any IPC entrypoint that pushes a steering
    // message onto a peer-keyed session from outside the daemon.
    // See ADR-042.)
}

#[cfg(test)]
mod buffer_tests {
    //! Regression tests for the IPC `SO_SNDBUF` bump.
    //!
    //! Symptom we're guarding against (peko-desktop#44, daemon-side
    //! EMSGSIZE): on macOS the default `SO_SNDBUF` for `AF_UNIX/SOCK_DGRAM`
    //! is 2048 bytes, and the `provider_list` response (15+ providers,
    //! each serialising to ~150 B) is larger than that. `send_to` then
    //! returns `EMSGSIZE` ("Message too long", os error 40), the handler
    //! logs the error, and the client silently hangs. The fix bumps
    //! `SO_SNDBUF` to `IPC_SEND_BUFFER_BYTES` on every server bind, and
    //! these tests assert both that the helper succeeds and that a
    //! large round-tripped payload actually fits.

    use super::bump_recv_buffer;
    use super::bump_send_buffer;
    use crate::ipc::packet::{RequestPacket, ResponsePacket};
    use std::os::fd::AsRawFd;
    use tokio::net::UnixDatagram;

    #[cfg(unix)]
    #[tokio::test]
    async fn bump_send_buffer_raises_unix_datagram_sndbuf() {
        // The kernel clamps `SO_SNDBUF` to its configured maximum, so
        // we can only assert *at least* `IPC_SEND_BUFFER_BYTES` is in
        // effect — not that the exact value was honoured verbatim.
        const MIN_EXPECTED: usize = 8 * 1024;

        let tmp =
            std::env::temp_dir().join(format!("peko_ipc_buf_test_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let sock = UnixDatagram::bind(&tmp).expect("bind unix datagram");
        bump_send_buffer(&sock).expect("bump_send_buffer");

        let actual = read_so_sndbuf(sock.as_raw_fd());
        let _ = std::fs::remove_file(&tmp);

        assert!(
            actual >= MIN_EXPECTED,
            "expected SO_SNDBUF >= {MIN_EXPECTED}, got {actual}"
        );
    }

    /// Round-trip a `provider_list`-shaped response large enough that
    /// the *default* macOS `SO_SNDBUF` (2048 B) would reject it with
    /// `EMSGSIZE`, and verify the bumped server can deliver it back
    /// without truncation. This mirrors the exact failure mode the
    /// desktop hit.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_datagram_round_trips_response_larger_than_default_sndbuf() {
        // 15 entries × ~200 B ≈ 3 KiB — comfortably over the macOS
        // 2048 B default, comfortably under `IPC_SEND_BUFFER_BYTES`.
        let providers: Vec<crate::ipc::packet::ProviderInfo> = (0..15)
            .map(|i| crate::ipc::packet::ProviderInfo {
                id: format!("test-provider-{i:02}-with-a-longer-id"),
                display_name: format!("Test Provider {i:02}"),
                api_type: "openai".into(),
                base_url: format!("https://api.test-provider-{i:02}.com/v1"),
                requires_key: true,
                is_local: false,
                enabled: true,
                models: vec![],
                default_model_id: "gpt-5".into(),
                headers: Default::default(),
                is_default: false,
            })
            .collect();
        let response = ResponsePacket::ProviderList {
            request_id: 1,
            providers,
        };

        let server_path = std::env::temp_dir().join(format!(
            "peko_ipc_roundtrip_server_{}.sock",
            std::process::id()
        ));
        let client_path = std::env::temp_dir().join(format!(
            "peko_ipc_roundtrip_client_{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&server_path);
        let _ = std::fs::remove_file(&client_path);

        let server = UnixDatagram::bind(&server_path).expect("server bind");
        bump_send_buffer(&server).expect("bump server buffer");
        let client = UnixDatagram::bind(&client_path).expect("client bind");
        bump_recv_buffer(&client).expect("bump client buffer");

        let bytes = serde_json::to_vec(&response).expect("serialize response");
        assert!(
            bytes.len() > 2048,
            "fixture must exceed the macOS default SO_SNDBUF (got {} B)",
            bytes.len()
        );

        // Server → client (this is the path that fails with the un-bumped
        // socket).
        client.send_to(b"hello", &server_path).await.unwrap();
        let (_req_len, server_peer) = server.recv_from(&mut [0u8; 64]).await.unwrap();
        let server_peer_path = server_peer
            .as_pathname()
            .expect("client peer must have a filesystem path");
        server
            .send_to(&bytes, server_peer_path)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "send_to({} B response) failed: {e}. \
                     This is the EMSGSIZE bug we're guarding against.",
                    bytes.len()
                )
            });

        let mut buf = vec![0u8; bytes.len() + 1024];
        let (len, _) = client
            .recv_from(&mut buf)
            .await
            .expect("client should receive the bumped response");
        buf.truncate(len);
        assert_eq!(buf, bytes, "round-tripped payload must match");

        let _ = std::fs::remove_file(&server_path);
        let _ = std::fs::remove_file(&client_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bump_send_buffer_is_idempotent() {
        let tmp =
            std::env::temp_dir().join(format!("peko_ipc_buf_idem_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let sock = UnixDatagram::bind(&tmp).expect("bind unix datagram");
        bump_send_buffer(&sock).expect("first bump");
        bump_send_buffer(&sock).expect("second bump");
        let _ = std::fs::remove_file(&tmp);
    }

    /// Read the *current* `SO_SNDBUF` for `fd`. Lives here rather than
    /// in the helper so the test asserts an observable side effect
    /// rather than trusting that `setsockopt` returned 0.
    #[cfg(unix)]
    fn read_so_sndbuf(fd: std::os::fd::RawFd) -> usize {
        let mut value: libc::c_int = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `fd` is live, `value` is a writable `c_int`, `len`
        // is set to the correct size. `getsockopt` does not retain
        // any pointer past the call.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &mut value as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        assert_eq!(
            rc,
            0,
            "getsockopt failed: {}",
            std::io::Error::last_os_error()
        );
        value as usize
    }

    /// Silences the unused-import lint when running only the bump
    /// test (the `RequestPacket` is wired in for a future integration
    /// test that drives a real handler through the server loop).
    #[allow(dead_code)]
    fn _ensure_request_packet_in_scope() -> RequestPacket {
        RequestPacket::Ping { request_id: 1 }
    }
}
