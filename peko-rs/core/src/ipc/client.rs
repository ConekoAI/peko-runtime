//! Daemon Client — Packet Send/Receive Only
//!
//! Per SRP, this struct only sends `RequestPacket`s and receives
//! `ResponsePacket`s. Connection management (discovery, reconnection)
//! is handled by `ConnectionManager`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::trace;

use super::connection::{ConnectionHandle, ConnectionManager};
use super::packet::{
    AuthCredential, AuthenticatedRequest, PrincipalSendControlMode, RequestPacket, ResponsePacket,
};
use super::stream::{PacketStream, StreamRouter};

/// Client for communicating with the peko daemon
///
/// Thin wrapper around a `ConnectionHandle`. Sends requests, returns
/// response streams. No connection management, no retry logic.
///
/// When a [`credential`] is set (via [`DaemonClient::connect_with_service_token`]
/// or future per-SID auto-loaders), every outgoing request is wrapped
/// in an [`AuthenticatedRequest`] envelope so the daemon's strict
/// session-token gate sees the matching `(peer_sid, token)` pair.
/// Without a credential, requests are sent bare (legacy format) and
/// the daemon falls back to `AuthCredential::None` — which is
/// rejected by strict mode but accepted when
/// `auth_session_required=false`.
pub struct DaemonClient {
    conn: ConnectionHandle,
    router: StreamRouter,
    next_request_id: Arc<AtomicU64>,
    /// Optional credential attached to every outgoing request
    /// envelope. `None` means "send bare RequestPacket" (legacy
    /// format). Daemon-internal clients set this to
    /// `AuthCredential::SessionToken(<service-token>)`; CLI-side
    /// clients in strict mode set it to the per-SID token read from
    /// `~/.peko/run/auth-token-<sid>`.
    credential: Option<AuthCredential>,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("next_request_id", &self.next_request_id)
            .finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// Connect to the daemon.
    ///
    /// The CLI does NOT auto-start the daemon. Start it manually with:
    ///   peko daemon start
    ///
    /// If `auth_session_required=true` (the post-PR #2 default), the
    /// daemon will gate every non-`AuthSubmit` request behind the
    /// strict SID+token check. This `connect` call therefore loads
    /// `~/.peko/run/auth-token-<sid>` — written by `peko auth
    /// submit` — and attaches the `SessionToken` credential to every
    /// subsequent packet automatically.
    ///
    /// Legacy behavior (no token file, no token attached) is preserved
    /// for callers that opt out via `PEKO_AUTH_SESSION_REQUIRED=0` on
    /// the daemon side; the daemon will reject their requests with
    /// `[auth_required]` if strict mode is on.
    ///
    /// # Errors
    /// Returns error if daemon is not reachable
    pub async fn connect() -> anyhow::Result<Self> {
        let conn = ConnectionManager::connect().await?;
        let mut client = Self::with_connection(conn).await?;
        client.credential = load_session_token_for_current_sid();
        Ok(client)
    }

    /// Create a client with an existing connection
    ///
    /// # Errors
    /// Returns error if the connection cannot be cloned for the receiver
    pub async fn with_connection(conn: ConnectionHandle) -> anyhow::Result<Self> {
        let router = super::stream::spawn_receiver(conn.try_clone().await?);
        Ok(Self {
            conn,
            router,
            next_request_id: Arc::new(AtomicU64::new(1)),
            credential: None,
        })
    }

    /// Connect with an explicit `SessionToken` credential attached.
    ///
    /// Used by daemon-internal clients (cron adapter, runtime hosts,
    /// etc.) that need to authenticate against the daemon's own
    /// preauthorized SID under `auth_session_required=true`. The
    /// service token is generated at daemon startup (see
    /// `Daemon::run`'s setsid + service-token block) and preauthorized
    /// for the daemon's SID with `AuthSource::Service`.
    ///
    /// CLI-side callers should use [`DaemonClient::connect`]
    /// (legacy, no credential — step 4 will load the per-SID token
    /// file automatically).
    ///
    /// # Errors
    /// Returns error if daemon is unreachable.
    pub async fn connect_with_service_token(token: impl Into<String>) -> anyhow::Result<Self> {
        let conn = ConnectionManager::connect().await?;
        let mut client = Self::with_connection(conn).await?;
        client.credential = Some(AuthCredential::SessionToken(token.into()));
        Ok(client)
    }

    /// Generate a new unique request ID
    fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a request packet and return a stream for responses
    async fn send_request(&self, packet: RequestPacket) -> anyhow::Result<PacketStream> {
        let request_id = packet.request_id();
        let stream = self.router.register(request_id).await;

        // Wrap in `AuthenticatedRequest` envelope when a credential
        // is configured. Bare `RequestPacket` is the legacy wire
        // format (still accepted by the daemon's `from_bytes`
        // fallback) — used by callers that haven't been migrated to
        // session-token auth yet.
        let bytes = match &self.credential {
            Some(credential) => {
                let envelope = AuthenticatedRequest {
                    auth: super::packet::AuthHeader {
                        credential: credential.clone(),
                    },
                    packet,
                };
                envelope.to_bytes()?
            }
            None => packet.to_bytes()?,
        };
        trace!("Sending request {} ({} bytes)", request_id, bytes.len());
        self.conn.send(&bytes).await?;

        Ok(stream)
    }

    /// Execute an agent message — retired in audit C4.
    ///
    /// The legacy `Execute` path went through `StatelessAgentService`
    /// directly, bypassing `PrincipalManager` permission checks,
    /// session creation, and root-agent routing. All chat traffic is
    /// Spawn an async background task
    ///
    /// # Errors
    /// Returns error if the request cannot be sent
    pub async fn spawn_async_task(
        &self,
        tool_name: impl Into<String>,
        params: serde_json::Value,
        session_key: impl Into<String>,
        workspace: std::path::PathBuf,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::AsyncSpawn {
            request_id,
            tool_name: tool_name.into(),
            params,
            session_key: session_key.into(),
            workspace,
        };

        self.send_request(packet).await
    }

    /// Cancel an async task
    ///
    /// # Errors
    /// Returns error if the request cannot be sent
    pub async fn cancel_async_task(
        &self,
        task_id: impl Into<String>,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::AsyncCancel {
            request_id,
            task_id: task_id.into(),
        };

        self.send_request(packet).await
    }

    /// Ping the daemon to check if it's alive
    ///
    /// Returns the Pong response with uptime and version.
    ///
    /// # Errors
    /// Returns error if the ping fails or times out
    pub async fn ping(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::Ping { request_id };
        let mut stream = self.send_request(packet).await?;

        // Wait for the first (and only) response
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Ping stream closed unexpectedly"),
        }
    }

    /// Check if the daemon is running
    ///
    /// Returns `true` if the daemon responds to a ping within the timeout.
    pub async fn is_running(&self) -> bool {
        match self.ping().await {
            Ok(ResponsePacket::Pong { .. }) => true,
            _ => false,
        }
    }

    // ── Session-group IPC auth (ADR-045 PR #2 step 3) ───────────────
    //
    // The first-time enrollment flow is the bootstrap call: the CLI
    // has no token yet, so it bypasses every session-token gate on
    // the server side. The server verifies the diceware code, mints a
    // fresh session token, and returns it for the CLI to persist at
    // `~/.peko/run/auth-token-<sid>`.
    //
    // The server's strict gate sees the supplied `RequestPacket` as
    // a bare request (no envelope wrapping), parses it via
    // `AuthenticatedRequest::from_bytes` (which falls back to bare
    // `RequestPacket`), and dispatches inline — no SID lookup, no
    // token verification, no caller resolution.

    /// Submit the daemon's startup diceware code and receive a fresh
    /// session token.
    ///
    /// On success returns `ResponsePacket::AuthSubmitted { token,
    /// expires_in_secs, .. }`; the caller is responsible for persisting
    /// the token at the appropriate `~/.peko/run/auth-token-<sid>`
    /// path (mode 0600) and for attaching it as
    /// `AuthCredential::SessionToken` on every subsequent request
    /// (PR #2 step 4 wires that side).
    ///
    /// On failure returns `ResponsePacket::Error { message, .. }`
    /// with a bracket-prefixed code: `[invalid_auth_code]`,
    /// `[auth_code_expired]`, `[auth_code_exhausted]`,
    /// `[auth_code_consumed]`, or `[auth_code_not_initialized]`.
    pub async fn auth_submit(&self, code: impl Into<String>) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::AuthSubmit {
            request_id,
            code: code.into(),
        };
        self.request_response(packet).await
    }

    /// Send an `ApprovalDecision` for a pending self-modification
    /// request (ADR-045 PR #4).
    ///
    /// The daemon marks the queue with the user's choice and (on
    /// grant) executes the privileged op. Returns the
    /// `ApprovalDecided` envelope — the CLI renders it as a one-line
    /// summary plus the per-op `op_result`.
    pub async fn approval_decide(
        &self,
        id: uuid::Uuid,
        decision: crate::ipc::packet::ApprovalDecisionPayload,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::ApprovalDecision {
            request_id,
            id,
            decision,
        };
        self.request_response(packet).await
    }

    /// Send a request and wait for a single response
    ///
    /// This is the generic method used by all CRUD operations.
    /// The caller constructs the `RequestPacket` and receives the `ResponsePacket`.
    ///
    /// # Errors
    /// Returns error if send fails, stream closes unexpectedly, or response is an Error packet
    pub async fn request_response(&self, packet: RequestPacket) -> anyhow::Result<ResponsePacket> {
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(ResponsePacket::Error { message, .. }) => {
                anyhow::bail!(message)
            }
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Stream closed unexpectedly"),
        }
    }

    // ------------------------------------------------------------------
    // Model catalog management
    // ------------------------------------------------------------------

    /// Ask the daemon to re-read `models.toml` and `vault.enc`
    /// from disk. Used by `peko model {add,remove}` and
    /// `peko credential {set,delete}` after their on-disk writes
    /// succeed, so the long-running daemon observes CLI mutations
    /// without a restart.
    pub async fn reload_providers(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::ModelReload { request_id };
        self.request_response(packet).await
    }

    /// Ask the daemon to re-read `mcp.toml` and `vault.enc` from disk.
    /// Used by `peko ext mcp {add,auth,remove}` after on-disk writes succeed,
    /// so the long-running daemon observes CLI mutations without a restart.
    pub async fn mcp_reload(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::McpReload { request_id };
        self.request_response(packet).await
    }

    /// Live-ping the model identified by `id` with its bound credential
    /// material (or no material for local models like Ollama) and
    /// return the structured outcome (ok, message, latency_ms,
    /// http_status, model_used, tested_at). Powers
    /// `peko model test <id>` and the desktop Test button's path.
    pub async fn credential_test(&self, id: &str) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::ModelTest {
            request_id,
            id: id.to_string(),
        };
        self.request_response(packet).await
    }

    // ------------------------------------------------------------------
    // Cron management
    // ------------------------------------------------------------------

    /// List cron jobs
    pub async fn cron_list(
        &self,
        include_disabled: bool,
        principal: Option<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CronList {
            request_id,
            include_disabled,
            principal,
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Cron list stream closed unexpectedly"),
        }
    }

    /// Add a cron job
    pub async fn cron_add(&self, job: peko_cron::CronJob) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CronAdd { request_id, job };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Cron add stream closed unexpectedly"),
        }
    }

    /// Remove a cron job
    pub async fn cron_remove(&self, job_id: impl Into<String>) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CronRemove {
            request_id,
            job_id: job_id.into(),
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Cron remove stream closed unexpectedly"),
        }
    }

    /// Run a cron job immediately
    pub async fn cron_run(&self, job_id: impl Into<String>) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CronRun {
            request_id,
            job_id: job_id.into(),
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Cron run stream closed unexpectedly"),
        }
    }

    /// Get cron job run history
    pub async fn cron_history(
        &self,
        job_id: impl Into<String>,
        limit: usize,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CronHistory {
            request_id,
            job_id: job_id.into(),
            limit,
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Cron history stream closed unexpectedly"),
        }
    }

    /// Read a peer's conversation thread with a Principal (peko log).
    ///
    /// This is the read complement to `principal_send`. Pass `None` for
    /// `peer` to read the principal's owner-root view; pass a
    /// `Subject::User`/`Subject::Principal` to read that peer's thread
    /// (caller must equal peer or be the principal's owner — the daemon
    /// enforces this).
    pub async fn principal_log(
        &self,
        principal: impl Into<String>,
        peer: Option<peko_auth::Subject>,
        limit: Option<usize>,
        since_secs: Option<u64>,
        cursor: Option<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalLog {
            request_id,
            name: principal.into(),
            peer,
            limit,
            since_secs,
            cursor,
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("PrincipalLog stream closed unexpectedly"),
        }
    }

    // ------------------------------------------------------------------
    // Extension runtime lifecycle (ADR-026)
    // ------------------------------------------------------------------

    /// Start a background runtime for an extension
    pub async fn ext_start(
        &self,
        extension_id: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::ExtStart {
            request_id,
            extension_id: extension_id.into(),
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Ext start stream closed unexpectedly"),
        }
    }

    /// Stop a background runtime for an extension
    pub async fn ext_stop(
        &self,
        extension_id: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::ExtStop {
            request_id,
            extension_id: extension_id.into(),
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Ext stop stream closed unexpectedly"),
        }
    }

    /// Restart a background runtime for an extension
    pub async fn ext_restart(
        &self,
        extension_id: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::ExtRestart {
            request_id,
            extension_id: extension_id.into(),
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Ext restart stream closed unexpectedly"),
        }
    }

    /// Get background runtime status for an extension
    pub async fn ext_status(
        &self,
        extension_id: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::ExtStatus {
            request_id,
            extension_id: extension_id.into(),
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Ext status stream closed unexpectedly"),
        }
    }

    // ── Tunnel (ADR-035) ──

    /// Stop the PekoHub tunnel
    pub async fn tunnel_stop(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::TunnelStop { request_id };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Tunnel stop stream closed unexpectedly"),
        }
    }

    /// Get tunnel status
    pub async fn tunnel_status(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::TunnelStatus { request_id };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Tunnel status stream closed unexpectedly"),
        }
    }

    /// Comprehensive daemon status (issue #8). Returns uptime, version,
    /// and tunnel health snapshot.
    pub async fn status(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::Status { request_id };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Status stream closed unexpectedly"),
        }
    }

    // ── Auth management (ADR-034) ──

    /// Create an API key
    pub async fn auth_api_key_create(
        &self,
        name: impl Into<String>,
        scopes: Vec<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::AuthApiKeyCreate {
            request_id,
            name: name.into(),
            scopes,
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Auth API key create stream closed unexpectedly"),
        }
    }

    /// List API keys
    pub async fn auth_api_key_list(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::AuthApiKeyList { request_id };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Auth API key list stream closed unexpectedly"),
        }
    }

    /// Revoke an API key
    pub async fn auth_api_key_revoke(
        &self,
        key_id: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::AuthApiKeyRevoke {
            request_id,
            key_id: key_id.into(),
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Auth API key revoke stream closed unexpectedly"),
        }
    }

    /// Get auth status
    pub async fn auth_status(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::AuthStatus { request_id };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Auth status stream closed unexpectedly"),
        }
    }

    // ── Capability authority management ───────────────────────────────

    /// Grant a capability to a Principal.
    pub async fn capability_grant(
        &self,
        principal: impl Into<String>,
        capability: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CapabilityGrant {
            request_id,
            principal: principal.into(),
            capability: capability.into(),
        };
        self.request_response(packet).await
    }

    /// Revoke a capability from a Principal.
    pub async fn capability_revoke(
        &self,
        principal: impl Into<String>,
        capability: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CapabilityRevoke {
            request_id,
            principal: principal.into(),
            capability: capability.into(),
        };
        self.request_response(packet).await
    }

    /// List capabilities granted to a Principal.
    pub async fn capability_list(
        &self,
        principal: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::CapabilityList {
            request_id,
            principal: principal.into(),
        };
        self.request_response(packet).await
    }

    // ── Principal operations ─────────────────────────────────────────

    /// Send a message to a Principal and stream the response.
    ///
    /// The server returns a `PrincipalSent` response followed by `Done`.
    pub async fn principal_send(
        &self,
        name: impl Into<String>,
        message: impl Into<String>,
        user: impl Into<String>,
        no_slash: bool,
        output_format: crate::principal::runtime::OutputFormat,
        override_model: Option<String>,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalSend {
            request_id,
            name: name.into(),
            message: message.into(),
            user: user.into(),
            no_slash,
            output_format,
            override_model,
        };
        self.send_request(packet).await
    }

    /// Streaming variant of [`principal_send`]. The server emits zero
    /// or more `PrincipalSentChunk` deltas followed by a single
    /// `PrincipalSentDone` carrying the full final answer, then `Done`.
    pub async fn principal_send_stream(
        &self,
        name: impl Into<String>,
        message: impl Into<String>,
        user: impl Into<String>,
        no_slash: bool,
        output_format: crate::principal::runtime::OutputFormat,
        override_model: Option<String>,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalSendStream {
            request_id,
            name: name.into(),
            message: message.into(),
            user: user.into(),
            no_slash,
            output_format,
            override_model,
        };
        self.send_request(packet).await
    }

    /// Send a `PrincipalSendControl` (soft interrupt or steer) to the
    /// running stream identified by `target_request_id`. Returns the
    /// server's `ResponsePacket::Done` directly — the caller is
    /// expected to inspect `success`/`error` and surface a useful
    /// message to the user (the CLI's `peko interrupt` command does
    /// this).
    ///
    /// `target_request_id` is the `request_id` of the original
    /// `PrincipalSendStream` request, surfaced by `peko send --stream`
    /// on stderr at start.
    pub async fn principal_send_control(
        &self,
        target_request_id: u64,
        mode: PrincipalSendControlMode,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalSendControl {
            request_id,
            target_request_id,
            mode,
        };
        self.request_response(packet).await
    }

    /// Export a Principal to a package.
    pub async fn principal_export(
        &self,
        name: impl Into<String>,
        output: Option<String>,
        include_sessions: bool,
        with_extensions: bool,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalExport {
            request_id,
            name: name.into(),
            output,
            include_sessions,
            with_extensions,
        };
        self.request_response(packet).await
    }

    /// Preview a `.principal` package before importing it.
    pub async fn principal_import_preview(
        &self,
        file_path: impl Into<String>,
        name: Option<String>,
        allow_unsigned: bool,
        force: bool,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalImportPreview {
            request_id,
            file_path: file_path.into(),
            name,
            allow_unsigned,
            force,
        };
        self.request_response(packet).await
    }

    /// Import a Principal from a package.
    pub async fn principal_import(
        &self,
        file_path: impl Into<String>,
        name: Option<String>,
        allow_unsigned: bool,
        force: bool,
        confirmed: bool,
        selected_capabilities: Vec<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalImport {
            request_id,
            file_path: file_path.into(),
            name,
            allow_unsigned,
            force,
            confirmed,
            selected_capabilities,
        };
        self.request_response(packet).await
    }

    /// Push a Principal package to a registry.
    pub async fn principal_push(
        &self,
        name: impl Into<String>,
        registry_host: Option<String>,
        registry_token: Option<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalPush {
            request_id,
            name: name.into(),
            registry_host,
            registry_token,
        };
        self.request_response(packet).await
    }

    /// Preview a remote Principal package before pulling it.
    pub async fn principal_pull_preview(
        &self,
        registry_ref: impl Into<String>,
        name: Option<String>,
        force: bool,
        registry_host: Option<String>,
        registry_token: Option<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalPullPreview {
            request_id,
            registry_ref: registry_ref.into(),
            name,
            force,
            registry_host,
            registry_token,
        };
        self.request_response(packet).await
    }

    /// Pull a Principal package from a registry and import it.
    pub async fn principal_pull(
        &self,
        registry_ref: impl Into<String>,
        name: Option<String>,
        force: bool,
        confirmed: bool,
        selected_capabilities: Vec<String>,
        allow_unsigned: bool,
        registry_host: Option<String>,
        registry_token: Option<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalPull {
            request_id,
            registry_ref: registry_ref.into(),
            name,
            force,
            confirmed,
            selected_capabilities,
            allow_unsigned,
            registry_host,
            registry_token,
        };
        self.request_response(packet).await
    }

    /// Grant a permission on a Principal.
    pub async fn principal_grant_permission(
        &self,
        name: impl Into<String>,
        subject: peko_auth::Subject,
        permission: peko_auth::ownership::Permission,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalGrantPermission {
            request_id,
            name: name.into(),
            subject,
            permission,
        };
        self.request_response(packet).await
    }

    /// Revoke a permission from a Principal.
    pub async fn principal_revoke_permission(
        &self,
        name: impl Into<String>,
        subject: peko_auth::Subject,
        permission: peko_auth::ownership::Permission,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalRevokePermission {
            request_id,
            name: name.into(),
            subject,
            permission,
        };
        self.request_response(packet).await
    }

    /// List permissions on a Principal.
    pub async fn principal_permissions(
        &self,
        name: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalPermissions {
            request_id,
            name: name.into(),
        };
        self.request_response(packet).await
    }

    /// Set the tunnel status of a Principal's instance. Persisted on the
    /// Principal and broadcast to the hub.
    pub async fn principal_set_status(
        &self,
        name: impl Into<String>,
        status: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalSetStatus {
            request_id,
            name: name.into(),
            status: status.into(),
        };
        self.request_response(packet).await
    }

    /// Set the tunnel exposure of a Principal's instance. Persisted on
    /// the Principal and broadcast to the hub.
    pub async fn principal_set_exposure(
        &self,
        name: impl Into<String>,
        exposure: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalSetExposure {
            request_id,
            name: name.into(),
            exposure: exposure.into(),
        };
        self.request_response(packet).await
    }

    /// Mint a signed invite token for a Principal. Owner-only
    /// (requires `Permission::ManageSettings` on the principal).
    /// `ttl_secs` is bounded to 30 days by the daemon.
    pub async fn principal_mint_invite(
        &self,
        name: impl Into<String>,
        scope: Vec<peko_auth::ownership::Permission>,
        ttl_secs: u64,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalMintInvite {
            request_id,
            name: name.into(),
            scope,
            ttl_secs,
        };
        self.request_response(packet).await
    }

    /// Revoke a previously minted invite token. Owner-only.
    /// `jti` is the UUID printed in the mint response.
    pub async fn principal_revoke_invite(
        &self,
        name: impl Into<String>,
        jti: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalRevokeInvite {
            request_id,
            name: name.into(),
            jti: jti.into(),
        };
        self.request_response(packet).await
    }

    /// List all loaded Principals.
    pub async fn principal_list(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalList { request_id };
        self.request_response(packet).await
    }

    /// Look up a single Principal by name.
    pub async fn principal_get(&self, name: impl Into<String>) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalGet {
            request_id,
            name: name.into(),
        };
        self.request_response(packet).await
    }

    /// Create a new Principal on disk + in-memory manager.
    pub async fn principal_create(
        &self,
        name: impl Into<String>,
        description: Option<String>,
        model_id: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalCreate {
            request_id,
            name: name.into(),
            description,
            model_id: model_id.into(),
        };
        self.request_response(packet).await
    }

    /// Update an existing Principal's mutable config. All fields
    /// except `name` are optional; omitted fields keep their current
    /// values.
    pub async fn principal_update(
        &self,
        name: impl Into<String>,
        description: Option<String>,
        status: Option<String>,
        exposure: Option<String>,
        preferred_model_id: Option<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalUpdate {
            request_id,
            name: name.into(),
            description,
            status,
            exposure,
            preferred_model_id,
        };
        self.request_response(packet).await
    }

    /// Remove a Principal and delete its workspace/data.
    pub async fn principal_remove(
        &self,
        name: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalRemove {
            request_id,
            name: name.into(),
        };
        self.request_response(packet).await
    }

    // ------------------------------------------------------------------
    // Persona draft (Fix D — guided persona builder)
    // ------------------------------------------------------------------

    /// Ask the daemon to draft a persona from `from` using the
    /// configured `model_id`. The daemon validates the model id
    /// against its catalog, calls the LLM via
    /// `Provider::chat_with_system`, and returns the raw text in
    /// `ResponsePacket::PersonaDrafted { content, parse_ok }`. There
    /// is no session, no streaming, no memory — a single-shot draft.
    pub async fn persona_draft(
        &self,
        model_id: impl Into<String>,
        from: impl Into<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PersonaDraft {
            request_id,
            model_id: model_id.into(),
            from: from.into(),
        };
        self.request_response(packet).await
    }

    // ------------------------------------------------------------------
    // Quota management (F18)
    // ------------------------------------------------------------------

    /// Fetch a principal's live quota status (counters + window).
    ///
    /// F20: pass `is_peer: true` to fetch a peer's quota instead.
    pub async fn quota_get(
        &self,
        name: impl Into<String>,
        is_peer: bool,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::QuotaGet {
            request_id,
            name: name.into(),
            is_peer,
        };
        self.request_response(packet).await
    }

    /// Replace a principal's quota configuration. Persists to
    /// `PrincipalConfig` and updates the live meter so the next
    /// `charge` consults the new limits.
    ///
    /// F20: pass `is_peer: true` to set a peer's quota.
    pub async fn quota_set(
        &self,
        name: impl Into<String>,
        config: peko_quota::QuotaConfig,
        is_peer: bool,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::QuotaSet {
            request_id,
            name: name.into(),
            is_peer,
            config,
        };
        self.request_response(packet).await
    }

    /// Force-reset the meter's counters and roll a fresh window.
    ///
    /// F20: pass `is_peer: true` to reset a peer's quota.
    pub async fn quota_reset(
        &self,
        name: impl Into<String>,
        is_peer: bool,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::QuotaReset {
            request_id,
            name: name.into(),
            is_peer,
        };
        self.request_response(packet).await
    }
}

/// Load the per-SID session token from `~/.peko/run/auth-token-<sid>`.
///
/// Returns `None` if:
/// - The current process's SID cannot be determined (non-Unix).
/// - The token file does not exist (caller has not yet run
///   `peko auth submit`).
/// - The token file fails any of the safety checks (symlink, wrong
///   mode, wrong owner, empty, oversized).
///
/// This is the canonical entry point for `DaemonClient::connect` to
/// auto-attach credentials on every CLI invocation. It deliberately
/// never returns `Err` — a missing/malformed token file just means
/// "no credential" and is the legacy (pre-PR #2) behavior.
fn load_session_token_for_current_sid() -> Option<AuthCredential> {
    #[cfg(unix)]
    {
        let sid = super::peer_credentials::getsid_self()?;
        let resolver = default_path_resolver();
        let token_path = resolver.auth_token_file(sid);
        read_token_file(&token_path).map(AuthCredential::SessionToken)
    }
    #[cfg(not(unix))]
    {
        let _ = sid;
        None
    }
}

/// Read a session token from `path`, applying the safety checks
/// documented in PR #2 step 4:
///
/// - Must be a regular file (not a symlink).
/// - File mode must be exactly `0o600`. A token file with broader
///   permissions is treated as tampered and ignored.
/// - Owned by the current UID (best-effort check via `MetadataExt`).
/// - Non-empty after trim.
/// - ≤ 1024 bytes (defensive cap; session tokens are ~43 bytes).
#[cfg(unix)]
fn read_token_file(path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // Reject symlinks outright — they could redirect us to a
    // world-readable or attacker-controlled file.
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }

    // Mode must be exactly 0600.
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        return None;
    }

    // Owner must match the current UID (defense in depth: catches
    // tokens planted by a different user).
    let my_uid = unsafe { libc::getuid() };
    if meta.uid() != my_uid {
        return None;
    }

    // Read with permissive size cap. We already enforce symlink +
    // mode + owner sanity, so a cap is just paranoid backstop.
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() || trimmed.len() > 1024 {
        return None;
    }
    Some(trimmed)
}

/// Default `PathResolver` honoring `PEKO_HOME` and the XDG rules.
///
/// Mirrors `GlobalPaths::new` resolution so the CLI's auth-token
/// file lives under the same tree as everything else.
fn default_path_resolver() -> crate::common::paths::PathResolver {
    use crate::common::paths::{
        default_cache_dir, default_config_dir, default_data_dir,
    };

    let config_dir = default_config_dir();
    let data_dir = default_data_dir();
    let cache_dir = default_cache_dir();

    crate::common::paths::PathResolver::with_dirs(config_dir, data_dir, cache_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a running daemon. They are integration tests.
    // Unit tests for serialization are in packet.rs.

    #[test]
    fn test_next_id_monotonic() {
        // We can't easily test connect() without a daemon, but we can test
        // the request ID generation
        let counter = Arc::new(AtomicU64::new(1));
        assert_eq!(counter.fetch_add(1, Ordering::SeqCst), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
}
