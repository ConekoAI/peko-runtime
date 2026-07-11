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

use anyhow::Context;
use tokio::net::UdpSocket;
#[cfg(unix)]
use tokio::net::UnixDatagram;
use tracing::{error, info, trace, warn};

use super::packet::{
    AuthenticatedRequest, PrincipalSendControlMode, RequestPacket, ResponsePacket,
};
use super::response_sink::{sink_for_unix_or_udp, ResponseSink};
use super::send_response::send_response;
#[cfg(windows)]
use super::{default_pipe_name, response_sink::sink_for_pipe, DAEMON_PIPE_ENV};
use super::{ensure_run_dir, DEFAULT_HOST, DEFAULT_PORT};
use crate::auth::caller::CallerContext;
use crate::ipc::handlers::auth::AuthHandler;
use crate::ipc::handlers::capability::CapabilityHandler;
use crate::ipc::handlers::cron::CronHandler;
use crate::ipc::handlers::ext_runtime::ExtRuntimeHandler;
use crate::ipc::handlers::extension::ExtensionHandler;
use crate::ipc::handlers::instance::InstanceHandler;
use crate::ipc::handlers::runtime::RuntimeHandler;
use crate::ipc::handlers::system::SystemHandler;
use crate::ipc::handlers::tool::ToolHandler;
use crate::ipc::handlers::tunnel::TunnelHandler;
use crate::ipc::handlers::RequestHandler;
#[cfg(not(windows))]
use crate::auth::config::enforce_auth_for_public_bind;
use crate::auth::ownership::{check_permission, Permission, Resource};
use crate::auth::permissions::AuthError;
use crate::auth::Subject;
use crate::common::services::session_event_to_history;
use crate::common::services::session_service::HistoryEvent;
use crate::daemon::state::{AppState, StreamingRunHandle};
use crate::principal::{
    router::{ChannelContext, ChannelKind},
    Principal, RouteDecision, RouterError,
};
use crate::session::events::SessionEvent;

// ─── peko log read-path types (ADR-042) ──────────────────────────────

/// Preview summary for a `.principal` package, produced server-side
/// before the destructive import step.
#[derive(Debug)]
pub struct PrincipalImportPreview {
    name: String,
    version: String,
    did: String,
    description: Option<String>,
    agents: Vec<String>,
    extensions: Vec<String>,
    required_capabilities: Vec<String>,
    signed: bool,
    validation_errors: Vec<String>,
    validation_warnings: Vec<String>,
}

/// Errors surfaced by `IpcServer::read_principal_log`. The match arm in
/// `handle_request` maps each variant into a `ResponsePacket::Error`
/// with a stable error-code prefix so the CLI can render a useful
/// message without parsing the human-readable body.
enum PrincipalLogError {
    NotFound(String),
    Forbidden(String),
    Internal(String),
}

/// Successful read shape consumed by the `PrincipalLog` response.
struct PrincipalLogResponse {
    name: String,
    peer: Subject,
    session_id: Option<String>,
    events: Vec<HistoryEvent>,
    truncated: bool,
}

/// RAII guard that removes a `PrincipalSendStream` run from the
/// `streaming_runs` registry on drop. The streaming handler holds one
/// of these for the lifetime of the run so registry cleanup happens on
/// every return path — natural completion, sink-write error, panic —
/// without needing a removal call at every `?`/`return` site.
struct StreamingRunGuard {
    registry: Arc<std::sync::Mutex<std::collections::HashMap<u64, StreamingRunHandle>>>,
    request_id: u64,
}

impl Drop for StreamingRunGuard {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.registry.lock() {
            runs.remove(&self.request_id);
        }
    }
}

/// Selects between the two IPC variants of `PrincipalSend`.
///
/// Both variants go through the same root-router streaming path
/// (`run_principal_send`) and the same `streaming_runs` registry, so
/// the only difference at the wire level is the success-packet shape:
///
/// - `OneShot` emits `PrincipalSent { content }` then `Done`. Used by
///   the `RequestPacket::PrincipalSend` handler (peko-desktop's
///   `usePrincipalSend` with no `onChunk`).
/// - `Streaming` emits zero-or-more `PrincipalSentChunk { delta }`
///   packets followed by `PrincipalSentDone { content }` and `Done`.
///   Used by the `RequestPacket::PrincipalSendStream` handler.
///
/// Both variants are interrupt-capable: the cancel token is registered
/// in `streaming_runs` regardless of which variant the caller chose,
/// so `peko interrupt <id>` works uniformly.
#[derive(Copy, Clone)]
enum PrincipalSendResponseKind {
    OneShot,
    Streaming,
}

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
    pub async fn new(app_state: AppState) -> anyhow::Result<Self> {
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

    /// Handle a single request
    ///
    /// The `sink: &dyn ResponseSink` abstraction (ADR-038) lets the giant
    /// `match` body below be platform-agnostic: Unix/UDP call sites
    /// construct a per-request `UnixDatagramSink` or `UdpSink` that
    /// captures the peer address from `recv_from`; Windows call sites
    /// construct a `PipeSink` over the per-connection `&mut
    /// NamedPipeServer`. See `response_sink.rs` for the full design.
    #[allow(clippy::large_futures)]
    async fn handle_request(
        request: RequestPacket,
        caller: CallerContext,
        state: AppState,
        sink: &dyn ResponseSink,
        peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        // For v0.1.0, local trust is treated as owner (all actions allowed).
        // JWT users have full access.
        // API key scopes are checked at resolution time.
        // Future: enforce per-resource ACLs here.

        // Pre-resolve the `subject` for grant/revoke variants (issue
        // #25). The match below consumes `request` so we can't call
        // `&request.resolved_subject()` from inside the arms — compute
        // it here while `request` is still accessible. The borrow
        // released by the time the match starts (NLL).
        let pre_resolved_subject: Option<crate::auth::Subject> = match &request {
            RequestPacket::PrincipalGrantPermission { .. }
            | RequestPacket::PrincipalRevokePermission { .. } => Some(request.resolved_subject()),
            _ => None,
        };

        /// Take the pre-resolved subject for a grant/revoke arm.
        /// Sends a `ResponsePacket::Error` and returns `Err(())` on
        /// resolution failure (caller should `return Ok(())`); returns
        /// `Ok(principal)` on success. Defined inside `handle_request`
        /// to avoid threading `sink` through a free-function signature.
        async fn take_resolved_subject(
            pre_resolved: Option<&crate::auth::Subject>,
            _request_id: u64,
            _sink: &dyn crate::ipc::response_sink::ResponseSink,
        ) -> Result<crate::auth::Subject, ()> {
            let Some(p) = pre_resolved else {
                unreachable!("take_resolved_subject called for a non-grant/revoke variant")
            };
            Ok(p.clone())
        }

        // F6: try per-domain request handlers first. Handlers own their
        // own dependencies (captured at construction), so the dispatcher
        // doesn't need to know the concrete state type. New domains are
        // added by appending to `domain_handlers`; the legacy match
        // below remains the fallback for variants not yet migrated.
        let domain_handlers: Vec<Arc<dyn RequestHandler>> = vec![
            Arc::new(SystemHandler::new(Arc::new(state.clone()))),
            Arc::new(AuthHandler::new(Arc::new(state.clone()))),
            Arc::new(ToolHandler::new(Arc::new(state.clone()))),
            Arc::new(TunnelHandler::new(Arc::new(state.clone()))),
            Arc::new(CapabilityHandler::new(Arc::new(state.clone()))),
            Arc::new(InstanceHandler::new(Arc::new(state.clone()))),
            Arc::new(ExtRuntimeHandler::new(Arc::new(state.clone()))),
            Arc::new(CronHandler::new(Arc::new(state.clone()))),
            Arc::new(RuntimeHandler::new(Arc::new(state.clone()))),
            Arc::new(ExtensionHandler::new(Arc::new(state.clone()))),
        ];
        for handler in &domain_handlers {
            if handler.matches(&request) {
                trace!("dispatching to {} handler", handler.domain());
                return handler.handle(request, &caller, sink, peer).await;
            }
        }

        // Defense-in-depth: capture the request id so the wildcard
        // fallback can echo it. Unreachable in normal operation — the
        // handler loop above dispatches every migrated variant. If a
        // new `RequestPacket` variant is added without either a handler
        // or a legacy arm, this surfaces a clear protocol error to the
        // client instead of dropping the packet silently.
        let request_id = request.request_id();

        match request {
            // `RequestPacket::Execute` was retired in audit C4. All
            // chat traffic now flows through `PrincipalSend` (one-shot)
            // and `PrincipalSendStream` (streaming) below — both go
            // through `PrincipalManager::receive` and produce
            // principal-scoped sessions and audit trails.
            //
            // `AsyncSpawn` / `AsyncCancel` (tool execution) are owned
            // by `ipc::handlers::tool::ToolHandler` (F6 step 3 + F8
            // grant threading) — dispatched in the `domain_handlers`
            // loop above.

            RequestPacket::PrincipalSendControl {
                request_id,
                target_request_id,
                mode,
            } => {
                Self::handle_principal_send_control(
                    request_id,
                    target_request_id,
                    mode,
                    state,
                    sink,
                )
                .await?;
            }

            // `CronList` / `CronAdd` / `CronRemove` / `CronRun` / `CronHistory`
            // are owned by `ipc::handlers::cron::CronHandler` (F6.7) —
            // dispatched in the `domain_handlers` loop above.

            // ─── Extension Runtime Lifecycle (ADR-026) ───────────────────────
            // `ExtStart` / `ExtStop` / `ExtRestart` / `ExtStatus` are owned by
            // `ipc::handlers::ext_runtime::ExtRuntimeHandler` (F6.11) —
            // dispatched in the `domain_handlers` loop above.

            // ─── Principal CRUD (post-migration actor surface) ────────────
            // The principal-as-single-actor migration (audit C1) replaced
            // the legacy `AgentList` IPC handler. Actor listing/show is
            // now served by `PrincipalManager::list_all` / `get_by_name`,
            // which read the post-migration `<workspace>/principals/...`
            // tree — the on-disk truth — instead of the legacy per-agent
            // mirror directories.
            RequestPacket::PrincipalList { request_id } => {
                let principal_manager = state.principal_manager();
                let mut principals = Vec::new();
                for p in principal_manager.list_all().await {
                    principals.push(p.summary().await);
                }
                let response = ResponsePacket::PrincipalList {
                    request_id,
                    principals,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalGet { request_id, name } => {
                let principal_manager = state.principal_manager();
                let principal = match principal_manager.get_by_name(&name).await {
                    Some(p) => Some(p.summary().await),
                    None => None,
                };
                let response = ResponsePacket::PrincipalGet {
                    request_id,
                    principal,
                };
                send_response(sink, response).await?;
            }

            // (Session CRUD: SessionList / SessionRemove retired under ADR-042.
            // The legacy `peko session` command tree that drove these is
            // gone; the only external session read surface is now
            // `RequestPacket::PrincipalLog` (see
            // `handle_principal_log`). See ADR-042 for the contract.)
            RequestPacket::ProviderList { request_id } => {
                let registry = crate::providers::ProviderRegistry::new();
                let mut providers: Vec<crate::ipc::packet::ProviderInfo> = Vec::new();
                let mut seen_ids = std::collections::HashSet::new();
                for (_id, meta) in registry.iter() {
                    if !seen_ids.insert(meta.id) {
                        continue;
                    }
                    providers.push(crate::ipc::packet::ProviderInfo {
                        id: meta.id.to_string(),
                        display_name: meta.display_name.to_string(),
                        api_type: match meta.api_type {
                            crate::providers::registry::ApiType::OpenAICompletions => {
                                "openai".to_string()
                            }
                            crate::providers::registry::ApiType::AnthropicMessages => {
                                "anthropic".to_string()
                            }
                        },
                        default_model: meta.default_model.to_string(),
                        requires_key: !meta.api_key_env.is_empty(),
                        is_local: meta.api_key_env.is_empty(),
                    });
                }
                let response = ResponsePacket::ProviderList {
                    request_id,
                    providers,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::ProviderReload { request_id } => match state.reload_providers().await {
                Ok((providers_count, keys_count)) => {
                    let response = ResponsePacket::ProviderReloaded {
                        request_id,
                        providers_count,
                        keys_count,
                    };
                    send_response(sink, response).await?;
                }
                Err(e) => {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("provider reload failed: {e}"),
                    };
                    send_response(sink, response).await?;
                }
            },

            RequestPacket::McpReload { request_id } => match state.reload_mcp_config().await {
                Ok(servers_count) => {
                    let response = ResponsePacket::McpReloaded {
                        request_id,
                        servers_count,
                    };
                    send_response(sink, response).await?;
                }
                Err(e) => {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("mcp reload failed: {e}"),
                    };
                    send_response(sink, response).await?;
                }
            },

            // ── Extension CRUD (ADR-030 Tier 1) ──
            // `ExtensionList` / `ExtensionInstall` / `ExtensionUninstall` /
            // `ExtensionValidate` / `ExtensionDebug` / `ExtensionInfo` /
            // `ExtensionExport` / `ExtensionBundle` are owned by
            // `ipc::handlers::extension::ExtensionHandler` (F6.6) —
            // dispatched in the `domain_handlers` loop above.

            // ── Runtime (ADR-032) ──
            // `RuntimeId` / `RuntimeInfo` / `RuntimeList` / `RuntimeRegister` /
            // `RuntimeTrust` / `RuntimeRemove` are owned by
            // `ipc::handlers::runtime::RuntimeHandler` (F6.9) —
            // dispatched in the `domain_handlers` loop above.

            // `TunnelStop` / `TunnelStatus` are owned by
            // `ipc::handlers::tunnel::TunnelHandler` (F6.4) — dispatched
            // in the `domain_handlers` loop above.

            // `InstanceSetStatus` / `InstanceSetExposure` are owned by
            // `ipc::handlers::instance::InstanceHandler` (F6.10) —
            // dispatched in the `domain_handlers` loop above.

            // ── Principal operations ─────────────────────────────────────────
            // Non-streaming `PrincipalSend` — peko-desktop's
            // `usePrincipalSend` (no `onChunk`) uses this variant.
            // Both this and the `PrincipalSendStream` variant are now
            // handled by the shared `run_principal_send` helper, which
            // routes the call through the streaming machinery, registers
            // a `CancellationToken` in `streaming_runs`, and picks the
            // wire-shape of the success packet based on
            // `PrincipalSendResponseKind`. Net effect: a soft-interrupt
            // issued via `peko interrupt <id>` works for both variants.
            RequestPacket::PrincipalSend {
                request_id,
                name,
                message,
                user,
                no_slash,
                output_format,
            } => {
                Self::run_principal_send(
                    request_id,
                    name,
                    message,
                    user,
                    no_slash,
                    output_format,
                    state,
                    sink,
                    PrincipalSendResponseKind::OneShot,
                )
                .await?;
            }

            // Streaming variant of `PrincipalSend`. The root agent
            // router's `route_streaming` emits `AgenticEvent`s; we
            // forward `AssistantDelta` (and the related streaming
            // events) as `PrincipalSentChunk` packets, and on completion
            // emit a single `PrincipalSentDone` carrying the full
            // final answer — identical to what `PrincipalSent` would
            // have returned — followed by the standard `Done`.
            //
            // The root agent runs in a `tokio::spawn`'d task that
            // pushes events into a bounded `mpsc::channel` and the
            // final `RouteDecision` into a `oneshot`. The handler
            // task drains the channel, writes each `PrincipalSentChunk`
            // to the sink, and finally awaits the oneshot for the
            // `PrincipalSentDone` payload. This keeps the callback
            // `Send + Sync + 'static` (it only holds an `mpsc::Sender`)
            // and avoids the `&dyn ResponseSink` lifetime problem.
            //
            // Both IPC variants of `PrincipalSend` go through
            // `run_principal_send` so the cancel-token registry,
            // build_router_context, and root-agent spawn are shared.
            RequestPacket::PrincipalSendStream {
                request_id,
                name,
                message,
                user,
                no_slash,
                output_format,
            } => {
                Self::run_principal_send(
                    request_id,
                    name,
                    message,
                    user,
                    no_slash,
                    output_format,
                    state,
                    sink,
                    PrincipalSendResponseKind::Streaming,
                )
                .await?;
            }

            // ─── peko log ────────────────────────────────────────────────
            // Read complement to `PrincipalSend`. There is deliberately no
            // `peko session` command (ADR-042): the CLI only ever sees a
            // peer's own thread, the owner sees their own by default, and
            // the principal's `Chat` grant plus a peer-privacy match
            // (`caller == target_peer || caller == owner`) gates access.
            RequestPacket::PrincipalLog {
                request_id,
                name,
                peer,
                limit,
                since_secs,
            } => {
                let caller_subject = caller.subject();
                let response = match Self::read_principal_log(
                    &state,
                    &name,
                    peer,
                    limit,
                    since_secs,
                    caller_subject,
                )
                .await
                {
                    Ok(resp) => ResponsePacket::PrincipalLog {
                        request_id,
                        name: resp.name,
                        peer: resp.peer,
                        session_id: resp.session_id,
                        events: resp.events,
                        truncated: resp.truncated,
                    },
                    Err(PrincipalLogError::NotFound(msg)) => ResponsePacket::Error {
                        request_id,
                        message: format!("[not_found] {msg}"),
                    },
                    Err(PrincipalLogError::Forbidden(msg)) => ResponsePacket::Error {
                        request_id,
                        message: format!("[forbidden] {msg}"),
                    },
                    Err(PrincipalLogError::Internal(msg)) => ResponsePacket::Error {
                        request_id,
                        message: format!("[internal_error] {msg}"),
                    },
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalExport {
                request_id,
                name,
                output,
                include_sessions,
                with_extensions,
            } => {
                match Self::export_principal_package(
                    &state,
                    &name,
                    output.clone(),
                    include_sessions,
                    with_extensions,
                )
                .await
                {
                    Ok(output_path) => {
                        let response = ResponsePacket::PrincipalExported {
                            request_id,
                            name,
                            output_path: output_path.display().to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal export failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalImportPreview {
                request_id,
                file_path,
                name,
                allow_unsigned: _,
                force: _,
            } => {
                match Self::preview_principal_import(
                    &state,
                    std::path::Path::new(&file_path),
                    name.clone(),
                )
                .await
                {
                    Ok(preview) => {
                        let response = ResponsePacket::PrincipalImportPreviewed {
                            request_id,
                            name: preview.name,
                            version: preview.version,
                            did: preview.did,
                            description: preview.description,
                            agents: preview.agents,
                            extensions: preview.extensions,
                            required_capabilities: preview.required_capabilities,
                            signed: preview.signed,
                            validation_errors: preview.validation_errors,
                            validation_warnings: preview.validation_warnings,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal import preview failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalImport {
                request_id,
                file_path,
                name,
                allow_unsigned,
                force,
                confirmed,
                selected_capabilities,
            } => {
                if !confirmed {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "Principal import was not confirmed. Use the preview flow or pass --yes.".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let trust_policy = if force {
                    crate::registry::packaging::TrustPolicy::AllowUntrusted
                } else {
                    crate::registry::packaging::TrustPolicy::Tofu
                };
                match Self::import_principal_package(
                    &state,
                    std::path::Path::new(&file_path),
                    name.clone(),
                    allow_unsigned,
                    trust_policy,
                    selected_capabilities,
                )
                .await
                {
                    Ok(result) => {
                        let response = ResponsePacket::PrincipalImported {
                            request_id,
                            name: result.name,
                            config_path: result.config_path.display().to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal import failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalPush {
                request_id,
                name,
                registry_host,
                registry_token,
            } => {
                match Self::push_principal_package(&state, &name, registry_host, registry_token)
                    .await
                {
                    Ok(digest) => {
                        let response = ResponsePacket::PrincipalPushed {
                            request_id,
                            name,
                            digest,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal push failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalPullPreview {
                request_id,
                registry_ref,
                name,
                force,
                registry_host,
                registry_token,
            } => {
                match Self::preview_principal_pull(
                    &state,
                    &registry_ref,
                    name.clone(),
                    force,
                    registry_host,
                    registry_token,
                )
                .await
                {
                    Ok(preview) => {
                        let response = ResponsePacket::PrincipalPullPreviewed {
                            request_id,
                            name: preview.name,
                            version: preview.version,
                            did: preview.did,
                            description: preview.description,
                            agents: preview.agents,
                            extensions: preview.extensions,
                            required_capabilities: preview.required_capabilities,
                            signed: preview.signed,
                            validation_errors: preview.validation_errors,
                            validation_warnings: preview.validation_warnings,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal pull preview failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalPull {
                request_id,
                registry_ref,
                name,
                force,
                confirmed,
                selected_capabilities,
                allow_unsigned,
                registry_host,
                registry_token,
            } => {
                if !confirmed {
                    let response = ResponsePacket::Error {
                        request_id,
                        message:
                            "Principal pull was not confirmed. Use the preview flow or pass --yes."
                                .to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                match Self::pull_principal_package(
                    &state,
                    &registry_ref,
                    name.clone(),
                    force,
                    selected_capabilities,
                    allow_unsigned,
                    registry_host,
                    registry_token,
                )
                .await
                {
                    Ok((imported_name, version, digest)) => {
                        let response = ResponsePacket::PrincipalPulled {
                            request_id,
                            name: imported_name,
                            version,
                            digest,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal pull failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalGrantPermission {
                request_id,
                name,
                permission,
                ..
            } => {
                let subject =
                    match take_resolved_subject(pre_resolved_subject.as_ref(), request_id, sink)
                        .await
                    {
                        Ok(s) => s,
                        Err(()) => return Ok(()),
                    };

                let principal = match Self::load_principal(&state, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = crate::auth::ownership::principal_resource(&name, &config);
                if let Err(denied) = crate::auth::ownership::check_permission(
                    &resource,
                    crate::auth::ownership::Permission::ManageSettings,
                    &caller_subject,
                ) {
                    warn!("PrincipalGrantPermission denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                drop(config);

                let grant = crate::auth::ownership::PermissionGrant {
                    subject: subject.clone(),
                    permission: permission.clone(),
                    granted_at: chrono::Utc::now().to_rfc3339(),
                    granted_by: caller_subject,
                };

                match state
                    .principal_manager()
                    .update_config(&name, |config| config.permissions.push(grant))
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = state.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.refresh_instance_allowed_principals(&name).await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to refresh allowed_users after principal grant: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalPermissionGranted {
                            request_id,
                            name,
                            subject,
                            permission,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalRevokePermission {
                request_id,
                name,
                permission,
                ..
            } => {
                let subject =
                    match take_resolved_subject(pre_resolved_subject.as_ref(), request_id, sink)
                        .await
                    {
                        Ok(s) => s,
                        Err(()) => return Ok(()),
                    };

                let principal = match Self::load_principal(&state, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = crate::auth::ownership::principal_resource(&name, &config);
                if let Err(denied) = crate::auth::ownership::check_permission(
                    &resource,
                    crate::auth::ownership::Permission::ManageSettings,
                    &caller_subject,
                ) {
                    warn!("PrincipalRevokePermission denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                drop(config);

                match state
                    .principal_manager()
                    .update_config(&name, |config| {
                        config.permissions.retain(|g| {
                            !(g.subject == subject && g.permission.covers(&permission))
                        });
                    })
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = state.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.refresh_instance_allowed_principals(&name).await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to refresh allowed_users after principal revoke: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalPermissionRevoked {
                            request_id,
                            name,
                            subject,
                            permission,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalPermissions { request_id, name } => {
                let principal = match Self::load_principal(&state, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = crate::auth::ownership::principal_resource(&name, &config);
                if let Err(denied) = crate::auth::ownership::check_permission(
                    &resource,
                    crate::auth::ownership::Permission::ViewSettings,
                    &caller_subject,
                ) {
                    warn!("PrincipalPermissions denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let permissions = config.permissions.clone();
                drop(config);

                let response = ResponsePacket::PrincipalPermissions {
                    request_id,
                    permissions,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalSetStatus {
                request_id,
                name,
                status,
            } => {
                use crate::principal::config::Status;
                let status_enum = match status.as_str() {
                    "online" => Status::Online,
                    "offline" => Status::Offline,
                    "busy" => Status::Busy,
                    "error" => Status::Error,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invalid status '{other}'. Expected: online, offline, busy, error"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                // Persist on the Principal's config so the change survives
                // daemon restart.
                match state
                    .principal_manager()
                    .update_config(&name, |config| {
                        config.status = Some(status_enum.clone());
                    })
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = state.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.set_instance_status(&name, status_enum.into()).await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to publish PrincipalSetStatus to hub: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalStatusUpdated {
                            request_id,
                            name,
                            status,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to persist status: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalSetExposure {
                request_id,
                name,
                exposure,
            } => {
                use crate::principal::config::Exposure;
                let exposure_enum = match exposure.as_str() {
                    "unexposed" => Exposure::Unexposed,
                    "private" => Exposure::Private,
                    "public" => Exposure::Public,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invalid exposure '{other}'. Expected: unexposed, private, public"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                match state
                    .principal_manager()
                    .update_config(&name, |config| {
                        config.exposure = exposure_enum;
                    })
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = state.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.set_instance_exposure(&name, exposure_enum.into()).await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to publish PrincipalSetExposure to hub: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalExposureUpdated {
                            request_id,
                            name,
                            exposure,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to persist exposure: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            } // ── Ownership and Permission (ADR-033) ──
              // NOTE: Team transfer/grant/revoke packets were removed along with
              // the team management concept. Only principal-scoped permission ops
              // remain here.

            // Defense-in-depth fallback for any variant not claimed by
            // a registered handler and not handled above. In normal
            // operation the handler loop at the top of `handle_request`
            // dispatches every migrated variant (currently the `system`
            // domain); this arm only fires if a new `RequestPacket`
            // variant is added without registering a handler.
            _ => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!(
                        "no handler registered for request variant (request_id={request_id})"
                    ),
                };
                send_response(sink, response).await?;
            }
        }

        Ok(())
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

    /// Handle a `PrincipalSendControl` request: soft-interrupt or
    /// steer a running `PrincipalSendStream` identified by
    /// `target_request_id`. Looks the run up in
    /// `AppState::streaming_runs`; if missing (run already completed,
    /// unknown id, or the wrong process) returns `success=false`.
    ///
    /// `Interrupt` flips the run's cancel token; the agentic loop
    /// observes it at the next iteration boundary and emits
    /// `Lifecycle::Interrupted` before returning. `Steer` derives the
    /// run's `session_id` from the stored peer and pushes a
    /// `SteeringMessage` into the same `InboxRegistry` the agentic
    /// loop drains.
    async fn handle_principal_send_control(
        request_id: u64,
        target_request_id: u64,
        mode: PrincipalSendControlMode,
        state: AppState,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        use crate::extensions::framework::async_exec::executor::SteeringMessage;
        use crate::principal::routers::root::root_session_id;

        // Snapshot the handle under the lock and drop the guard
        // before doing any work — never hold the lock across an
        // `.await` or a steering push (which takes its own inbox
        // lock).
        let snapshot = {
            let runs_registry = state.streaming_runs();
            let runs = runs_registry.lock().unwrap();
            runs.get(&target_request_id)
                .map(|h| (h.cancel.clone(), h.peer.clone(), h.principal_name.clone()))
        };

        let (success, error) = match (snapshot, mode) {
            (Some((cancel, _peer, _name)), PrincipalSendControlMode::Interrupt) => {
                cancel.cancel();
                (true, None)
            }
            (Some((_cancel, peer, _name)), PrincipalSendControlMode::Steer { text }) => {
                let session_id = root_session_id(&peer);
                let inbox = state.inbox_registry.get_or_create(&session_id).await;
                inbox.push(SteeringMessage::new(text));
                (true, None)
            }
            (None, _) => (
                false,
                Some(format!(
                    "Stream run {target_request_id} not found (already completed or unknown id)"
                )),
            ),
        };

        let response = ResponsePacket::Done {
            request_id,
            success,
            error,
        };
        send_response(sink, response).await?;
        Ok(())
    }

    // (handle_ext_start / handle_ext_stop / handle_ext_restart /
    // handle_ext_status retired to
    // `ipc::handlers::ext_runtime::ExtRuntimeHandler` under F6 step 11.
    // The background extension runtime lifecycle (ADR-025) is now
    // driven through a narrow `ExtRuntimeHost` port.)

    /// Build a PrincipalPackager for export/push, optionally resolving and
    /// embedding the extensions referenced by the principal's capabilities.
    async fn build_principal_packager(
        state: &AppState,
        name: &str,
        with_extensions: bool,
    ) -> anyhow::Result<crate::registry::packaging::PrincipalPackager> {
        let principal = Self::load_principal(state, name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Principal '{}' not found", name))?;
        let config = principal.config.read().await.clone();
        let did = config
            .did
            .as_ref()
            .map(|d| d.0.clone())
            .ok_or_else(|| anyhow::anyhow!("Principal '{}' has no identity DID", name))?;

        let resolver = crate::common::paths::PathResolver::with_dirs(
            state.config_dir.clone(),
            state.data_dir.clone(),
            state.cache_dir.clone(),
        );
        let identity = Self::load_principal_identity(&resolver, name, &did).await?;

        let packager = crate::registry::packaging::PrincipalPackager::new(config.clone(), identity)
            .with_agents_dir(resolver.principal_agents_dir(name))
            .with_memory_dir(resolver.principal_memory_dir(name))
            .with_sessions_dir(resolver.principal_sessions_dir(name));

        if with_extensions {
            let store = state.extension_store();
            let packager = packager.with_extensions_from_store(store, &config).await?;
            Ok(packager)
        } else {
            Ok(packager)
        }
    }

    /// Export a Principal to a `.principal` package on disk.
    async fn export_principal_package(
        state: &AppState,
        name: &str,
        output: Option<String>,
        include_sessions: bool,
        with_extensions: bool,
    ) -> anyhow::Result<std::path::PathBuf> {
        let packager = Self::build_principal_packager(state, name, with_extensions).await?;

        let opts = crate::registry::packaging::PrincipalExportOptions {
            output_path: output,
            include_sessions,
            with_extensions,
            description: None,
        };
        packager.export(opts).await
    }

    /// Preview shape extracted from a `.principal` package before import.
    async fn preview_principal_import(
        state: &AppState,
        file_path: &std::path::Path,
        new_name: Option<String>,
    ) -> anyhow::Result<PrincipalImportPreview> {
        let unpackager = crate::registry::packaging::PrincipalUnpackager::new(
            file_path,
            state.config_dir.clone(),
            state.data_dir.clone(),
        );
        let (manifest, files, validation) = unpackager.inspect_detailed().await?;

        let signed = !manifest.signatures.manifest.trim().is_empty();
        let name = new_name.unwrap_or_else(|| manifest.principal.name.clone());
        let agents = Self::extract_agent_names_from_package(&files);
        let extensions: Vec<String> = manifest.extensions.iter().map(|r| r.id.clone()).collect();
        let (required_capabilities, cap_warnings) =
            crate::registry::packaging::PrincipalUnpackager::extract_extension_capabilities(
                &manifest, &files,
            );

        let validation_errors: Vec<String> =
            validation.errors.iter().map(|e| format!("{e:?}")).collect();
        let validation_warnings: Vec<String> = validation
            .warnings
            .iter()
            .map(|w| format!("{w:?}"))
            .chain(cap_warnings.into_iter())
            .collect();

        Ok(PrincipalImportPreview {
            name,
            version: manifest.principal.version,
            did: manifest.principal.did,
            description: manifest.principal.description,
            agents,
            extensions,
            required_capabilities,
            signed,
            validation_errors,
            validation_warnings,
        })
    }

    /// Extract agent prompt names from the `agents/` layer of a package.
    fn extract_agent_names_from_package(
        files: &std::collections::HashMap<String, Vec<u8>>,
    ) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for path in files.keys() {
            let Some(rest) = path.strip_prefix("agents/") else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            // `agents/<name>.md`  -> `<name>`
            // `agents/<name>/AGENT.md` -> `<name>`
            let name = if rest.eq_ignore_ascii_case("AGENT.md") {
                continue;
            } else if let Some(parent) = std::path::Path::new(rest).parent() {
                let file_name = std::path::Path::new(rest)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(rest);
                if file_name.eq_ignore_ascii_case("AGENT.md") {
                    parent
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| rest.to_string())
                } else {
                    std::path::Path::new(rest)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| rest.to_string())
                }
            } else {
                std::path::Path::new(rest)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| rest.to_string())
            };
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names.sort();
        names
    }

    /// Import a `.principal` package and register it with the manager.
    async fn import_principal_package(
        state: &AppState,
        file_path: &std::path::Path,
        new_name: Option<String>,
        allow_unsigned: bool,
        trust_policy: crate::registry::packaging::TrustPolicy,
        selected_capabilities: Vec<String>,
    ) -> anyhow::Result<crate::registry::packaging::PrincipalImportResult> {
        let unpackager = crate::registry::packaging::PrincipalUnpackager::new(
            file_path,
            state.config_dir.clone(),
            state.data_dir.clone(),
        );
        let opts = crate::registry::packaging::PrincipalImportOptions {
            new_name,
            allow_unsigned,
            force: trust_policy == crate::registry::packaging::TrustPolicy::AllowUntrusted,
            trust_store: Some(state.trust_store().clone()),
            trust_policy,
            selected_capabilities,
            ..Default::default()
        };
        let mut result = unpackager.import(opts).await?;

        // Install any embedded extension packages.
        let (manifest, _validation) = unpackager.inspect().await?;
        if !manifest.extensions.is_empty() {
            let store = state.extension_store();
            let installed = unpackager
                .import_extensions(&manifest, store)
                .await
                .with_context(|| "Failed to install embedded extensions")?;
            result.installed_extensions = installed.into_iter().map(|id| id.0).collect();
        }

        // Load the freshly imported principal into the in-memory manager.
        let resolver = crate::common::paths::PathResolver::with_dirs(
            state.config_dir.clone(),
            state.data_dir.clone(),
            state.cache_dir.clone(),
        );
        let config_path = resolver.principal_config(&result.name);
        if let Err(e) = state.principal_manager().load(&config_path).await {
            warn!(
                "Imported principal '{}' but failed to load it: {}",
                result.name, e
            );
        }

        Ok(result)
    }

    /// Push a Principal to a registry, returning the pushed manifest digest.
    async fn push_principal_package(
        state: &AppState,
        name: &str,
        registry_host: Option<String>,
        registry_token: Option<String>,
    ) -> anyhow::Result<String> {
        let packager = Self::build_principal_packager(state, name, true).await?;
        let version = "1.0.0".to_string();

        let descriptor = packager
            .export_for_registry(crate::registry::packaging::PrincipalExportOptions {
                with_extensions: true,
                ..Default::default()
            })
            .await?;

        let host = registry_host.unwrap_or_else(|| "pekohub.org".to_string());
        let mut reg_config = crate::registry::config::load_from_workspace(&state.data_dir);
        if let Some(token) = registry_token {
            reg_config.add_source(crate::registry::config::RegistrySource {
                url: host.clone(),
                priority: 1,
                auth: None,
                token: Some(token),
            });
        }

        let agent_registry =
            crate::registry::AgentRegistry::new(crate::registry::AgentRegistry::default_path());
        agent_registry.init().await?;

        let client = crate::registry::client::RegistryClient::new(reg_config, agent_registry);
        let remote_ref = format!("{host}/peko/principals/{name}:{version}");
        let manifest = client
            .push_principal(&descriptor, name, &version, &remote_ref, |_| {})
            .await?;

        // Best-effort cleanup of the temporary local package file.
        let _ = std::fs::remove_file(&descriptor.package_path);

        Ok(manifest.digest)
    }

    /// Preview a remote Principal package before pulling it.
    async fn preview_principal_pull(
        state: &AppState,
        registry_ref: &str,
        new_name: Option<String>,
        _force: bool,
        registry_host: Option<String>,
        registry_token: Option<String>,
    ) -> anyhow::Result<PrincipalImportPreview> {
        let host = registry_host.unwrap_or_else(|| {
            crate::registry::client::RegistryRef::parse_with_default(
                registry_ref,
                None,
                Some(crate::registry::client::ResourceType::Principal),
            )
            .map(|r| r.host)
            .unwrap_or_else(|_| "pekohub.org".to_string())
        });

        let mut reg_config = crate::registry::config::load_from_workspace(&state.data_dir);
        if let Some(token) = registry_token {
            reg_config.add_source(crate::registry::config::RegistrySource {
                url: host.clone(),
                priority: 1,
                auth: None,
                token: Some(token),
            });
        }

        let agent_registry =
            crate::registry::AgentRegistry::new(crate::registry::AgentRegistry::default_path());
        agent_registry.init().await?;

        let client = crate::registry::client::RegistryClient::new(reg_config, agent_registry);

        let temp_path = state.cache_dir.join(format!(
            "peko-pull-principal-preview-{}.principal",
            std::process::id()
        ));
        let _manifest = client
            .pull_principal(registry_ref, &temp_path, |_| {})
            .await?;

        let preview = Self::preview_principal_import(state, &temp_path, new_name).await;
        let _ = std::fs::remove_file(&temp_path);
        preview
    }

    /// Pull a Principal from a registry and import it.
    async fn pull_principal_package(
        state: &AppState,
        registry_ref: &str,
        new_name: Option<String>,
        force: bool,
        selected_capabilities: Vec<String>,
        allow_unsigned: bool,
        registry_host: Option<String>,
        registry_token: Option<String>,
    ) -> anyhow::Result<(String, String, String)> {
        let host = registry_host.unwrap_or_else(|| {
            crate::registry::client::RegistryRef::parse_with_default(
                registry_ref,
                None,
                Some(crate::registry::client::ResourceType::Principal),
            )
            .map(|r| r.host)
            .unwrap_or_else(|_| "pekohub.org".to_string())
        });

        let mut reg_config = crate::registry::config::load_from_workspace(&state.data_dir);
        if let Some(token) = registry_token {
            reg_config.add_source(crate::registry::config::RegistrySource {
                url: host.clone(),
                priority: 1,
                auth: None,
                token: Some(token),
            });
        }

        let agent_registry =
            crate::registry::AgentRegistry::new(crate::registry::AgentRegistry::default_path());
        agent_registry.init().await?;

        let client = crate::registry::client::RegistryClient::new(reg_config, agent_registry);

        let temp_path = state.cache_dir.join(format!(
            "peko-pull-principal-{}.principal",
            std::process::id()
        ));
        let manifest = client
            .pull_principal(registry_ref, &temp_path, |_| {})
            .await?;

        let import_result = Self::import_principal_package(
            state,
            &temp_path,
            new_name,
            // Pulled packages are signed at export; honor force for overwrite
            // and trust pinning override.
            allow_unsigned,
            if force {
                crate::registry::packaging::TrustPolicy::AllowUntrusted
            } else {
                crate::registry::packaging::TrustPolicy::Tofu
            },
            selected_capabilities,
        )
        .await;
        let _ = std::fs::remove_file(&temp_path);

        let result = match import_result {
            Ok(r) => r,
            Err(e) => {
                if force {
                    return Err(anyhow::anyhow!("Import after pull failed: {e}"));
                }
                return Err(e);
            }
        };

        Ok((
            result.name,
            manifest.version.clone(),
            manifest.digest.clone(),
        ))
    }

    /// Load a Principal's `Identity` (with keypair) from its identity store.
    async fn load_principal_identity(
        resolver: &crate::common::paths::PathResolver,
        name: &str,
        did: &str,
    ) -> anyhow::Result<crate::identity::Identity> {
        let identity_dir = resolver.principal_identity_dir(name);
        let did = did.to_string();
        tokio::task::spawn_blocking(move || {
            let storage = crate::identity::storage::KeyStorage::with_path(identity_dir)?;
            storage.load(&did)
        })
        .await?
    }

    // ─── peko log read path ──────────────────────────────────────────

    /// Server-side handler for `RequestPacket::PrincipalLog`.
    ///
    /// Enforces three gates in order:
    /// 1. **`Chat` permission** on the principal — same gate as a peer
    ///    wanting to chat at all.
    /// 2. **Peer-privacy match** — `caller == target_peer` (you're
    ///    reading your own thread) or `caller == owner` (the owner can
    ///    audit any peer).
    /// 3. **No `Subject::Public` thread** — `public` is not a session
    ///    peer (`Subject::is_session_peer`).
    ///
    /// Privacy invariant: the default view is the *owner's* thread, not
    /// the caller's. This is intentional — `peko log` is the owner's
    /// activity feed, distinct from a peer's own read-back. A non-owner
    /// peer calling `peko log` without `--peer` therefore errors out
    /// (the request resolves to owner-view, but caller is not the owner).
    async fn read_principal_log(
        state: &AppState,
        name: &str,
        peer: Option<Subject>,
        limit: Option<usize>,
        since_secs: Option<u64>,
        caller: Subject,
    ) -> Result<PrincipalLogResponse, PrincipalLogError> {
        // ── Resolve the principal ─────────────────────────────────────
        let manager = state.principal_manager();
        let principal = manager
            .get_by_name(name)
            .await
            .ok_or_else(|| PrincipalLogError::NotFound(format!("Principal '{name}' not loaded")))?;

        // ── Build the resource for permission gating ──────────────────
        let (owner, permissions, exposure) = {
            let cfg = principal.config.read().await;
            (cfg.owner.clone(), cfg.permissions.clone(), cfg.exposure)
        };
        let resource = Resource::Principal {
            name: name.to_string(),
            owner: owner.clone(),
            permissions,
            exposure,
        };

        // ── Chat permission ───────────────────────────────────────────
        if check_permission(&resource, Permission::Chat, &caller).is_err() {
            return Err(PrincipalLogError::Forbidden(format!(
                "caller '{caller}' lacks Chat permission on principal '{name}'"
            )));
        }

        // ── Resolve the target peer ───────────────────────────────────
        // Default is the principal's owner (the owner-root view). A
        // caller who isn't the owner and didn't supply `--peer` is
        // asking for the owner's thread and is rejected by the privacy
        // check below.
        let target_peer = peer.unwrap_or_else(|| owner.clone());

        if !target_peer.is_session_peer() {
            return Err(PrincipalLogError::Forbidden(format!(
                "subject '{target_peer}' is not a session peer"
            )));
        }

        // ── Peer-privacy match ────────────────────────────────────────
        if caller != target_peer && caller != owner {
            return Err(PrincipalLogError::Forbidden(
                "you can only read your own conversation; ask the owner to read on your behalf"
                    .to_string(),
            ));
        }

        // ── Resolve session id ────────────────────────────────────────
        let artifact = principal
            .memory
            .find_latest_session_for_peer(&target_peer)
            .await
            .map_err(|e| {
                PrincipalLogError::Internal(format!(
                    "failed to look up session for '{target_peer}': {e}"
                ))
            })?;

        let Some(artifact) = artifact else {
            return Ok(PrincipalLogResponse {
                name: name.to_string(),
                peer: target_peer,
                session_id: None,
                events: Vec::new(),
                truncated: false,
            });
        };
        let session_id = artifact.session_id.clone();
        drop(artifact);

        // ── Stream the session JSONL ─────────────────────────────────
        let effective_limit = limit.unwrap_or(50).clamp(1, 1000);
        let (events, truncated) = Self::load_principal_session_events(
            principal.memory.sessions_dir().join(&session_id),
            since_secs,
            effective_limit,
        )
        .await
        .map_err(|e| PrincipalLogError::Internal(format!("read failed: {e}")))?;

        Ok(PrincipalLogResponse {
            name: name.to_string(),
            peer: target_peer,
            session_id: Some(session_id),
            events,
            truncated,
        })
    }

    /// Read a principal-owned JSONL session file and convert each event
    /// into `HistoryEvent`. Applies `since_secs` (skips events whose
    /// `envelope.ts` is older than `now() - since_secs`) and `limit`
    /// (caps the number of returned events, oldest-first). Reports
    /// truncation via the second tuple field when the file held more
    /// events than the limit allows for.
    ///
    /// Missing files (`session.jsonl` not yet created) yield `(vec![], false)`.
    async fn load_principal_session_events(
        path: std::path::PathBuf,
        since_secs: Option<u64>,
        limit: usize,
    ) -> anyhow::Result<(Vec<HistoryEvent>, bool)> {
        if !path.exists() {
            return Ok((Vec::new(), false));
        }

        let cutoff = since_secs.map(|s| chrono::Utc::now() - chrono::Duration::seconds(s as i64));
        let raw = tokio::fs::read_to_string(&path).await?;

        // Two-pass: collect (ts, HistoryEvent) tuples preserving order,
        // then apply the since+limit window in document order. This
        // matches `SessionService::get_history`'s semantic (oldest-first
        // within the window).
        let mut ordered: Vec<(chrono::DateTime<chrono::Utc>, HistoryEvent)> = Vec::new();

        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let event: SessionEvent = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue, // skip malformed lines; JSONL append-only durability wins
            };
            let ts = event.envelope().ts;
            if let Some(cutoff_ts) = cutoff {
                if ts < cutoff_ts {
                    continue;
                }
            }
            if let Some(hist) = session_event_to_history(&event) {
                ordered.push((ts, hist));
            }
        }

        let truncated = ordered.len() > limit;
        ordered.truncate(limit);
        let events: Vec<HistoryEvent> = ordered.into_iter().map(|(_, h)| h).collect();
        Ok((events, truncated))
    }

    /// Resolve a Principal by name, loading it from disk if it has not yet
    /// been loaded into the daemon's in-memory manager.
    async fn load_principal(state: &AppState, name: &str) -> Option<Arc<Principal>> {
        let manager = state.principal_manager();
        if let Some(principal) = manager.get_by_name(name).await {
            return Some(principal);
        }

        let resolver = crate::common::paths::PathResolver::with_dirs(
            state.config_dir.clone(),
            state.data_dir.clone(),
            state.cache_dir.clone(),
        );
        let config_path = resolver.principal_config(name);
        if config_path.exists() {
            if let Err(e) = manager.load(&config_path).await {
                warn!(
                    "Failed to load principal '{}' from {}: {}",
                    name,
                    config_path.display(),
                    e
                );
                return None;
            }
        }

        manager.get_by_name(name).await
    }

    /// Shared body for `RequestPacket::PrincipalSend` and
    /// `RequestPacket::PrincipalSendStream`. Both IPC variants run the
    /// root agent via the streaming machinery (`router.route_streaming`)
    /// and register a `CancellationToken` in `streaming_runs`, so the
    /// `PrincipalSendControl` IPC works uniformly regardless of which
    /// variant the caller chose. The only difference at the wire level
    /// is the success packet — `PrincipalSent` for `OneShot` and
    /// `PrincipalSentDone` for `Streaming` — selected by
    /// `response_kind`.
    #[allow(clippy::too_many_arguments)]
    async fn run_principal_send(
        request_id: u64,
        name: String,
        message: String,
        user: String,
        no_slash: bool,
        output_format: crate::common::types::OutputFormat,
        state: AppState,
        sink: &dyn ResponseSink,
        response_kind: PrincipalSendResponseKind,
    ) -> anyhow::Result<()> {
        // Look up the principal first — short-circuit with a clean
        // Error packet and Done so the client doesn't hang waiting on
        // a never-arriving response.
        let principal = match Self::load_principal(&state, &name).await {
            Some(p) => p,
            None => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!("Principal '{}' not found", name),
                };
                send_response(sink, response).await?;
                let done = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(format!("Principal '{name}' not found")),
                };
                send_response(sink, done).await?;
                return Ok(());
            }
        };

        // Intercept slash commands before acquiring the run permit or
        // building a router context. This keeps the behavior uniform
        // across CLI, GUI, and tunnel callers.
        let (slash_response, message) = match state
            .principal_manager()
            .preprocess_slash(&principal, message, no_slash, output_format)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: e.to_string(),
                };
                send_response(sink, response).await?;
                let done = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(e.to_string()),
                };
                send_response(sink, done).await?;
                return Ok(());
            }
        };

        if let Some(content) = slash_response {
            let final_packet = match response_kind {
                PrincipalSendResponseKind::Streaming => ResponsePacket::PrincipalSentDone {
                    request_id,
                    content,
                },
                PrincipalSendResponseKind::OneShot => ResponsePacket::PrincipalSent {
                    request_id,
                    content,
                },
            };
            send_response(sink, final_packet).await?;
            let done = ResponsePacket::Done {
                request_id,
                success: true,
                error: None,
            };
            send_response(sink, done).await?;
            return Ok(());
        }

        let peer = crate::auth::Subject::User(user);
        let channel = ChannelContext {
            kind: ChannelKind::Cli,
            // The channel flag is informational — both variants are
            // routed through the streaming machinery and the
            // streaming_runs registry now, so a `OneShot` request
            // still has cancel capability.
            streaming: matches!(response_kind, PrincipalSendResponseKind::Streaming),
        };

        // Construct the RouterContext the root router expects.
        // Audit H1: the streaming path now uses the same
        // `PrincipalManager::build_router_context` helper as the
        // legacy one-shot `PrincipalManager::receive` path (which
        // is no longer called from this handler), so permission
        // checks, session recall, and per-message configuration
        // can't drift between the two variants.
        let router_ctx = match state
            .principal_manager()
            .build_router_context(&principal, peer.clone(), message.clone(), channel)
            .await
        {
            Ok(ctx) => ctx,
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!("Failed to build router context: {e}"),
                };
                send_response(sink, response).await?;
                let done = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(e.to_string()),
                };
                send_response(sink, done).await?;
                return Ok(());
            }
        };

        // Bounded channel for streaming events. Capacity 256; a slow
        // client back-pressures the root agent (events are dropped on
        // `try_send` failure). Note: for the `OneShot` variant we
        // still drain the channel into a temporary buffer — the
        // `Streaming` branch emits the chunks, the `OneShot` branch
        // discards them because the client expects a single
        // `PrincipalSent { content }` at the end.
        let (event_tx, mut event_rx) =
            tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(256);

        // Oneshot for the final RouteDecision.
        let (result_tx, result_rx) =
            tokio::sync::oneshot::channel::<Result<RouteDecision, RouterError>>();

        let on_event = move |event: crate::engine::AgenticEvent| {
            let _ = event_tx.try_send(event);
        };

        // Soft-interrupt plumbing. The cancel token is shared
        // between the spawned agentic loop (observed at iteration
        // boundaries) and the in-flight run registry (the
        // `PrincipalSendControl` IPC handler flips it). The Drop
        // guard removes the registry entry on every return path,
        // including the early sink-error return below and panics.
        let cancel = tokio_util::sync::CancellationToken::new();
        let interrupt_acked = Arc::new(tokio::sync::Notify::new());
        let run_handle = StreamingRunHandle {
            principal_name: name.clone(),
            peer: peer.clone(),
            cancel: cancel.clone(),
            interrupt_acked: Arc::clone(&interrupt_acked),
        };
        {
            let runs_registry = state.streaming_runs();
            let mut runs = runs_registry.lock().unwrap();
            runs.insert(request_id, run_handle);
        }
        let _run_guard = StreamingRunGuard {
            registry: state.streaming_runs(),
            request_id,
        };

        // Run the root agent in a background task. When the task
        // completes, the event_tx is dropped, closing the channel
        // and signalling the handler to flush.
        let router = Arc::clone(&principal.router);
        let root_agent_handle = tokio::spawn(async move {
            let result = router
                .route_streaming(router_ctx, Box::new(on_event), Some(cancel))
                .await;
            let _ = result_tx.send(result);
        });

        // Drain the channel. For `Streaming` we forward each
        // delta to the client; for `OneShot` we discard the events
        // and rely on the final `PrincipalSent { content }` to
        // carry the answer. Either way, a sink-write error aborts
        // the root agent task and returns early.
        while let Some(event) = event_rx.recv().await {
            let delta = match event {
                crate::engine::AgenticEvent::AssistantDelta { text, .. } => text,
                crate::engine::AgenticEvent::AssistantText { text, .. } => text,
                _ => continue,
            };
            if matches!(response_kind, PrincipalSendResponseKind::Streaming) {
                let packet = ResponsePacket::PrincipalSentChunk { request_id, delta };
                if let Err(e) = send_response(sink, packet).await {
                    tracing::warn!("failed to send PrincipalSentChunk: {e}; aborting stream");
                    root_agent_handle.abort();
                    let done = ResponsePacket::Done {
                        request_id,
                        success: false,
                        error: Some(format!("sink write failed: {e}")),
                    };
                    send_response(sink, done).await?;
                    return Ok(());
                }
            }
            // For OneShot we drop `delta` — the client expects one
            // final packet with the full answer, not deltas.
        }

        // The channel closed because the root agent task dropped
        // `event_tx`. Await the result.
        let route_result = match result_rx.await {
            Ok(r) => r,
            Err(_) => Err(RouterError::AgentFailed(
                "root-agent task died before producing a result".into(),
            )),
        };
        let _ = root_agent_handle.await;

        match route_result {
            Ok(decision) => {
                let content = match decision {
                    RouteDecision::Respond { response } => response,
                };
                let final_packet = match response_kind {
                    PrincipalSendResponseKind::Streaming => ResponsePacket::PrincipalSentDone {
                        request_id,
                        content,
                    },
                    PrincipalSendResponseKind::OneShot => ResponsePacket::PrincipalSent {
                        request_id,
                        content,
                    },
                };
                send_response(sink, final_packet).await?;
                let done = ResponsePacket::Done {
                    request_id,
                    success: true,
                    error: None,
                };
                send_response(sink, done).await?;
                state.record_principal_activity(&name).await;
            }
            Err(e) => {
                let message = e.to_string();
                let response = ResponsePacket::Error {
                    request_id,
                    message: message.clone(),
                };
                send_response(sink, response).await?;
                let done = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(message),
                };
                send_response(sink, done).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_agent_names_handles_flat_and_nested_prompts() {
        let mut files = std::collections::HashMap::new();
        files.insert("agents/primary.md".to_string(), vec![]);
        files.insert("agents/researcher/AGENT.md".to_string(), vec![]);
        files.insert("agents/utils.toml".to_string(), vec![]);
        files.insert("config/principal.toml".to_string(), vec![]);

        let mut names = IpcServer::extract_agent_names_from_package(&files);
        names.sort();

        assert_eq!(names, vec!["primary", "researcher", "utils"]);
    }

    #[test]
    fn extract_agent_names_ignores_top_level_agent_md() {
        // A bare `agents/AGENT.md` is not a named prompt; skip it.
        let mut files = std::collections::HashMap::new();
        files.insert("agents/AGENT.md".to_string(), vec![]);
        files.insert("agents/primary.md".to_string(), vec![]);

        let names = IpcServer::extract_agent_names_from_package(&files);
        assert_eq!(names, vec!["primary"]);
    }
}
