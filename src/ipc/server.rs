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
use tokio::time::interval;
use tracing::{error, info, trace, warn};

use super::packet::{AuthenticatedRequest, RequestPacket, ResponsePacket, HEARTBEAT_INTERVAL_SECS};
use super::response_sink::{sink_for_unix_or_udp, ResponseSink};
use super::{ensure_run_dir, DEFAULT_HOST, DEFAULT_PORT};
#[cfg(windows)]
use super::{default_pipe_name, response_sink::sink_for_pipe, DAEMON_PIPE_ENV};
use crate::auth::caller::CallerContext;
#[cfg(not(windows))]
use crate::auth::config::enforce_auth_for_public_bind;
use crate::auth::permissions::AuthError;
use crate::daemon::state::AppState;

/// Platform-specific server socket (wrapped in Arc for shared ownership)
#[derive(Clone)]
pub(crate) enum ServerSocket {
    #[cfg(unix)]
    Unix {
        socket: Arc<UnixDatagram>,
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

/// Principal address returned by `ServerSocket::recv_from` and threaded through
/// the request handlers so they can `send_to` the response back. Unix domain
/// datagram sockets return the sender's filesystem path; UDP returns a
/// `std::net::SocketAddr`. Windows named pipes (ADR-038) are
/// connection-oriented and have no per-message peer address — the
/// `Local` variant represents a connection that is local by construction.
#[derive(Debug, Clone)]
pub(crate) enum PeerAddr {
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
                let path = addr
                    .as_pathname()
                    .map(|p| p.to_path_buf())
                    .ok_or_else(|| {
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
            let pipe_name = std::env::var(DAEMON_PIPE_ENV)
                .unwrap_or_else(|_| default_pipe_name());
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
                                        if let Err(e) = Self::handle_request(envelope.packet, caller, state, &*sink, &peer).await {
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
            let is_jwt = matches!(envelope.auth.credential, super::packet::AuthCredential::Jwt(_));
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
        let pre_resolved_subject: Option<crate::auth::principal::Principal> =
            match &request {
                RequestPacket::AgentGrantPermission { .. }
                | RequestPacket::AgentRevokePermission { .. }
                | RequestPacket::TeamGrantPermission { .. }
                | RequestPacket::TeamRevokePermission { .. } => Some(request.resolved_subject()),
                _ => None,
            };

        /// Take the pre-resolved subject for a grant/revoke arm.
        /// Sends a `ResponsePacket::Error` and returns `Err(())` on
        /// resolution failure (caller should `return Ok(())`); returns
        /// `Ok(principal)` on success. Defined inside `handle_request`
        /// to avoid threading `sink` through a free-function signature.
        async fn take_resolved_subject(
            pre_resolved: Option<&crate::auth::principal::Principal>,
            _request_id: u64,
            _sink: &dyn crate::ipc::response_sink::ResponseSink,
        ) -> Result<crate::auth::principal::Principal, ()> {
            let Some(p) = pre_resolved else {
                unreachable!("take_resolved_subject called for a non-grant/revoke variant")
            };
            Ok(p.clone())
        }

        match request {
            RequestPacket::Ping { request_id } => {
                let uptime = state.uptime_seconds();
                let response = ResponsePacket::Pong {
                    request_id,
                    uptime_secs: uptime,
                    version: crate::VERSION.to_string(),
                };
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::Shutdown { request_id, force } => {
                info!("Shutdown request received via IPC (force={})", force);
                let response = ResponsePacket::ShuttingDown { request_id };
                Self::send_sink(sink, response).await?;
                state.request_shutdown(force).await;
            }

            RequestPacket::Execute {
                request_id,
                agent,
                team,
                message,
                session_id,
                new_session,
                stream,
                user,
            } => {
                Self::handle_execute(
                    request_id,
                    agent,
                    team,
                    message,
                    session_id,
                    new_session,
                    stream,
                    user,
                    state,
                    sink,
                    peer,
                )
                .await?;
            }

            RequestPacket::AsyncSpawn {
                request_id,
                tool_name,
                params,
                session_key,
                workspace,
            } => {
                Self::handle_async_spawn(
                    request_id,
                    tool_name,
                    params,
                    session_key,
                    workspace,
                    state,
                    sink,
                    peer,
                )
                .await?;
            }

            RequestPacket::AsyncCancel {
                request_id,
                task_id,
            } => {
                Self::handle_async_cancel(request_id, task_id, state, sink, peer).await?;
            }

            RequestPacket::CronList {
                request_id,
                include_disabled,
            } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.list_jobs(include_disabled) {
                        Ok(jobs) => {
                            let response = ResponsePacket::CronList { request_id, jobs };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to list jobs: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::CronAdd { request_id, job } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.add_job(&job) {
                        Ok(()) => {
                            let response = ResponsePacket::CronAdded {
                                request_id,
                                job_id: job.id,
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to add job: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::CronRemove { request_id, job_id } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.delete_job(&job_id) {
                        Ok(true) => {
                            let response = ResponsePacket::CronRemoved { request_id, job_id };
                            Self::send_sink(sink, response).await?;
                        }
                        Ok(false) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Job {job_id} not found"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to remove job: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::CronRun { request_id, job_id } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.get_job(&job_id) {
                        Ok(Some(_job)) => {
                            let now = chrono::Utc::now();
                            if let Err(e) =
                                scheduler.update_job_after_run(&job_id, "triggered", now)
                            {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Failed to trigger job: {e}"),
                                };
                                Self::send_sink(sink, response).await?;
                            } else {
                                let run_id = uuid::Uuid::new_v4().to_string();
                                let response = ResponsePacket::CronRunStarted {
                                    request_id,
                                    job_id,
                                    run_id,
                                };
                                Self::send_sink(sink, response).await?;
                            }
                        }
                        Ok(None) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Job {job_id} not found"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to get job: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::CronHistory {
                request_id,
                job_id,
                limit,
            } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.get_run_history(&job_id, limit) {
                        Ok(runs) => {
                            let response = ResponsePacket::CronHistory { request_id, runs };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to get history: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            // ─── Extension Runtime Lifecycle (ADR-026) ───────────────────────
            RequestPacket::ExtStart {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_start(request_id, extension_id, state, sink, peer).await?;
            }

            RequestPacket::ExtStop {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_stop(request_id, extension_id, state, sink, peer).await?;
            }

            RequestPacket::ExtRestart {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_restart(request_id, extension_id, state, sink, peer).await?;
            }

            RequestPacket::ExtStatus {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_status(request_id, extension_id, state, sink, peer).await?;
            }

            // ─── Agent CRUD ─────────────────────────────────────────────────
            RequestPacket::AgentList {
                request_id,
                team_filter,
            } => {
                let service = state.agent_mgmt_service();
                match service.list_agents(team_filter.as_deref()).await {
                    Ok(agents) => {
                        let response = ResponsePacket::AgentList { request_id, agents };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::AgentGet {
                request_id,
                name,
                team,
            } => {
                let service = state.agent_mgmt_service();
                match service.get_agent(&name, team.as_deref()).await {
                    Ok(agent) => {
                        let response = ResponsePacket::AgentGet { request_id, agent };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::AgentCreate {
                request_id,
                request,
            } => {
                let service = state.agent_mgmt_service();
                let mut request = request;
                if request.host_runtime_id.is_none() {
                    request.host_runtime_id = Some(state.runtime_identity().runtime_did.clone());
                }
                if request.owner_id.is_none() {
                    request.owner_id = Some(caller.subject_id());
                }
                let agent_name = request.name.clone();
                match service.create_agent(request).await {
                    Ok(result) => {
                        // ADR-035: Announce the new instance if tunnel is connected
                        if let Some(dispatcher) = state.tunnel_dispatcher().await {
                            if dispatcher.is_ready().await {
                                if let Err(e) = dispatcher.announce_single_instance(&agent_name).await {
                                    warn!("Failed to announce new agent instance {}: {}", agent_name, e);
                                }
                            }
                        }
                        let response = ResponsePacket::AgentCreated { request_id, result };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::AgentDelete {
                request_id,
                name,
                team,
                force,
            } => {
                let service = state.agent_mgmt_service();
                // ADR-033: Enforce ownership/permission check before deletion
                let agent_info = match service.get_agent(&name, team.as_deref()).await {
                    Ok(Some(info)) => info,
                    Ok(None) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Agent '{}' not found", name),
                        };
                        Self::send_sink(sink, response).await?;
                        return Ok(());
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                        return Ok(());
                    }
                };
                let resource = crate::auth::ownership::agent_resource(&name, &agent_info.config);
                if let Err(denied) = crate::auth::ownership::check_permission(
                    &resource,
                    crate::auth::ownership::Permission::Delete,
                    &caller.subject(),
                ) {
                    warn!("AgentDelete permission denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                    return Ok(());
                }
                let opts = crate::common::types::agent::AgentDeleteOptions {
                    force,
                    ..Default::default()
                };
                match service.delete_agent(&name, team.as_deref(), opts).await {
                    Ok(result) => {
                        let response = ResponsePacket::AgentDeleted { request_id, result };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::AgentMove {
                request_id,
                old_name,
                new_name,
                team,
            } => {
                let service = state.agent_mgmt_service();
                match service
                    .rename_agent(&old_name, &new_name, team.as_deref())
                    .await
                {
                    Ok(result) => {
                        let response = ResponsePacket::AgentMoved { request_id, result };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::AgentUpdate {
                request_id,
                name,
                team,
                model,
                description,
                system_prompt,
                config,
            } => {
                let service = state.agent_mgmt_service();
                let update_req = crate::common::types::agent::AgentUpdateRequest {
                    image: None,
                    model,
                    description,
                    system_prompt,
                    config,
                };
                match service.update_agent(&name, team.as_deref(), update_req).await {
                    Ok(_) => {
                        let response = ResponsePacket::AgentUpdated { request_id, name };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::AgentExport {
                request_id,
                name,
                team,
                output,
                include_sessions,
                with_extensions,
            } => {
                let service = state.agent_mgmt_service();
                let opts = crate::common::types::agent::AgentExportOptions {
                    output_path: output.map(std::path::PathBuf::from),
                    include_sessions,
                    with_extensions,
                };
                match service.export_agent(&name, team.as_deref(), opts).await {
                    Ok(result) => {
                        let response = ResponsePacket::AgentExported {
                            request_id,
                            name: result.name,
                            output_path: result.output_path.to_string_lossy().to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::AgentImport {
                request_id,
                file_path,
                name,
                team: _team,
                allow_unsigned,
            } => {
                let service = state.agent_mgmt_service();
                let opts = crate::common::types::agent::AgentImportOptions {
                    name,
                    force: false,
                    allow_unsigned,
                };
                match service
                    .import_agent(std::path::Path::new(&file_path), opts)
                    .await
                {
                    Ok(result) => {
                        // Update host_runtime_id to current runtime
                        let config_path = result.config_path.clone();
                        if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
                            if let Ok(mut config) =
                                toml::from_str::<crate::agents::agent_config::AgentConfig>(&content)
                            {
                                config.host_runtime_id =
                                    state.runtime_identity().runtime_did.clone();
                                if let Ok(updated) = toml::to_string_pretty(&config) {
                                    let _ = tokio::fs::write(&config_path, updated).await;
                                }
                            }
                        }
                        let response = ResponsePacket::AgentImported {
                            request_id,
                            name: result.name,
                            config_path: result.config_path.to_string_lossy().to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        // Use Debug formatting so the full anyhow
                        // error chain (top-level `context()` wrapper
                        // plus the underlying cause) is preserved
                        // across the IPC boundary. With
                        // `e.to_string()` (Display) anyhow shows only
                        // the topmost context, which leaves callers
                        // — and the integration tests — with an
                        // opaque "Failed to import agent package"
                        // and no indication of the actual cause
                        // (e.g. `signature_verification_failed`).
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("{e:?}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            // ─── Team CRUD ──────────────────────────────────────────────────
            RequestPacket::TeamList { request_id } => {
                let service = state.team_service();
                match service.list_teams().await {
                    Ok(teams) => {
                        let response = ResponsePacket::TeamList { request_id, teams };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamGet { request_id, name } => {
                let service = state.team_service();
                match service.get_team(&name).await {
                    Ok(team) => {
                        let response = ResponsePacket::TeamGet { request_id, team };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamCreate {
                request_id,
                name,
                description,
                members,
            } => {
                let service = state.team_service();
                let host_runtime_id = state.runtime_identity().runtime_did.clone();
                let owner = caller.subject();
                match service
                    .create_team(
                        &name,
                        description.as_deref(),
                        Some(&host_runtime_id),
                        Some(&owner),
                    )
                    .await
                {
                    Ok(result) => {
                        // Auto-join members if provided
                        if let Some(member_names) = members {
                            for agent_name in member_names {
                                let _ = service
                                    .join_team(
                                        &name,
                                        &agent_name,
                                        crate::common::types::membership::MembershipRole::Member,
                                    )
                                    .await;
                            }
                        }
                        let response = ResponsePacket::TeamCreated { request_id, result };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamJoin {
                request_id,
                team,
                agent,
            } => {
                let service = state.team_service();
                match service
                    .join_team(
                        &team,
                        &agent,
                        crate::common::types::membership::MembershipRole::Member,
                    )
                    .await
                {
                    Ok(result) => {
                        let response = ResponsePacket::TeamJoined {
                            request_id,
                            agent: result.agent,
                            team: result.team,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamLeave {
                request_id,
                team,
                agent,
            } => {
                let service = state.team_service();
                match service.leave_team(&team, &agent).await {
                    Ok(result) => {
                        let response = ResponsePacket::TeamLeft {
                            request_id,
                            agent: result.agent,
                            team: result.team,
                            was_member: result.was_member,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamDelete {
                request_id,
                name,
                force: _,
            } => {
                let service = state.team_service();
                match service.delete_team(&name).await {
                    Ok(result) => {
                        let response = ResponsePacket::TeamDeleted { request_id, result };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamMove {
                request_id,
                old_name,
                new_name,
            } => {
                let service = state.team_service();
                match service.move_team(&old_name, &new_name).await {
                    Ok(_) => {
                        let response = ResponsePacket::TeamMoved {
                            request_id,
                            old_name,
                            new_name,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamExport {
                request_id,
                name,
                output,
                include_sessions,
            } => {
                let service = state.team_service();
                match service
                    .export_team(&name, output, !include_sessions, false, false)
                    .await
                {
                    Ok(result) => {
                        let response = ResponsePacket::TeamExported {
                            request_id,
                            name: result.name,
                            output_path: result.output_path.to_string_lossy().to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::TeamImport {
                request_id,
                file_path,
                name,
                force,
            } => {
                let service = state.team_service();
                let host_runtime_id = state.runtime_identity().runtime_did.clone();
                match service
                    .import_team(&file_path, name, force, true, Some(&host_runtime_id))
                    .await
                {
                    Ok(result) => {
                        let response = ResponsePacket::TeamImported {
                            request_id,
                            name: result.name,
                            path: result.path.to_string_lossy().to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            // ─── Session CRUD ───────────────────────────────────────────────
            RequestPacket::SessionList {
                request_id,
                agent,
                team,
            } => {
                let service = state.session_service();
                match agent {
                    Some(agent_name) => {
                        let session_peer =
                            crate::auth::principal::Principal::User("default".to_string());
                        match service
                            .list_sessions_with_active(&agent_name, team.as_deref(), &session_peer)
                            .await
                        {
                            Ok((sessions, active_session)) => {
                                let response = ResponsePacket::SessionList {
                                    request_id,
                                    sessions,
                                    active_session,
                                };
                                Self::send_sink(sink, response).await?;
                            }
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: e.to_string(),
                                };
                                Self::send_sink(sink, response).await?;
                            }
                        }
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: "Agent name is required for session listing".to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::SessionGet { request_id, id: _ } => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: "SessionGet requires both agent name and session ID. Use the HTTP API for detailed session lookups.".to_string(),
                };
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::SessionShow {
                request_id,
                agent,
                team,
                session_id,
                history,
            } => {
                let service = state.session_service();
                match service
                    .get_session_details(&agent, team.as_deref(), &session_id)
                    .await
                {
                    Ok(Some(details)) => {
                        let history_events = if history {
                            match service
                                .get_history(
                                    &agent,
                                    team.as_deref(),
                                    &session_id,
                                    crate::common::services::HistoryQuery {
                                        limit: 100,
                                        ..Default::default()
                                    },
                                )
                                .await
                            {
                                Ok(result) => Some(result.events),
                                Err(_) => None,
                            }
                        } else {
                            None
                        };
                        let response = ResponsePacket::SessionShown {
                            request_id,
                            session: details,
                            history: history_events,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Ok(None) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Session '{session_id}' not found"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::SessionRemove {
                request_id,
                agent,
                team,
                session_id,
                force: _,
            } => {
                let service = state.session_service();
                match service
                    .delete_session(&agent, team.as_deref(), &session_id)
                    .await
                {
                    Ok(deleted) => {
                        let response = ResponsePacket::SessionRemoved {
                            request_id,
                            session_id,
                            deleted,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::SessionSwitch {
                request_id,
                agent,
                team,
                session_id,
                user,
            } => {
                let mut manager = crate::session::SessionManager::for_cli(
                    crate::common::paths::PathResolver::with_dirs(
                        state.config_dir.clone(),
                        state.data_dir.clone(),
                        state.cache_dir.clone(),
                    ),
                    &agent,
                    team.as_deref(),
                    &user,
                );
                let session_peer = crate::auth::principal::Principal::User(user);
                match manager.switch_session(&session_peer, &session_id).await {
                    Ok(_) => {
                        let response = ResponsePacket::SessionSwitched {
                            request_id,
                            session_id,
                            agent,
                            team: team.unwrap_or_else(|| "default".to_string()),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

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
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::SystemStatus { request_id } => {
                let response = ResponsePacket::SystemStatus {
                    request_id,
                    version: crate::VERSION.to_string(),
                    uptime_secs: state.uptime_seconds(),
                    degraded: state.is_degraded().await,
                    instance_count: state.instance_count().await,
                    team_count: state.team_count().await,
                    ready: state.is_ready().await,
                };
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::SystemDoctor { request_id } => {
                let mut checks = Vec::new();

                let ready = state.is_ready().await;
                checks.push(super::packet::DoctorCheck {
                    name: "daemon_ready".to_string(),
                    status: if ready {
                        "pass".to_string()
                    } else {
                        "fail".to_string()
                    },
                    message: if ready {
                        "Daemon is ready to serve requests".to_string()
                    } else {
                        "Daemon is not ready".to_string()
                    },
                    suggestion: if !ready {
                        Some("Check daemon logs for startup errors".to_string())
                    } else {
                        None
                    },
                });

                let degraded = state.is_degraded().await;
                checks.push(super::packet::DoctorCheck {
                    name: "not_degraded".to_string(),
                    status: if !degraded {
                        "pass".to_string()
                    } else {
                        "warn".to_string()
                    },
                    message: if !degraded {
                        "Daemon is operating normally".to_string()
                    } else {
                        "Daemon is in degraded mode".to_string()
                    },
                    suggestion: if degraded {
                        Some("Check resource usage and consider restarting".to_string())
                    } else {
                        None
                    },
                });

                let uptime = state.uptime_seconds();
                checks.push(super::packet::DoctorCheck {
                    name: "uptime".to_string(),
                    status: "pass".to_string(),
                    message: format!("Daemon uptime: {} seconds", uptime),
                    suggestion: None,
                });

                let passed = checks.iter().filter(|c| c.status == "pass").count() as u32;
                let failed = checks.iter().filter(|c| c.status == "fail").count() as u32;
                let warnings = checks.iter().filter(|c| c.status == "warn").count() as u32;

                let response = ResponsePacket::SystemDoctor {
                    request_id,
                    checks,
                    passed,
                    failed,
                    warnings,
                };
                Self::send_sink(sink, response).await?;
            }

            // ─── Extension CRUD (ADR-030 Tier 1) ────────────────────────────
            RequestPacket::ExtensionList {
                request_id,
                enabled_only: _,
                ext_type,
            } => {
                // Reload extensions from disk before listing. The
                // `peko agent pull` auto-ext-pull path runs in the
                // CLI process (not via IPC) — see
                // `ensure_extensions_for_agent` at
                // `src/commands/agent/handlers.rs:1308+` — so the
                // daemon's in-memory manager is out of date with
                // the on-disk extension storage. Re-reading from
                // disk on every list keeps the daemon's view in
                // sync with the CLI's writes. Phase D3 flow 5b is
                // the first end-to-end test that surfaced this
                // gap (test asserts on `peko ext list` after the
                // auto-ext-pull).
                {
                    let mut manager = state.extension_manager().write().await;
                    if let Err(e) = manager.load_all().await {
                        tracing::warn!("Failed to reload extensions on list: {e}");
                    }
                }
                let manager = state.extension_manager().read().await;
                let ext_services = state.extension_services();

                let installed = manager.list_extensions();
                let builtins = ext_services.list_builtin_extensions().await;

                let mut extensions = Vec::new();

                // Add builtins
                for b in &builtins {
                    extensions.push(super::packet::ExtensionSummary {
                        id: b.id.clone(),
                        name: b.name.clone(),
                        ext_type: b.ext_type.clone(),
                        version: "n/a".to_string(),
                        source: "built-in".to_string(),
                        enabled: b.enabled,
                        runtime: "n/a".to_string(),
                        description: String::new(),
                    });
                }

                // Add installed
                for ext in installed {
                    if let Some(ref t) = ext_type {
                        if &ext.extension_type != t {
                            continue;
                        }
                    }
                    extensions.push(super::packet::ExtensionSummary {
                        id: ext.manifest.id.0.clone(),
                        name: ext.manifest.name.clone(),
                        ext_type: ext.extension_type.clone(),
                        version: ext.manifest.version.clone(),
                        source: "installed".to_string(),
                        enabled: true,
                        runtime: "n/a".to_string(),
                        description: ext.manifest.description.clone(),
                    });
                }

                let total = extensions.len();
                let response = ResponsePacket::ExtensionList {
                    request_id,
                    extensions,
                    total,
                };
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::ExtensionEnable {
                request_id,
                id,
                target,
            } => {
                let is_builtin = crate::extensions::framework::adapters::builtin_tools::is_builtin_tool(&id)
                    || id.starts_with("builtin:");

                // Build canonical extension ID for whitelist entries.
                let canonical_id = if is_builtin {
                    if id.starts_with("builtin:") {
                        id.clone()
                    } else {
                        format!("builtin:tool:{id}")
                    }
                } else {
                    id.clone()
                };

                let result = match target {
                    None => {
                        // Global scope: enable extension at daemon level
                        let mut manager = state.extension_manager().write().await;
                        let ext_services = state.extension_services();
                        if is_builtin {
                            let capability = if id.starts_with("builtin:") {
                                id.splitn(3, ':').nth(2).unwrap_or(&id).to_string()
                            } else {
                                id.clone()
                            };
                            ext_services.enable_builtin_hooks(&capability).await;
                            Ok(format!(
                                "Built-in capability '{capability}' enabled globally"
                            ))
                        } else {
                            let ext_id = crate::extensions::framework::types::ExtensionId::new(&id);
                            match manager.enable(&ext_id).await {
                                Ok(()) => Ok(format!("Extension '{id}' enabled globally")),
                                Err(e) => Err(e),
                            }
                        }
                    }
                    Some(ref target_str) if target_str.contains('/') => {
                        // Legacy compound scope: "team/agent" — resolves to agent
                        let parts: Vec<&str> = target_str.split('/').collect();
                        let agent_name = if parts.len() == 2 {
                            parts[1]
                        } else {
                            target_str.as_str()
                        };
                        let config_service = state.config_service();
                        match config_service.enable_tool_sync(agent_name, &canonical_id) {
                            Ok(()) => Ok(format!(
                                "Extension '{canonical_id}' enabled for agent '{agent_name}'"
                            )),
                            Err(e) => {
                                Err(anyhow::anyhow!("Failed to enable extension for agent: {e}"))
                            }
                        }
                    }
                    Some(ref target_str) => {
                        let config_service = state.config_service();
                        // Under ADR-031, bare names are agent scope if the agent exists,
                        // otherwise team scope for backward compatibility.
                        let agent_config_path = state
                            .config_dir
                            .join("agents")
                            .join(target_str)
                            .join("config.toml");
                        if agent_config_path.exists() {
                            match config_service.enable_tool_sync(target_str, &canonical_id) {
                                Ok(()) => Ok(format!(
                                    "Extension '{canonical_id}' enabled for agent '{target_str}'"
                                )),
                                Err(e) => Err(anyhow::anyhow!(
                                    "Failed to enable extension for agent: {e}"
                                )),
                            }
                        } else {
                            match config_service.enable_tool_for_team(target_str, &canonical_id) {
                                Ok(count) => Ok(format!("Extension '{canonical_id}' enabled for {count} agent(s) in team '{target_str}'")),
                                Err(e) => Err(anyhow::anyhow!("Failed to enable extension for team: {e}")),
                            }
                        }
                    }
                };

                match result {
                    Ok(msg) => {
                        let response = ResponsePacket::ExtensionEnabled {
                            request_id,
                            id,
                            message: msg,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionDisable {
                request_id,
                id,
                target,
            } => {
                let is_builtin = crate::extensions::framework::adapters::builtin_tools::is_builtin_tool(&id)
                    || id.starts_with("builtin:");

                let canonical_id = if is_builtin {
                    if id.starts_with("builtin:") {
                        id.clone()
                    } else {
                        format!("builtin:tool:{id}")
                    }
                } else {
                    id.clone()
                };

                let result = match target {
                    None => {
                        // Global scope: disable extension at daemon level
                        let mut manager = state.extension_manager().write().await;
                        let ext_services = state.extension_services();
                        if is_builtin {
                            let capability = if id.starts_with("builtin:") {
                                id.splitn(3, ':').nth(2).unwrap_or(&id).to_string()
                            } else {
                                id.clone()
                            };
                            ext_services.disable_builtin_hooks(&capability).await;
                            Ok(format!(
                                "Built-in capability '{capability}' disabled globally"
                            ))
                        } else {
                            let ext_id = crate::extensions::framework::types::ExtensionId::new(&id);
                            match manager.disable(&ext_id).await {
                                Ok(()) => Ok(format!("Extension '{id}' disabled globally")),
                                Err(e) => Err(e),
                            }
                        }
                    }
                    Some(ref target_str) if target_str.contains('/') => {
                        // Legacy compound scope: "team/agent" — resolves to agent
                        let parts: Vec<&str> = target_str.split('/').collect();
                        let agent_name = if parts.len() == 2 {
                            parts[1]
                        } else {
                            target_str.as_str()
                        };
                        let config_service = state.config_service();
                        match config_service.disable_tool_sync(agent_name, &canonical_id) {
                            Ok(()) => Ok(format!(
                                "Extension '{canonical_id}' disabled for agent '{agent_name}'"
                            )),
                            Err(e) => Err(anyhow::anyhow!(
                                "Failed to disable extension for agent: {e}"
                            )),
                        }
                    }
                    Some(ref target_str) => {
                        let config_service = state.config_service();
                        // Under ADR-031, bare names are agent scope if the agent exists,
                        // otherwise team scope for backward compatibility.
                        let agent_config_path = state
                            .config_dir
                            .join("agents")
                            .join(target_str)
                            .join("config.toml");
                        if agent_config_path.exists() {
                            match config_service.disable_tool_sync(target_str, &canonical_id) {
                                Ok(()) => Ok(format!(
                                    "Extension '{canonical_id}' disabled for agent '{target_str}'"
                                )),
                                Err(e) => Err(anyhow::anyhow!(
                                    "Failed to disable extension for agent: {e}"
                                )),
                            }
                        } else {
                            match config_service.disable_tool_for_team(target_str, &canonical_id) {
                                Ok(count) => Ok(format!("Extension '{canonical_id}' disabled for {count} agent(s) in team '{target_str}'")),
                                Err(e) => Err(anyhow::anyhow!("Failed to disable extension for team: {e}")),
                            }
                        }
                    }
                };

                match result {
                    Ok(msg) => {
                        let response = ResponsePacket::ExtensionDisabled {
                            request_id,
                            id,
                            message: msg,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::SystemClean { request_id, scope } => {
                let cache_dir = &state.cache_dir;
                let mut cleaned = Vec::new();
                let mut bytes_freed: u64 = 0;

                let scope = scope.as_deref().unwrap_or("all");

                if scope == "all" || scope == "cache" {
                    if cache_dir.exists() {
                        match std::fs::read_dir(cache_dir) {
                            Ok(entries) => {
                                for entry in entries.flatten() {
                                    let path = entry.path();
                                    if let Ok(meta) = entry.metadata() {
                                        bytes_freed += meta.len();
                                    }
                                    if path.is_file() {
                                        let _ = std::fs::remove_file(&path);
                                        cleaned.push(path.to_string_lossy().to_string());
                                    } else if path.is_dir() {
                                        let _ = std::fs::remove_dir_all(&path);
                                        cleaned.push(path.to_string_lossy().to_string());
                                    }
                                }
                            }
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Failed to clean cache: {e}"),
                                };
                                Self::send_sink(sink, response).await?;
                                return Ok(());
                            }
                        }
                    }
                }

                let response = ResponsePacket::SystemCleaned {
                    request_id,
                    cleaned,
                    bytes_freed,
                };
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::CronAddSimple {
                request_id,
                name,
                schedule,
                message,
            } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => {
                        let _normalized = crate::cron::normalize_cron_expr(&schedule);
                        let schedule_kind = crate::cron::ScheduleKind::Cron {
                            expr: schedule.clone(),
                            tz: None,
                        };
                        let next_run = match crate::cron::calculate_next_run(
                            &schedule_kind,
                            chrono::Utc::now(),
                        ) {
                            Ok(t) => t,
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Invalid schedule: {e}"),
                                };
                                Self::send_sink(sink, response).await?;
                                return Ok(());
                            }
                        };
                        let job = crate::cron::CronJob {
                            id: format!("cron_{}", uuid::Uuid::new_v4().simple()),
                            name,
                            schedule: schedule_kind,
                            target: crate::cron::ExecutionTarget::Main,
                            agent_id: None,
                            message,
                            delivery: crate::cron::DeliveryMode::None,
                            delete_after_run: false,
                            enabled: true,
                            created_at: chrono::Utc::now(),
                            next_run,
                            last_run: None,
                            last_status: None,
                            run_count: 0,
                        };
                        match scheduler.add_job(&job) {
                            Ok(()) => {
                                let response = ResponsePacket::CronAddedSimple {
                                    request_id,
                                    job_id: job.id,
                                };
                                Self::send_sink(sink, response).await?;
                            }
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Failed to add job: {e}"),
                                };
                                Self::send_sink(sink, response).await?;
                            }
                        }
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::SessionBranch {
                request_id,
                agent,
                team,
                session_id,
                label,
            } => {
                let service = state.session_service();
                match service
                    .branch_session(&agent, team.as_deref(), &session_id, label)
                    .await
                {
                    Ok(result) => {
                        let response = ResponsePacket::SessionBranched {
                            request_id,
                            new_session_id: result.new_session_id,
                            parent_session_id: result.parent_session_id,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::SessionCompact {
                request_id,
                agent,
                team,
                session_id,
                dry_run,
                instruction,
            } => {
                let service = state.session_service();
                let sessions_dir = match service.get_sessions_dir(&agent, team.as_deref()).await {
                    Ok(d) => d,
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                        return Ok(());
                    }
                };
                if !sessions_dir.exists() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Agent '{agent}' not found"),
                    };
                    Self::send_sink(sink, response).await?;
                    return Ok(());
                }
                let mut session = match service
                    .open_session(&agent, team.as_deref(), &session_id, "default")
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                        return Ok(());
                    }
                };
                let compactor = crate::session::compaction::cli::SessionCompactor::new();
                if dry_run {
                    match compactor.dry_run(&session, instruction).await {
                        Ok(report) => {
                            let response = ResponsePacket::SessionCompactDryRun {
                                request_id,
                                session_id: session_id.clone(),
                                estimated_tokens: report.estimated_tokens,
                                context_window: report.context_window,
                                percent: report.percent,
                                message_count: report.message_count,
                                messages_to_compact: report.messages_to_compact,
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: e.to_string(),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    }
                } else {
                    match compactor.compact(&mut session, instruction).await {
                        Ok(result) => {
                            let response = ResponsePacket::SessionCompacted {
                                request_id,
                                session_id: session_id.clone(),
                                messages_compacted: result.entry.messages_compacted,
                                tokens_saved: result.tokens_saved,
                                tokens_before: result.entry.tokens_before,
                                tokens_after: result.entry.tokens_after,
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: e.to_string(),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    }
                }
            }

            RequestPacket::ExtensionInstall { request_id, path } => {
                let mut manager = state.extension_manager().write().await;
                let install_path =
                    match crate::commands::ext::prepare_install_path(std::path::Path::new(&path)) {
                        Ok(p) => p,
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to prepare extension for install: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                            return Ok(());
                        }
                    };

                match manager.install(&install_path).await {
                    Ok(ext_id) => {
                        let id = ext_id.0;
                        let response = ResponsePacket::ExtensionInstalled {
                            request_id,
                            id: id.clone(),
                            message: format!("Extension '{id}' installed successfully"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to install extension: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionUninstall { request_id, id } => {
                let mut manager = state.extension_manager().write().await;
                let ext_id = crate::extensions::framework::types::ExtensionId::new(&id);

                match manager.uninstall(&ext_id).await {
                    Ok(()) => {
                        let response = ResponsePacket::ExtensionUninstalled {
                            request_id,
                            id: id.clone(),
                            message: format!("Extension '{id}' uninstalled"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to uninstall extension: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionValidate {
                request_id,
                path,
                verbose,
                semantic,
            } => {
                let depth = if semantic {
                    crate::extensions::validation::ValidationDepth::Semantic
                } else {
                    crate::extensions::validation::ValidationDepth::Static
                };
                match crate::extensions::validation::ExtensionValidationService::validate_with_depth(
                    std::path::Path::new(&path),
                    verbose,
                    depth,
                )
                .await
                {
                    Ok(report) => {
                        let response = ResponsePacket::ExtensionValidated {
                            request_id,
                            valid: report.errors.is_empty(),
                            errors: report.errors,
                            warnings: report.warnings,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionDebug { request_id, id } => {
                let manager = state.extension_manager().read().await;
                let ext_id = crate::extensions::framework::types::ExtensionId::new(&id);
                match manager.get_extension(&ext_id) {
                    Some(ext) => {
                        let info = serde_json::json!({
                            "id": ext.manifest.id.0,
                            "name": ext.manifest.name,
                            "type": ext.extension_type,
                            "version": ext.manifest.version,
                            "path": ext.path.to_string_lossy().to_string(),
                            "hooks": ext.hook_ids.len(),
                        });
                        let response = ResponsePacket::ExtensionDebugInfo {
                            request_id,
                            id,
                            info,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Extension '{id}' not found"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionInfo { request_id, id } => {
                let manager = state.extension_manager().read().await;
                let ext_id = crate::extensions::framework::types::ExtensionId::new(&id);
                match manager.get_extension(&ext_id) {
                    Some(ext) => {
                        let info = serde_json::json!({
                            "id": ext.manifest.id.0,
                            "name": ext.manifest.name,
                            "type": ext.extension_type,
                            "version": ext.manifest.version,
                            "description": ext.manifest.description,
                        });
                        let response = ResponsePacket::ExtensionInfoResponse {
                            request_id,
                            id,
                            info,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Extension '{id}' not found"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionExport {
                request_id,
                id,
                output,
            } => {
                let manager = state.extension_manager().read().await;
                let ext_id = crate::extensions::framework::types::ExtensionId::new(&id);
                match crate::extensions::framework::manager::packaging::ExtensionPackager::export(
                    &manager, &ext_id, &output,
                ) {
                    Ok(_) => {
                        let response = ResponsePacket::ExtensionExported {
                            request_id,
                            id,
                            output,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionBundle {
                request_id,
                name,
                ids,
            } => {
                let manager = state.extension_manager().read().await;
                let ext_ids: Vec<_> = ids
                    .iter()
                    .map(|id| crate::extensions::framework::types::ExtensionId::new(id))
                    .collect();
                match manager.create_bundle(ext_ids, &name) {
                    Ok(bundle) => {
                        let response = ResponsePacket::ExtensionBundled {
                            request_id,
                            name,
                            count: bundle.extensions.len(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            RequestPacket::RegistryPull {
                request_id,
                registry_ref,
                team: _,
                force,
                registry_token,
                registry_host,
            } => {
                // Build registry config
                let host = registry_host.unwrap_or_else(|| {
                    crate::registry::client::RegistryRef::parse_with_default(
                        &registry_ref,
                        None,
                        Some(crate::registry::client::ResourceType::Agent),
                    )
                    .map(|r| r.host)
                    .unwrap_or_else(|_| "pekohub.org".to_string())
                });

                let mut config = crate::registry::config::load_from_workspace(&state.data_dir);

                // Add auth token if provided
                if let Some(token) = registry_token {
                    config.add_source(crate::registry::config::RegistrySource {
                        url: host.clone(),
                        priority: 1,
                        auth: None,
                        token: Some(token),
                    });
                }

                let agent_registry = crate::registry::AgentRegistry::new(
                    crate::registry::AgentRegistry::default_path(),
                );
                if let Err(e) = agent_registry.init().await {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Registry init failed: {e}"),
                    };
                    Self::send_sink(sink, response).await?;
                    return Ok(());
                }

                let client =
                    crate::registry::client::RegistryClient::new(config, agent_registry.clone());

                match client.pull(&registry_ref, |_| {}).await {
                    Ok(manifest) => {
                        // Export from registry to temp file
                        let tag = format!("{}:{}", manifest.name, manifest.version);
                        let temp_path = state.cache_dir.join(format!(
                            "peko-pull-{}-{}.agent",
                            manifest.name,
                            std::process::id()
                        ));

                        match agent_registry.export_package(&tag, &temp_path).await {
                            Ok(_) => {
                                // Import using AgentService
                                let service = state.agent_mgmt_service();
                                let import_opts = crate::common::types::agent::AgentImportOptions {
                                    name: None,
                                    force,
                                    // Registry pull path does not surface the
                                    // unsigned opt-in to the CLI; default to
                                    // false (secure by default).
                                    allow_unsigned: false,
                                };

                                match service.import_agent(&temp_path, import_opts).await {
                                    Ok(result) => {
                                        let _ = std::fs::remove_file(&temp_path);
                                        // Update host_runtime_id to current runtime
                                        let config_path = result.config_path.clone();
                                        if let Ok(content) =
                                            tokio::fs::read_to_string(&config_path).await
                                        {
                                            if let Ok(mut config) =
                                                toml::from_str::<crate::agents::agent_config::AgentConfig>(
                                                    &content,
                                                )
                                            {
                                                config.host_runtime_id =
                                                    state.runtime_identity().runtime_did.clone();
                                                if let Ok(updated) = toml::to_string_pretty(&config)
                                                {
                                                    let _ = tokio::fs::write(&config_path, updated)
                                                        .await;
                                                }
                                            }
                                        }
                                        let response = ResponsePacket::RegistryPulled {
                                            request_id,
                                            name: result.name,
                                            version: manifest.version.clone(),
                                            digest: manifest.digest.clone(),
                                        };
                                        Self::send_sink(sink, response).await?;
                                    }
                                    Err(e) => {
                                        let _ = std::fs::remove_file(&temp_path);
                                        let response = ResponsePacket::Error {
                                            request_id,
                                            message: format!("Import failed: {e}"),
                                        };
                                        Self::send_sink(sink, response).await?;
                                    }
                                }
                            }
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Export failed: {e}"),
                                };
                                Self::send_sink(sink, response).await?;
                            }
                        }
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Pull failed: {e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            // ── Runtime (ADR-032) ──
            RequestPacket::RuntimeId { request_id } => {
                let did = state.runtime_identity().runtime_did.clone();
                let response = ResponsePacket::RuntimeId { request_id, did };
                Self::send_sink(sink, response).await?;
            }
            RequestPacket::RuntimeInfo { request_id } => {
                let meta = state.runtime_metadata();
                let response = ResponsePacket::RuntimeInfo {
                    request_id,
                    metadata: super::packet::RuntimeMetadataResponse {
                        runtime_id: meta.runtime_id.clone(),
                        display_name: meta.display_name.clone(),
                        created_at: meta.created_at.to_rfc3339(),
                        last_seen_at: meta.last_seen_at.to_rfc3339(),
                        version: meta.version.clone(),
                        capabilities: meta.capabilities.clone(),
                        host_info: super::packet::HostInfoResponse {
                            os: meta.host_info.os.clone(),
                            arch: meta.host_info.arch.clone(),
                            hostname: meta.host_info.hostname.clone(),
                        },
                    },
                };
                Self::send_sink(sink, response).await?;
            }
            RequestPacket::RuntimeRename { request_id, .. } => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: "Runtime rename not yet implemented".to_string(),
                };
                Self::send_sink(sink, response).await?;
            }
            RequestPacket::RuntimeList { request_id } => {
                let registry = state.known_runtimes().read().await;
                let runtimes: Vec<super::packet::KnownRuntimeResponse> = registry
                    .list()
                    .iter()
                    .map(|r| super::packet::KnownRuntimeResponse {
                        runtime_id: r.runtime_id.clone(),
                        display_name: r.display_name.clone(),
                        last_seen: Some(r.last_seen.to_rfc3339()),
                        connection_endpoint: r.connection_endpoint.clone(),
                        trust_level: format!("{:?}", r.trust_level).to_lowercase(),
                    })
                    .collect();
                let response = ResponsePacket::RuntimeList {
                    request_id,
                    runtimes,
                };
                Self::send_sink(sink, response).await?;
            }
            RequestPacket::RuntimeRegister {
                request_id,
                runtime_id,
                display_name,
            } => {
                let mut registry = state.known_runtimes().write().await;
                registry.register(
                    &runtime_id,
                    &display_name,
                    None,
                    crate::tunnel::known_runtimes::TrustLevel::Untrusted,
                );
                let resolver = crate::common::paths::PathResolver::with_dirs(
                    state.config_dir.clone(),
                    state.data_dir.clone(),
                    state.cache_dir.clone(),
                );
                match registry.save(&resolver) {
                    Ok(()) => {
                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
            RequestPacket::RuntimeTrust {
                request_id,
                runtime_id,
            } => {
                let mut registry = state.known_runtimes().write().await;
                match registry.trust(
                    &runtime_id,
                    crate::tunnel::known_runtimes::TrustLevel::Authorized,
                ) {
                    Ok(()) => {
                        let resolver = crate::common::paths::PathResolver::with_dirs(
                            state.config_dir.clone(),
                            state.data_dir.clone(),
                            state.cache_dir.clone(),
                        );
                        let _ = registry.save(&resolver);
                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
            RequestPacket::RuntimeRemove {
                request_id,
                runtime_id,
            } => {
                let mut registry = state.known_runtimes().write().await;
                match registry.remove(&runtime_id) {
                    Ok(()) => {
                        let resolver = crate::common::paths::PathResolver::with_dirs(
                            state.config_dir.clone(),
                            state.data_dir.clone(),
                            state.cache_dir.clone(),
                        );
                        let _ = registry.save(&resolver);
                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }

            // ── Auth management (ADR-034) ──
            // API key management is restricted to local-trust (owner) for v0.1.0.
            RequestPacket::AuthApiKeyCreate {
                request_id,
                name,
                scopes,
            } => {
                if !caller.is_local() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key management requires local access".to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                } else if let Some(store) = state.api_key_store() {
                    let parsed_scopes: Vec<crate::auth::types::ApiKeyScope> =
                        scopes.iter().filter_map(|s| s.parse().ok()).collect();
                    match store.create_key(name, parsed_scopes).await {
                        Ok((full_key, key_id)) => {
                            let response = ResponsePacket::AuthApiKeyCreated {
                                request_id,
                                key_id,
                                full_key,
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: e.to_string(),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    }
                } else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key store not initialized".to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                }
            }
            RequestPacket::AuthApiKeyList { request_id } => {
                if !caller.is_local() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key management requires local access".to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                } else if let Some(store) = state.api_key_store() {
                    let keys = store.list_keys().await;
                    let summaries: Vec<super::packet::ApiKeySummary> = keys
                        .into_iter()
                        .map(|k| super::packet::ApiKeySummary {
                            id: k.id,
                            name: k.name,
                            created_at: k.created_at.to_rfc3339(),
                            last_used_at: k.last_used_at.map(|t| t.to_rfc3339()),
                            scopes: k.scopes.iter().map(|s| s.to_string()).collect(),
                            enabled: k.enabled,
                        })
                        .collect();
                    let response = ResponsePacket::AuthApiKeyList {
                        request_id,
                        keys: summaries,
                    };
                    Self::send_sink(sink, response).await?;
                } else {
                    let response = ResponsePacket::AuthApiKeyList {
                        request_id,
                        keys: Vec::new(),
                    };
                    Self::send_sink(sink, response).await?;
                }
            }
            RequestPacket::AuthApiKeyRevoke { request_id, key_id } => {
                if !caller.is_local() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key management requires local access".to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                } else if let Some(store) = state.api_key_store() {
                    match store.revoke_key(&key_id).await {
                        Ok(true) => {
                            let response = ResponsePacket::AuthApiKeyRevoked { request_id, key_id };
                            Self::send_sink(sink, response).await?;
                        }
                        Ok(false) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Key '{key_id}' not found"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: e.to_string(),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    }
                } else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key store not initialized".to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                }
            }
            RequestPacket::AuthStatus { request_id } => {
                let auth_config = state.auth_config();
                let api_key_count = if let Some(store) = state.api_key_store() {
                    store.list_keys().await.len()
                } else {
                    0
                };
                let response = ResponsePacket::AuthStatus {
                    request_id,
                    local_trust_enabled: auth_config.enable_local_trust(),
                    pekohub_jwt_enabled: auth_config.enable_pekohub_jwt(),
                    api_key_enabled: auth_config.enable_api_key(),
                    api_key_count,
                };
                Self::send_sink(sink, response).await?;
            }

            // ── Tunnel (ADR-035) ──
            RequestPacket::TunnelStop { request_id } => {
                state.stop_tunnel().await;
                let response = ResponsePacket::Done {
                    request_id,
                    success: true,
                    error: None,
                };
                Self::send_sink(sink, response).await?;
            }
            RequestPacket::TunnelStatus { request_id } => {
                let configured = crate::tunnel::credential::has_pekohub_credential();
                let connected = state.tunnel_connected().await;
                let response = ResponsePacket::TunnelStatus {
                    request_id,
                    configured,
                    daemon_running: true,
                    connected,
                };
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::Status { request_id } => {
                // Issue #8: comprehensive status including tunnel health.
                let health = state.tunnel_health().await;
                let degraded = state.is_degraded().await;
                let response = ResponsePacket::Status {
                    request_id,
                    uptime_secs: state.uptime_seconds(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    tunnel_state: health.state_str().to_string(),
                    tunnel_reconnect_attempts: health.reconnect_attempts(),
                    tunnel_last_error: health.last_error().map(str::to_string),
                    degraded,
                };
                Self::send_sink(sink, response).await?;
            }

            RequestPacket::InstanceSetStatus {
                request_id,
                agent_name,
                status,
            } => {
                let status_enum = match status.as_str() {
                    "online" => crate::tunnel::protocol::InstanceStatus::Online,
                    "offline" => crate::tunnel::protocol::InstanceStatus::Offline,
                    "busy" => crate::tunnel::protocol::InstanceStatus::Busy,
                    "error" => crate::tunnel::protocol::InstanceStatus::Error,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Invalid status '{other}'. Expected: online, offline, busy, error"),
                        };
                        Self::send_sink(sink, response).await?;
                        return Ok(());
                    }
                };

                if let Some(dispatcher) = state.tunnel_dispatcher().await {
                    match dispatcher.set_instance_status(&agent_name, status_enum).await {
                        Ok(()) => {
                            let response = ResponsePacket::Done {
                                request_id,
                                success: true,
                                error: None,
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to set instance status: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    }
                } else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "Tunnel is not active".to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                }
            }

            RequestPacket::InstanceSetExposure {
                request_id,
                agent_name,
                exposure,
            } => {
                let exposure_enum = match exposure.as_str() {
                    "unexposed" => crate::tunnel::protocol::InstanceExposure::Unexposed,
                    "private" => crate::tunnel::protocol::InstanceExposure::Private,
                    "public" => crate::tunnel::protocol::InstanceExposure::Public,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Invalid exposure '{other}'. Expected: unexposed, private, public"),
                        };
                        Self::send_sink(sink, response).await?;
                        return Ok(());
                    }
                };

                if let Some(dispatcher) = state.tunnel_dispatcher().await {
                    match dispatcher.set_instance_exposure(&agent_name, exposure_enum).await {
                        Ok(()) => {
                            let response = ResponsePacket::Done {
                                request_id,
                                success: true,
                                error: None,
                            };
                            Self::send_sink(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to set instance exposure: {e}"),
                            };
                            Self::send_sink(sink, response).await?;
                        }
                    }
                } else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "Tunnel is not active".to_string(),
                    };
                    Self::send_sink(sink, response).await?;
                }
            }

            // ── Ownership and Permission (ADR-033) ──
            RequestPacket::AgentTransferOwner {
                request_id,
                agent,
                new_owner_id,
            } => {
                let service = state.agent_mgmt_service();
                let caller_principal = caller.subject();
                match service
                    .transfer_agent_owner(&agent, &new_owner_id, &caller_principal)
                    .await
                {
                    Ok(()) => {
                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
            RequestPacket::AgentGrantPermission {
                request_id,
                agent,
                permission,
                ..
            } => {
                let subject = match take_resolved_subject(
                    pre_resolved_subject.as_ref(),
                    request_id,
                    sink,
                )
                .await
                {
                    Ok(s) => s,
                    Err(()) => return Ok(()),
                };
                let service = state.agent_mgmt_service();
                let caller_principal = caller.subject();
                let grant = crate::auth::ownership::PermissionGrant {
                    subject,
                    permission,
                    granted_at: chrono::Utc::now().to_rfc3339(),
                    granted_by: caller_principal.clone(),
                };
                match service
                    .grant_agent_permission(&agent, grant, &caller_principal)
                    .await
                {
                    Ok(()) => {
                        // Propagate the new `allowed_users` to PekoHub and
                        // refresh the runtime's defense-in-depth cache
                        // (issue #16). Best-effort: a tunnel outage does
                        // not fail the permit — the next `announce_instances`
                        // after `TunnelReady` will pick up the latest config.
                        if let Some(dispatcher) = state.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.refresh_instance_allowed_users(&agent).await
                            {
                                warn!(
                                    agent = %agent,
                                    "Failed to refresh allowed_users after grant: {e}"
                                );
                            }
                        }

                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
            RequestPacket::AgentRevokePermission {
                request_id,
                agent,
                permission,
                ..
            } => {
                let subject = match take_resolved_subject(
                    pre_resolved_subject.as_ref(),
                    request_id,
                    sink,
                )
                .await
                {
                    Ok(s) => s,
                    Err(()) => return Ok(()),
                };
                let service = state.agent_mgmt_service();
                let caller_principal = caller.subject();
                match service
                    .revoke_agent_permission(&agent, &subject, &permission, &caller_principal)
                    .await
                {
                    Ok(()) => {
                        // Symmetric to AgentGrantPermission — propagate the
                        // updated `allowed_users` to PekoHub so the revoked
                        // user loses access within ~1s, no daemon restart
                        // (issue #16). Best-effort; see note above.
                        if let Some(dispatcher) = state.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.refresh_instance_allowed_users(&agent).await
                            {
                                warn!(
                                    agent = %agent,
                                    "Failed to refresh allowed_users after revoke: {e}"
                                );
                            }
                        }

                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
            RequestPacket::TeamTransferOwner {
                request_id,
                team,
                new_owner_id,
            } => {
                let service = crate::common::services::TeamService::new(
                    state.team_service().resolver().clone(),
                );
                let caller_principal = caller.subject();
                match service
                    .transfer_team_owner(&team, &new_owner_id, &caller_principal)
                    .await
                {
                    Ok(()) => {
                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("{e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
            RequestPacket::TeamGrantPermission {
                request_id,
                team,
                permission,
                ..
            } => {
                let subject = match take_resolved_subject(
                    pre_resolved_subject.as_ref(),
                    request_id,
                    sink,
                )
                .await
                {
                    Ok(s) => s,
                    Err(()) => return Ok(()),
                };
                let service = crate::common::services::TeamService::new(
                    state.team_service().resolver().clone(),
                );
                let caller_principal = caller.subject();
                let grant = crate::auth::ownership::PermissionGrant {
                    subject,
                    permission,
                    granted_at: chrono::Utc::now().to_rfc3339(),
                    granted_by: caller_principal.clone(),
                };
                match service
                    .grant_team_permission(&team, grant, &caller_principal)
                    .await
                {
                    Ok(()) => {
                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("{e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
            RequestPacket::TeamRevokePermission {
                request_id,
                team,
                permission,
                ..
            } => {
                let subject = match take_resolved_subject(
                    pre_resolved_subject.as_ref(),
                    request_id,
                    sink,
                )
                .await
                {
                    Ok(s) => s,
                    Err(()) => return Ok(()),
                };
                let service = crate::common::services::TeamService::new(
                    state.team_service().resolver().clone(),
                );
                let caller_principal = caller.subject();
                match service
                    .revoke_team_permission(&team, &subject, &permission, &caller_principal)
                    .await
                {
                    Ok(()) => {
                        let response = ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        };
                        Self::send_sink(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("{e}"),
                        };
                        Self::send_sink(sink, response).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle an Execute request — run the agentic loop and stream responses
    async fn handle_execute(
        request_id: u64,
        agent: String,
        team: String,
        message: String,
        session_id: Option<String>,
        new_session: bool,
        stream_enabled: bool,
        user: String,
        state: AppState,
        sink: &dyn ResponseSink,
        peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        use crate::agents::stateless_service::MessageRequest;
        use crate::engine::{AgenticEvent, LifecyclePhase};

        tracing::info!(
            "IPC handle_execute started: request_id={}, agent={}, user={}, stream={}, session_id={:?}, new_session={}",
            request_id,
            agent,
            user,
            stream_enabled,
            session_id,
            new_session
        );

        let agent_service = state.agent_service().clone();

        let request = MessageRequest::new(&agent, message)
            .with_team(&team)
            .with_session_opt(session_id)
            .with_new_session(new_session)
            .with_user(&user);

        // Start the agentic loop — wrap in catch_unwind-like error handling
        // so the client always gets a response even if execution fails
        let mut event_stream = match agent_service.execute_message_streaming(request).await {
            Ok(stream) => stream,
            Err(e) => {
                let error_packet = ResponsePacket::Error {
                    request_id,
                    message: format!("Failed to start agent execution: {e}"),
                };
                Self::send_sink(sink, error_packet).await?;
                let done_packet = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(e.to_string()),
                };
                Self::send_sink(sink, done_packet).await?;
                return Ok(());
            }
        };

        // Stream events back as packets
        let mut seq = 0u32;
        let mut heartbeat = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        // Buffer for non-streaming mode: accumulate all text and send at the end
        let mut non_streaming_buffer = String::new();

        loop {
            info!("IPC: waiting for event...");
            tokio::select! {
                maybe_event = event_stream.receiver.recv() => {
                    info!("IPC: received event from channel: {:?}", maybe_event.is_some());
                    match maybe_event {
                        Some(event) => {
                            match event {
                                AgenticEvent::AssistantDelta { text, .. } => {
                                    if stream_enabled {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: text,
                                        };
                                        Self::send_sink(sink, packet).await?;
                                        seq += 1;
                                    } else {
                                        // Accumulate for non-streaming mode
                                        non_streaming_buffer.push_str(&text);
                                    }
                                }
                                AgenticEvent::AssistantText { text, .. } => {
                                    // Full block text (non-streaming mode)
                                    if stream_enabled {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: text,
                                        };
                                        Self::send_sink(sink, packet).await?;
                                        seq += 1;
                                    } else {
                                        non_streaming_buffer.push_str(&text);
                                    }
                                }
                                AgenticEvent::ToolStart { name, .. } => {
                                    if stream_enabled {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: format!("\n[Running tool: {}]\n", name),
                                        };
                                        Self::send_sink(sink, packet).await?;
                                        seq += 1;
                                    }
                                }
                                AgenticEvent::ToolEnd { result, success, .. } => {
                                    info!("IPC: received ToolEnd event, stream_enabled={}", stream_enabled);
                                    if stream_enabled {
                                        let output = if success {
                                            result.to_string()
                                        } else {
                                            format!("[Tool failed: {}]", result)
                                        };
                                        info!("Sending ToolEnd result to client: len={}, output={}", output.len(), output);
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: format!("\n[Tool result]: {}\n", output),
                                        };
                                        Self::send_sink(sink, packet).await?;
                                    }
                                }
                                AgenticEvent::Lifecycle { phase: LifecyclePhase::End, .. } => {
                                    // In non-streaming mode, send accumulated text before Done
                                    if !stream_enabled && !non_streaming_buffer.is_empty() {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: std::mem::take(&mut non_streaming_buffer),
                                        };
                                        Self::send_sink(sink, packet).await?;
                                    }
                                    let packet = ResponsePacket::Done {
                                        request_id,
                                        success: true,
                                        error: None,
                                    };
                                    Self::send_sink(sink, packet).await?;
                                    break;
                                }
                                AgenticEvent::Lifecycle { phase: LifecyclePhase::Error, error, .. } => {
                                    // In non-streaming mode, send accumulated text before Done (even on error)
                                    if !stream_enabled && !non_streaming_buffer.is_empty() {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: std::mem::take(&mut non_streaming_buffer),
                                        };
                                        Self::send_sink(sink, packet).await?;
                                    }
                                    let packet = ResponsePacket::Done {
                                        request_id,
                                        success: false,
                                        error,
                                    };
                                    Self::send_sink(sink, packet).await?;
                                    break;
                                }
                                _ => {
                                    // Ignore other events (Thinking, Status, Usage, etc.)
                                }
                            }
                        }
                        None => {
                            // In non-streaming mode, send accumulated text before Done
                            if !stream_enabled && !non_streaming_buffer.is_empty() {
                                let packet = ResponsePacket::Text {
                                    request_id,
                                    seq,
                                    chunk: std::mem::take(&mut non_streaming_buffer),
                                };
                                Self::send_sink(sink, packet).await?;
                            }
                            let packet = ResponsePacket::Done {
                                request_id,
                                success: true,
                                error: None,
                            };
                            Self::send_sink(sink, packet).await?;
                            break;
                        }
                    }
                }

                _ = heartbeat.tick() => {
                    let packet = ResponsePacket::Heartbeat { request_id };
                    Self::send_sink(sink, packet).await?;
                }
            }
        }

        Ok(())
    }

    /// Handle an AsyncSpawn request
    async fn handle_async_spawn(
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: std::path::PathBuf,
        state: AppState,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        use crate::extensions::framework::async_exec::executor::{AsyncTaskId, AsyncToolConfig};

        let tool_runtime = state.tool_runtime.clone();
        let executor = state.async_task_executor.clone();

        let config = AsyncToolConfig::default();
        let task_id = AsyncTaskId::new();

        let receipt = executor
            .execute(
                task_id,
                tool_name.clone(),
                params.clone(),
                session_key,
                config,
                move || {
                    let runtime = tool_runtime.clone();
                    let ws = workspace.clone();
                    let name = tool_name.clone();
                    let p = params.clone();
                    Box::pin(async move {
                        match runtime.execute_tool_with_workspace(&name, p, &ws).await {
                            Ok(value) => Ok(value),
                            Err(e) => Err(e),
                        }
                    })
                },
            )
            .await?;

        let response = ResponsePacket::AsyncReceipt {
            request_id,
            receipt,
        };
        Self::send_sink(sink, response).await?;

        Ok(())
    }

    /// Handle an AsyncCancel request
    async fn handle_async_cancel(
        request_id: u64,
        task_id: String,
        state: AppState,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let executor = state.async_task_executor.clone();
        let cancelled = executor.cancel(&task_id).await.unwrap_or(false);

        let response = ResponsePacket::Done {
            request_id,
            success: cancelled,
            error: if cancelled {
                None
            } else {
                Some(format!("Task {} not found or already completed", task_id))
            },
        };
        Self::send_sink(sink, response).await?;

        Ok(())
    }

    /// Handle an ExtStart request — start a background runtime for an extension
    async fn handle_ext_start(
        request_id: u64,
        extension_id: String,
        state: AppState,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let registry = state.runtime_starter_registry().clone();
        let ctx = state.starter_context();

        match registry.start(&extension_id, &ctx).await {
            Ok(()) => {
                let response = ResponsePacket::ExtStarted {
                    request_id,
                    extension_id,
                };
                Self::send_sink(sink, response).await?;
            }
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: e.to_string(),
                };
                Self::send_sink(sink, response).await?;
            }
        }

        Ok(())
    }

    /// Handle an ExtStop request
    async fn handle_ext_stop(
        request_id: u64,
        extension_id: String,
        state: AppState,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let registry = state.runtime_starter_registry().clone();
        let ctx = state.starter_context();

        match registry.stop(&extension_id, &ctx).await {
            Ok(()) => {
                let response = ResponsePacket::ExtStopped {
                    request_id,
                    extension_id,
                };
                Self::send_sink(sink, response).await?;
            }
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: e.to_string(),
                };
                Self::send_sink(sink, response).await?;
            }
        }

        Ok(())
    }

    /// Handle an ExtRestart request
    async fn handle_ext_restart(
        request_id: u64,
        extension_id: String,
        state: AppState,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let registry = state.runtime_starter_registry().clone();
        let ctx = state.starter_context();

        match registry.restart(&extension_id, &ctx).await {
            Ok(()) => {
                let response = ResponsePacket::ExtRestarted {
                    request_id,
                    extension_id,
                };
                Self::send_sink(sink, response).await?;
            }
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: e.to_string(),
                };
                Self::send_sink(sink, response).await?;
            }
        }

        Ok(())
    }

    /// Handle an ExtStatus request
    async fn handle_ext_status(
        request_id: u64,
        extension_id: String,
        state: AppState,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let manager = state.background_runtime_manager().clone();

        match manager.get_state(&extension_id).await {
            Some(runtime_state) => {
                // Also get summary for restart_count and last_error
                let summaries = manager.list().await;
                let summary = summaries.iter().find(|s| s.id == extension_id);
                let restart_count = summary.map(|s| s.restart_count).unwrap_or(0);
                let last_error = summary.and_then(|s| s.last_error.clone());

                let response = ResponsePacket::ExtStatus {
                    request_id,
                    extension_id,
                    state: runtime_state.to_string(),
                    restart_count,
                    last_error,
                };
                Self::send_sink(sink, response).await?;
            }
            None => {
                let response = ResponsePacket::ExtStatus {
                    request_id,
                    extension_id,
                    state: "not_found".to_string(),
                    restart_count: 0,
                    last_error: None,
                };
                Self::send_sink(sink, response).await?;
            }
        }

        Ok(())
    }

    /// Send a response packet back to the client via the per-request sink.
    ///
    /// Replaces the pre-ADR-038 `send_packet(&socket, packet, peer)` which
    /// needed the peer address explicitly. With `ResponseSink` the
    /// destination is captured once when the sink is built (Unix/UDP) or
    /// is the per-connection `NamedPipeServer` (Windows), so this helper
    /// only needs the packet.
    async fn send_sink(sink: &dyn ResponseSink, packet: ResponsePacket) -> anyhow::Result<()> {
        let bytes = packet.to_bytes()?;
        trace!("Sending response: {:?} ({} bytes)", packet, bytes.len());
        sink.send_bytes(&bytes).await?;
        Ok(())
    }
}
