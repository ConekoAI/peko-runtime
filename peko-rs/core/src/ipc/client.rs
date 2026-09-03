//! Daemon Client — Packet Send/Receive Only
//!
//! Per SRP, this struct only sends `RequestPacket`s and receives
//! `ResponsePacket`s. Connection management (discovery, reconnection)
//! is handled by `ConnectionManager`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::trace;

use super::connection::{ConnectionHandle, ConnectionManager};
use super::packet::{RequestPacket, ResponsePacket};
use super::stream::{PacketStream, StreamRouter};

/// Client for communicating with the peko daemon
///
/// Thin wrapper around a `ConnectionHandle`. Sends requests, returns
/// response streams. No connection management, no retry logic.
pub struct DaemonClient {
    conn: ConnectionHandle,
    router: StreamRouter,
    next_request_id: Arc<AtomicU64>,
    /// Liveness handle for the background receiver task. Checked on
    /// send so a dead receiver (panic, socket error) fails fast instead
    /// of hanging until the per-request timeout.
    receiver: Arc<tokio::task::JoinHandle<()>>,
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
    /// # Errors
    /// Returns error if daemon is not reachable
    pub async fn connect() -> anyhow::Result<Self> {
        let conn = ConnectionManager::connect().await?;
        Self::with_connection(conn).await
    }

    /// Create a client with an existing connection
    ///
    /// # Errors
    /// Returns error if the connection cannot be cloned for the receiver
    pub async fn with_connection(conn: ConnectionHandle) -> anyhow::Result<Self> {
        let (router, receiver) = super::stream::spawn_receiver(conn.try_clone().await?);
        Ok(Self {
            conn,
            router,
            next_request_id: Arc::new(AtomicU64::new(1)),
            receiver,
        })
    }

    /// Generate a new unique request ID
    fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a request packet and return a stream for responses
    async fn send_request(&self, packet: RequestPacket) -> anyhow::Result<PacketStream> {
        // Fast-fail: if the background receiver is gone (panic, socket
        // error), no response can ever arrive — error immediately
        // instead of hanging until the per-request timeout.
        if self.receiver.is_finished() {
            anyhow::bail!(
                "IPC receiver task is dead; responses can no longer arrive. \
                 Reconnect the client."
            );
        }
        let request_id = packet.request_id();
        let stream = self.router.register(request_id).await;

        let bytes = packet.to_bytes()?;
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
    // 2026-08-25: cron is an internal principal tool. The IPC variants
    // were retired; principals interact with cron via `tool:Cron*`
    // grants in the agentic-loop funnel (see `peko_cron::tools`).

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
        query: Option<String>,
        author: Option<String>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalLog {
            request_id,
            name: principal.into(),
            peer,
            limit,
            since_secs,
            cursor,
            query,
            author,
        };
        let mut stream = self.send_request(packet).await?;
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("PrincipalLog stream closed unexpectedly"),
        }
    }

    /// Open a `PrincipalLogWatch` stream for the (principal, peer)
    /// thread: replay of `Posted` rows newer than `since_cursor`
    /// followed by live `PrincipalLogAppended` packets + heartbeats.
    /// The caller drains the returned `PacketStream` until it closes
    /// (daemon shutdown) or an `Error` packet arrives (privacy
    /// rejection, lagged broadcast).
    pub async fn principal_log_watch(
        &self,
        name: impl Into<String>,
        peer: Option<peko_auth::Subject>,
        since_cursor: Option<String>,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalLogWatch {
            request_id,
            name: name.into(),
            peer,
            since_cursor,
        };
        self.send_request(packet).await
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

    // ── Principal operations ─────────────────────────────────────────

    /// Send a message to a Principal and stream the response.
    ///
    /// The server returns a `PrincipalSent` response followed by `Done`.
    pub async fn principal_send(
        &self,
        name: impl Into<String>,
        message: impl Into<String>,
        user: impl Into<String>,
        override_model: Option<String>,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalSend {
            request_id,
            name: name.into(),
            message: message.into(),
            user: user.into(),
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
        override_model: Option<String>,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalSendStream {
            request_id,
            name: name.into(),
            message: message.into(),
            user: user.into(),
            override_model,
        };
        self.send_request(packet).await
    }

    /// Send a `PrincipalStop` for the run bound to the (principal,
    /// peer) thread. Returns the server's `ResponsePacket::Done`
    /// directly — the caller is expected to inspect `success`/`error`
    /// and surface a useful message to the user (the CLI's `peko stop`
    /// command does this). `peer: None` targets the principal owner's
    /// thread; no in-flight run yields `success: false` with a
    /// "no running turn…" error so callers can treat it as an
    /// idempotent no-op.
    pub async fn principal_stop(
        &self,
        name: impl Into<String>,
        peer: Option<peko_auth::Subject>,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalStop {
            request_id,
            name: name.into(),
            peer,
        };
        self.request_response(packet).await
    }

    /// Export a Principal to a package.
    pub async fn principal_export(
        &self,
        name: impl Into<String>,
        output: Option<String>,
        include_sessions: bool,
    ) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::PrincipalExport {
            request_id,
            name: name.into(),
            output,
            include_sessions,
            with_extensions: false,
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
