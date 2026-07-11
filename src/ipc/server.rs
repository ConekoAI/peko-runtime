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

