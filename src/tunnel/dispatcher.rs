//! Tunnel Request Dispatcher
//!
//! Bridges proxied requests from the PekoHub tunnel to the daemon's service layer.
//! Handles chat execution, streaming responses, and instance lifecycle messages.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Namespace UUID for generating stable instance IDs from (runtime_did, agent_name).
/// This is a fixed UUIDv4 that acts as the namespace for UUIDv5 generation.
const INSTANCE_ID_NAMESPACE: uuid::Uuid = uuid::uuid!("a1b2c3d4-e5f6-47a8-b9c0-d1e2f3a4b5c6");

use crate::auth::Principal;
use crate::daemon::state::AppState;
use crate::engine::AgenticEvent;
use crate::common::types::a2a::A2aMessageRequest;

use super::a2a_audit;
use super::protocol::{
    ExposureUpdatePayload, InstanceAnnouncePayload, InstanceExposure, InstanceHeartbeatPayload,
    InstanceStatus, InstanceType, StatusUpdatePayload, TunnelMessage,
};
use super::TunnelHandle;
use super::{
    a2a_signature::{verify_request, SignedFields},
    did_key::did_key_to_verifying_key,
};
use crate::tunnel::a2a_send_tool::{A2aSendResult, HubA2AErrorResponse};

use crate::auth::ownership::Permission;

/// Resolve the calling user from a PekoHub-proxied bridge payload (issue #17).
///
/// PekoHub is the security boundary: it must set `Authorization: Bearer <jwt>`
/// on every proxied request. When a JWT is present and a `JwtValidator` is
/// configured, this function validates the JWT (signature, audience,
/// issuer, expiry) and uses the validated `sub` claim as the caller.
/// Cross-checks the validated `sub` against `x-pekohub-user-id` and warns
/// if they disagree — a mismatch means PekoHub is asserting one user in
/// the header while the JWT proves a different one, which is a tamper
/// attempt worth surfacing.
///
/// Falls back to the unverified `x-pekohub-user-id` header only when no
/// JWT is present (back-compat with deployments that haven't enabled
/// pekohub JWT validation yet) or when JWT validation fails. In both
/// cases the caller is logged with `(unverified)` so downstream audit
/// consumers know the identity wasn't cryptographically checked.
///
/// Returns `"anonymous"` when neither a JWT nor a `x-pekohub-user-id`
/// header is present or usable — never the literal `"web"` that the
/// dispatcher used to hard-code.
pub(crate) async fn resolve_bridge_caller(
    bridge_payload: &serde_json::Value,
    jwt_validator: Option<&crate::auth::jwt::JwtValidator>,
) -> String {
    // 1. Try the signed JWT first.
    if let Some(jwt) = extract_bearer_jwt(bridge_payload) {
        if let Some(validator) = jwt_validator {
            match validator.validate(&jwt).await {
                Ok(validated) => {
                    // Cross-check the JWT sub against the hub-asserted header
                    // so a tampered hub that emits `Authorization: ...subA...
                    // x-pekohub-user-id: subB` is surfaced, not silently trusted.
                    if let Some(hub_user) = header_user(bridge_payload) {
                        if hub_user != validated.sub {
                            warn!(
                                "JWT sub ({}) does not match x-pekohub-user-id ({}); \
                                 possible tamper — trusting the JWT",
                                validated.sub, hub_user
                            );
                        }
                    }
                    return validated.sub;
                }
                Err(e) => {
                    warn!(
                        "JWT validation failed ({}); falling back to x-pekohub-user-id header \
                         (unverified)",
                        e
                    );
                }
            }
        } else {
            debug!(
                "Authorization: Bearer <jwt> present but no JWT validator configured; \
                 falling back to x-pekohub-user-id header (unverified)"
            );
        }
    }

    // 2. Fall back to the unverified hub-asserted header.
    header_user(bridge_payload).unwrap_or_else(|| "anonymous".to_string())
}

/// Pull the unverified `x-pekohub-user-id` header out of the bridge payload.
fn header_user(bridge_payload: &serde_json::Value) -> Option<String> {
    bridge_payload
        .get("headers")
        .and_then(|h| h.get("x-pekohub-user-id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Extract the `Authorization: Bearer <jwt>` value from the bridge payload.
///
/// Header keys are case-insensitive per RFC 7230, so we lowercase before
/// lookup. The token is anything after `Bearer ` (single space per RFC
/// 6750 §2.1) with leading/trailing whitespace trimmed.
fn extract_bearer_jwt(bridge_payload: &serde_json::Value) -> Option<String> {
    let headers = bridge_payload.get("headers")?.as_object()?;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("authorization") {
            let raw = v.as_str()?.trim();
            let rest = raw
                .strip_prefix("Bearer")
                .or_else(|| raw.strip_prefix("bearer"))?
                .trim_start();
            // RFC 6750 §2.1: optional single space after scheme.
            let token = rest.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Per-instance state tracked by the dispatcher.
#[derive(Debug, Clone)]
pub struct InstanceState {
    /// Current exposure level
    pub exposure: InstanceExposure,
    /// Allowed user IDs (for private exposure)
    pub allowed_users: Vec<String>,
    /// Current instance status
    pub status: InstanceStatus,
}

impl Default for InstanceState {
    fn default() -> Self {
        Self {
            exposure: InstanceExposure::Private,
            allowed_users: Vec::new(),
            status: InstanceStatus::Online,
        }
    }
}

/// Shared dispatcher state for instance lifecycle management
#[derive(Debug, Default)]
pub struct TunnelDispatcherState {
    /// Whether the tunnel is authenticated and ready
    pub ready: bool,
    /// Heartbeat interval from server (seconds)
    pub heartbeat_interval_secs: u32,
    /// Local instance state cache: instance_id -> state (updated by exposure_update and status_update)
    pub instance_state: std::collections::HashMap<String, InstanceState>,
    /// Current tunnel handle for sending messages
    pub tunnel_handle: Option<TunnelHandle>,
}

/// Dispatches tunnel messages to daemon services.
///
/// `Clone` is a shallow copy: the `Arc<RwLock<...>>` is shared, which is
/// intentional because the dispatcher is moved into spawned tasks.
#[derive(Clone)]
pub struct TunnelDispatcher {
    app_state: AppState,
    state: Arc<RwLock<TunnelDispatcherState>>,
    runtime_display_name: String,
    /// Slot the dispatcher writes the live tunnel handle to on
    /// every inbound message. The `CrossRuntimeA2aCtx` (issue #29)
    /// holds a clone of this `Arc` and reads the handle when
    /// sending outbound `AgentToAgentRequest` envelopes, so the
    /// outbound path always uses the most-recent handle without
    /// having to be re-built on reconnect. `None` until the first
    /// inbound message lands.
    tunnel_handle_slot: Arc<tokio::sync::RwLock<Option<TunnelHandle>>>,
}

impl TunnelDispatcher {
    /// Create a new tunnel dispatcher bound to the daemon's AppState
    pub fn new(app_state: AppState) -> Self {
        // Pull the tunnel handle slot FIRST (clones the `Arc`,
        // not the handle) so the field move into the struct
        // below is the final use of `app_state`.
        let tunnel_handle_slot = app_state.tunnel_handle_slot();
        let runtime_display_name = app_state.runtime_metadata().display_name.clone();
        Self {
            app_state,
            state: Arc::new(RwLock::new(TunnelDispatcherState::default())),
            runtime_display_name,
            // Mirror the AppState's slot so the dispatcher publishes
            // and the ctx reads from the SAME `Arc<RwLock<...>>`. The
            // getter on AppState (`tunnel_handle_slot`) returns the
            // exact `Arc`; cloning it here is a no-op refcount bump.
            tunnel_handle_slot,
        }
    }

    /// Handle a tunnel message (called from the tunnel client's read loop)
    pub async fn handle_message(&self, msg: TunnelMessage, handle: TunnelHandle) {
        // Store the handle synchronously so set_instance_status can use it immediately
        {
            let mut state = self.state.write().await;
            state.tunnel_handle = Some(handle.clone());
        }
        // Publish the live handle to the AppState slot so the
        // outbound `CrossRuntimeA2aCtx` can send on the most-recent
        // tunnel. Doing this synchronously (not under the spawn
        // boundary) means the ctx sees the new handle before any
        // a2a call started by the inbound message could race.
        {
            let mut slot = self.tunnel_handle_slot.write().await;
            *slot = Some(handle.clone());
        }
        let dispatcher = self.clone();
        tokio::spawn(async move {
            if let Err(e) = dispatcher.dispatch(msg, handle).await {
                error!("Tunnel dispatch error: {}", e);
            }
        });
    }

    /// Mark the tunnel as ready (called after TunnelReady received)
    pub async fn mark_ready(&self, heartbeat_interval_secs: u32) {
        let mut state = self.state.write().await;
        state.ready = true;
        state.heartbeat_interval_secs = heartbeat_interval_secs;
        info!(
            "Tunnel dispatcher ready, heartbeat interval: {}s",
            heartbeat_interval_secs
        );
    }

    /// Mark the tunnel as disconnected
    pub async fn mark_disconnected(&self) {
        let mut state = self.state.write().await;
        state.ready = false;
        // Clear instance state cache to prevent stale data on reconnect
        state.instance_state.clear();
        info!("Tunnel dispatcher disconnected, instance state cache cleared");
    }

    /// Check if the tunnel is ready
    pub async fn is_ready(&self) -> bool {
        self.state.read().await.ready
    }

    /// Generate a stable instance ID from runtime DID and agent name.
    fn instance_id(&self, agent_name: &str) -> String {
        let name = format!(
            "{}:{}",
            self.app_state.runtime_identity().runtime_did,
            agent_name
        );
        uuid::Uuid::new_v5(&INSTANCE_ID_NAMESPACE, name.as_bytes()).to_string()
    }

    /// Compute allowed user IDs from an agent's permission grants.
    ///
    /// Filters for `Chat` permission grants where `subject` is a `User`
    /// principal, returning the bare user id (with `user:` prefix
    /// stripped if present). Non-User subjects (Agent/Team/Public) are
    /// filtered out — they cannot be expressed as a hub user_id.
    ///
    /// TODO(#16): re-derive from `Principal::User` and surface
    /// `Principal::Agent` subjects to the hub once PekoHub accepts the
    /// agent principal (post #11).
    fn compute_allowed_user_ids(config: &crate::agents::agent_config::AgentConfig) -> Option<Vec<String>> {
        use crate::auth::principal::{Principal, SubjectKind};
        let ids: Vec<String> = config
            .permissions
            .iter()
            .filter(|g| {
                g.permission.covers(&Permission::Chat) && g.subject.kind() == SubjectKind::User
            })
            .filter_map(|g| match &g.subject {
                Principal::User(id) => {
                    // Strip `user:` prefix if present; hub expects bare user IDs
                    Some(
                        id.strip_prefix("user:")
                            .map(String::from)
                            .unwrap_or_else(|| id.clone()),
                    )
                }
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            None
        } else {
            Some(ids)
        }
    }

    /// Send initial instance announcements for all local agents
    pub async fn announce_instances(&self, handle: &TunnelHandle) -> anyhow::Result<()> {
        let agent_service = self.app_state.agent_mgmt_service();
        let agents = match agent_service.list_agents(None).await {
            Ok(agents) => agents,
            Err(e) => {
                warn!("Failed to list agents for announce: {}", e);
                return Ok(());
            }
        };

        for agent in agents {
            let instance_id = self.instance_id(&agent.name);
            let allowed_users = Self::compute_allowed_user_ids(&agent.config);
            let payload = InstanceAnnouncePayload {
                id: instance_id.clone(),
                instance_type: InstanceType::Agent,
                name: agent.name.clone(),
                agent_did: agent.config.agent_did.clone(),
                bundle_ref: None,
                runtime_display_name: Some(self.runtime_display_name.clone()),
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_users: allowed_users.clone(),
                capabilities: None,
                metadata: None,
            };

            // Seed local instance state cache with default Online status and Private exposure
            let mut state = self.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    allowed_users: allowed_users.unwrap_or_default(),
                    status: InstanceStatus::Online,
                },
            );
            drop(state);

            if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce { payload }) {
                warn!("Failed to announce instance {}: {}", agent.name, e);
            } else {
                debug!("Announced instance: {}", agent.name);
            }
        }

        Ok(())
    }

    /// Announce a single agent instance through the tunnel.
    ///
    /// Used when a new agent is created after the tunnel is already connected.
    pub async fn announce_single_instance(&self, agent_name: &str) -> anyhow::Result<()> {
        let handle = {
            let state = self.state.read().await;
            match state.tunnel_handle.clone() {
                Some(h) => h,
                None => {
                    debug!(
                        "No tunnel handle available; skipping instance announce for {}",
                        agent_name
                    );
                    return Ok(());
                }
            }
        };

        let agent_service = self.app_state.agent_mgmt_service();
        let agent = match agent_service.get_agent(agent_name, None).await {
            Ok(Some(info)) => info,
            Ok(None) => {
                warn!("Agent {} not found; cannot announce instance", agent_name);
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "Failed to load agent {} for instance announce: {}",
                    agent_name, e
                );
                return Ok(());
            }
        };

        let instance_id = self.instance_id(agent_name);
        let allowed_users = Self::compute_allowed_user_ids(&agent.config);
        let payload = InstanceAnnouncePayload {
            id: instance_id.clone(),
            instance_type: InstanceType::Agent,
            name: agent.name.clone(),
            agent_did: agent.config.agent_did.clone(),
            bundle_ref: None,
            runtime_display_name: Some(self.runtime_display_name.clone()),
            status: InstanceStatus::Online,
            exposure: InstanceExposure::Private,
            allowed_users: allowed_users.clone(),
            capabilities: None,
            metadata: None,
        };

        // Seed local instance state cache
        let mut state = self.state.write().await;
        state.instance_state.insert(
            instance_id,
            InstanceState {
                exposure: InstanceExposure::Private,
                allowed_users: allowed_users.unwrap_or_default(),
                status: InstanceStatus::Online,
            },
        );
        drop(state);

        if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce { payload }) {
            warn!("Failed to announce instance {}: {}", agent_name, e);
        } else {
            debug!("Announced single instance: {}", agent_name);
        }

        Ok(())
    }

    /// Start periodic instance heartbeat task
    pub fn spawn_heartbeat_task(&self, handle: TunnelHandle) -> tokio::task::JoinHandle<()> {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if !dispatcher.is_ready().await {
                    continue;
                }
                if let Err(e) = dispatcher.send_heartbeats(&handle).await {
                    warn!("Instance heartbeat error: {}", e);
                }
            }
        })
    }

    async fn send_heartbeats(&self, handle: &TunnelHandle) -> anyhow::Result<()> {
        let agent_service = self.app_state.agent_mgmt_service();
        let agents = agent_service.list_agents(None).await?;

        let now = chrono::Utc::now().to_rfc3339();
        for agent in agents {
            let instance_id = self.instance_id(&agent.name);
            let status = self.get_instance_status(&agent.name).await;
            let payload = InstanceHeartbeatPayload {
                id: instance_id,
                status,
                timestamp: now.clone(),
            };
            let _ = handle.send(TunnelMessage::InstanceHeartbeat { payload });
        }
        Ok(())
    }

    /// Main dispatch method
    async fn dispatch(&self, msg: TunnelMessage, handle: TunnelHandle) -> anyhow::Result<()> {
        match msg {
            TunnelMessage::ProxiedRequest {
                request_id,
                agent,
                payload,
            } => {
                self.handle_proxied_request(request_id, agent, payload, handle)
                    .await?;
            }
            TunnelMessage::ExposureUpdate { payload } => {
                self.handle_exposure_update(payload).await?;
            }
            TunnelMessage::StatusUpdate { payload } => {
                self.handle_status_update(payload).await?;
            }
            TunnelMessage::TunnelReady {
                heartbeat_interval_secs,
            } => {
                self.mark_ready(heartbeat_interval_secs).await;
                // Announce all instances after auth
                if let Err(e) = self.announce_instances(&handle).await {
                    warn!("Failed to announce instances: {}", e);
                }
            }
            TunnelMessage::Disconnect { reason } => {
                info!("Tunnel disconnect: {}", reason);
                self.mark_disconnected().await;
            }
            // Issue #29 (Slice C): inbound `AgentToAgentRequest` from
            // a peer runtime (proxied by pekohub). Verify the caller's
            // signature against the `caller_runtime_id` they claim,
            // look up the local agent by `target_agent_did`,
            // attribute the dispatch under
            // `Principal::Agent(caller_agent_did)`, run it, and send
            // back an `AgentToAgentResponse` carrying the
            // `A2aSendResult` payload.
            TunnelMessage::AgentToAgentRequest {
                request_id,
                caller_runtime_id,
                caller_agent_did,
                target_agent_did,
                session_id,
                message,
                team,
                signature,
            } => {
                self.handle_inbound_agent_to_agent_request(
                    handle,
                    request_id,
                    caller_runtime_id,
                    caller_agent_did,
                    target_agent_did,
                    session_id,
                    message,
                    team,
                    signature,
                )
                .await?;
            }
            // Inbound `AgentToAgentResponse` for a request the
            // outbound `A2aSendTool` path registered in the pending
            // registry. Complete the oneshot so the outbound
            // `execute_remote` unblocks and decodes the payload.
            TunnelMessage::AgentToAgentResponse { request_id, payload } => {
                self.handle_inbound_agent_to_agent_response(request_id, payload)
                    .await?;
            }
            _ => {
                debug!("Ignoring tunnel message: {:?}", msg);
            }
        }
        Ok(())
    }

    /// Handle a proxied request from PekoHub
    async fn handle_proxied_request(
        &self,
        request_id: String,
        agent_name: String,
        payload: Vec<u8>,
        handle: TunnelHandle,
    ) -> anyhow::Result<()> {
        debug!(
            "Handling proxied request {} for agent {}",
            request_id, agent_name
        );

        // Parse the HTTP bridge payload from PekoHub
        let bridge_payload: serde_json::Value = match serde_json::from_slice(&payload) {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse proxied request payload: {}", e);
                return self
                    .send_error_response(&handle, &request_id, "Invalid request payload")
                    .await;
            }
        };

        // Defense-in-depth: enforce local ACL even though PekoHub already checked
        if let Err(e) = self
            .check_request_allowed(&agent_name, &bridge_payload)
            .await
        {
            warn!("Tunnel ACL denied request for {}: {}", agent_name, e);
            return self
                .send_error_response(&handle, &request_id, &format!("Forbidden: {}", e))
                .await;
        }

        // Extract message from the bridge payload
        let message = bridge_payload
            .get("body")
            .and_then(|b| b.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        if message.is_empty() {
            return self
                .send_error_response(&handle, &request_id, "Empty message")
                .await;
        }

        // Resolve the calling user from the PekoHub-proxied headers/JWT.
        // `resolve_bridge_caller` prefers a signed JWT (if PekoHub sent
        // `Authorization: Bearer <jwt>` and a `JwtValidator` is
        // configured) over the unverified `x-pekohub-user-id` header —
        // see issue #17 acceptance criteria ("src/auth/jwt.rs pekohub
        // JWT validation is enabled ... and unit-tested").
        let caller_user =
            resolve_bridge_caller(&bridge_payload, self.app_state.jwt_validator().as_ref()).await;

        // Audit: record the proxied request with the resolved caller so the
        // event stream is attributable to a real user, not the literal
        // `"web"` placeholder that this dispatcher used to stamp on every
        // request (issue #17). The caller is projected to a typed
        // `Principal` (issue #26) so the audit wire shape is `{kind, id}`
        // and per-user / per-agent queries can index on the kind tag.
        // `Principal::from_bridge_user` centralizes the `user:` prefix
        // and the `"anonymous" → Public` mapping next to the type's
        // other constructors (issue #26 review feedback).
        let caller_principal = Principal::from_bridge_user(&caller_user);
        self.app_state
            .observability()
            .audit_with_caller(
                Some(&caller_principal),
                "tunnel_proxied_request",
                Some(&agent_name),
                serde_json::json!({
                    "request_id": &request_id,
                    "caller": &caller_user,
                }),
            )
            .await
            .ok();

        // Build message request
        let request = A2aMessageRequest::new(agent_name.clone(), message)
            .with_user(caller_user)
            .with_new_session(false);

        // Execute via stateless agent service with streaming
        let agent_service = self.app_state.agent_service();
        match agent_service.execute_message_streaming(request).await {
            Ok(event_stream) => {
                self.stream_response(event_stream, handle, request_id)
                    .await?;
            }
            Err(e) => {
                warn!("Agent execution failed for {}: {}", agent_name, e);
                return self
                    .send_error_response(&handle, &request_id, &format!("Execution failed: {}", e))
                    .await;
            }
        }

        Ok(())
    }

    /// Stream agent events back through the tunnel as chunks
    async fn stream_response(
        &self,
        mut event_stream: crate::engine::EventStream,
        handle: TunnelHandle,
        request_id: String,
    ) -> anyhow::Result<()> {
        let mut seq: u32 = 0;
        let mut buffer = String::new();

        while let Some(event) = event_stream.receiver.recv().await {
            match event {
                AgenticEvent::AssistantText {
                    text,
                    is_interstitial: false,
                    ..
                } => {
                    buffer.push_str(&text);
                }
                AgenticEvent::AssistantDelta { text, .. } => {
                    buffer.push_str(&text);
                }
                AgenticEvent::Lifecycle { phase, error, .. } => {
                    match phase {
                        crate::engine::LifecyclePhase::End => {
                            // Flush remaining buffer
                            if !buffer.is_empty() {
                                let chunk = serde_json::json!({
                                    "chunk": buffer,
                                    "done": false,
                                });
                                let _ = handle.send_stream_chunk(
                                    request_id.clone(),
                                    seq,
                                    chunk.to_string().into_bytes(),
                                );
                                seq = seq.saturating_add(1);
                            }
                            // Send done marker
                            let done = serde_json::json!({ "done": true });
                            let _ = handle.send_stream_chunk(
                                request_id.clone(),
                                seq,
                                done.to_string().into_bytes(),
                            );
                            let _ = handle.send_stream_end(request_id.clone());
                            break;
                        }
                        crate::engine::LifecyclePhase::Error => {
                            let err_msg = error.unwrap_or_else(|| "Unknown error".to_string());
                            let _ = handle.send_stream_chunk(
                                request_id.clone(),
                                seq,
                                serde_json::json!({ "error": err_msg })
                                    .to_string()
                                    .into_bytes(),
                            );
                            let _ = handle.send_stream_end(request_id.clone());
                            break;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }

            // Flush buffer periodically to avoid large delays
            if buffer.len() > 200 {
                let chunk_text = buffer.clone();
                buffer.clear();
                let chunk = serde_json::json!({
                    "chunk": chunk_text,
                    "done": false,
                });
                let _ = handle.send_stream_chunk(
                    request_id.clone(),
                    seq,
                    chunk.to_string().into_bytes(),
                );
                seq = seq.saturating_add(1);
            }
        }

        // Flush any remaining buffer if the stream closed without Lifecycle::End
        if !buffer.is_empty() {
            let chunk = serde_json::json!({
                "chunk": buffer,
                "done": false,
            });
            let _ =
                handle.send_stream_chunk(request_id.clone(), seq, chunk.to_string().into_bytes());
        }
        // Ensure stream end is sent even if the event loop exited unexpectedly
        let _ = handle.send_stream_end(request_id.clone());

        // Wait for session persistence
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            event_stream.completion,
        )
        .await;

        Ok(())
    }

    /// Send an error response back through the tunnel
    async fn send_error_response(
        &self,
        handle: &TunnelHandle,
        request_id: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        let error_json = serde_json::json!({ "error": message });
        let _ = handle.send_stream_chunk(
            request_id.to_string(),
            0,
            error_json.to_string().into_bytes(),
        );
        let _ = handle.send_stream_end(request_id.to_string());
        Ok(())
    }

    /// Set the status of an instance and send a status_update message to the hub.
    pub async fn set_instance_status(
        &self,
        agent_name: &str,
        status: InstanceStatus,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);

        // Update local state
        {
            let mut state = self.state.write().await;
            if let Some(entry) = state.instance_state.get_mut(&instance_id) {
                entry.status = status.clone();
            } else {
                state.instance_state.insert(
                    instance_id.clone(),
                    InstanceState {
                        status: status.clone(),
                        ..Default::default()
                    },
                );
            }
        }

        // Send status update through tunnel if available
        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };
        if let Some(handle) = handle {
            let payload = StatusUpdatePayload {
                instance_id: instance_id.clone(),
                status: status.clone(),
            };
            if let Err(e) = handle.send(TunnelMessage::StatusUpdate { payload }) {
                warn!("Failed to send status update for {}: {}", agent_name, e);
            } else {
                debug!("Sent status update for {}: {:?}", agent_name, status);
            }
        }

        Ok(())
    }

    /// Get the current status of an instance.
    pub async fn get_instance_status(&self, agent_name: &str) -> InstanceStatus {
        let instance_id = self.instance_id(agent_name);
        let state = self.state.read().await;
        state
            .instance_state
            .get(&instance_id)
            .map(|s| s.status.clone())
            .unwrap_or(InstanceStatus::Online)
    }

    /// Set the exposure of an instance and send an exposure_update message to the hub.
    pub async fn set_instance_exposure(
        &self,
        agent_name: &str,
        exposure: super::protocol::InstanceExposure,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);

        // Update local state
        {
            let mut state = self.state.write().await;
            if let Some(entry) = state.instance_state.get_mut(&instance_id) {
                entry.exposure = exposure.clone();
            } else {
                state.instance_state.insert(
                    instance_id.clone(),
                    InstanceState {
                        exposure: exposure.clone(),
                        ..Default::default()
                    },
                );
            }
        }

        self.send_exposure_update(agent_name, exposure).await
    }

    /// Re-push the current exposure for an instance to PekoHub with an
    /// `allowed_user_ids` list freshly derived from the on-disk agent
    /// config. Used by the permit/revoke IPC paths (issue #16) so that
    /// PekoHub's `canChat` ACL and the runtime's defense-in-depth
    /// `instance_state.allowed_users` cache are kept in sync with the
    /// local `AgentConfig.permissions` without requiring a daemon
    /// restart or a new `instance_announce`.
    ///
    /// No-ops if:
    /// - the agent has no cached `instance_state` (tunnel not yet
    ///   connected, or instance never announced). The next
    ///   `announce_instances` after `TunnelReady` will pick up the
    ///   latest config.
    /// - the current exposure is not `Private` (Public/Unexposed
    ///   agents don't carry an `allowed_users` list, and we must not
    ///   silently flip the exposure as a side effect of a permit
    ///   call).
    /// - there is no live tunnel handle.
    pub async fn refresh_instance_allowed_users(&self, agent_name: &str) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);
        let exposure = {
            let state = self.state.read().await;
            state
                .instance_state
                .get(&instance_id)
                .map(|s| s.exposure.clone())
        };
        let exposure = match exposure {
            Some(e) if e == InstanceExposure::Private => e,
            Some(e) => {
                debug!(
                    "Skipping allowed_users refresh for {}: exposure is {:?}, not Private",
                    agent_name, e
                );
                return Ok(());
            }
            None => {
                debug!(
                    "Skipping allowed_users refresh for {}: no cached instance state \
                     (tunnel not yet connected or instance not announced)",
                    agent_name
                );
                return Ok(());
            }
        };
        self.send_exposure_update(agent_name, exposure).await
    }

    /// Build and send an `ExposureUpdate` for the given agent, with
    /// `allowed_user_ids` re-derived from the live `AgentConfig`.
    /// The caller is responsible for ensuring the agent's current
    /// exposure is meaningful (i.e. `Private`) and that local
    /// `instance_state` reflects the desired exposure before invoking.
    async fn send_exposure_update(
        &self,
        agent_name: &str,
        exposure: InstanceExposure,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);
        let allowed_user_ids = if exposure == InstanceExposure::Private {
            let agent_service = self.app_state.agent_mgmt_service();
            match agent_service.get_agent(agent_name, None).await {
                Ok(Some(info)) => Self::compute_allowed_user_ids(&info.config),
                Ok(None) => None,
                Err(e) => {
                    warn!("Failed to load agent config for {}: {}", agent_name, e);
                    None
                }
            }
        } else {
            None
        };

        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };

        if let Some(handle) = handle {
            let payload = ExposureUpdatePayload {
                instance_id: instance_id.clone(),
                exposure: exposure.clone(),
                allowed_user_ids: allowed_user_ids.clone(),
            };
            if let Err(e) = handle.send(TunnelMessage::ExposureUpdate { payload }) {
                warn!("Failed to send exposure update for {}: {}", agent_name, e);
                return Err(e.into());
            }
            debug!("Sent exposure update for {}: {:?}", agent_name, exposure);
        } else {
            debug!(
                "No tunnel handle, exposure update for {} is dropped (will be re-announced on next TunnelReady)",
                agent_name
            );
        }

        Ok(())
    }

    /// Handle exposure update control message from PekoHub
    async fn handle_exposure_update(&self, payload: ExposureUpdatePayload) -> anyhow::Result<()> {
        info!(
            "Exposure update for instance {}: {:?}",
            payload.instance_id, payload.exposure
        );

        // Update local instance state cache for defense-in-depth enforcement
        let mut state = self.state.write().await;
        if let Some(entry) = state.instance_state.get_mut(&payload.instance_id) {
            entry.exposure = payload.exposure.clone();
            entry.allowed_users = payload.allowed_user_ids.clone().unwrap_or_default();
        } else {
            state.instance_state.insert(
                payload.instance_id.clone(),
                InstanceState {
                    exposure: payload.exposure.clone(),
                    allowed_users: payload.allowed_user_ids.clone().unwrap_or_default(),
                    status: InstanceStatus::Online,
                },
            );
        }
        drop(state);

        // Re-announce the instance to confirm the change
        let agent_service = self.app_state.agent_mgmt_service();
        let agents = match agent_service.list_agents(None).await {
            Ok(agents) => agents,
            Err(e) => {
                warn!("Failed to list agents for exposure re-announce: {}", e);
                return Ok(());
            }
        };

        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };

        if let Some(handle) = handle {
            for agent in agents {
                let instance_id = self.instance_id(&agent.name);
                if instance_id == payload.instance_id {
                    let status = self.get_instance_status(&agent.name).await;
                    let announce_payload = InstanceAnnouncePayload {
                        id: instance_id,
                        instance_type: InstanceType::Agent,
                        name: agent.name.clone(),
                        agent_did: agent.config.agent_did.clone(),
                        bundle_ref: None,
                        runtime_display_name: Some(self.runtime_display_name.clone()),
                        status,
                        exposure: payload.exposure.clone(),
                        allowed_users: payload.allowed_user_ids.clone(),
                        capabilities: None,
                        metadata: None,
                    };
                    if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce {
                        payload: announce_payload,
                    }) {
                        warn!(
                            "Failed to re-announce instance {} after exposure update: {}",
                            agent.name, e
                        );
                    } else {
                        debug!("Re-announced instance {} after exposure update", agent.name);
                    }
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle status update control message from PekoHub (hub forcing status change)
    async fn handle_status_update(&self, payload: StatusUpdatePayload) -> anyhow::Result<()> {
        info!(
            "Status update for instance {}: {:?}",
            payload.instance_id, payload.status
        );

        // Update local state cache
        let mut state = self.state.write().await;
        if let Some(entry) = state.instance_state.get_mut(&payload.instance_id) {
            entry.status = payload.status.clone();
        } else {
            state.instance_state.insert(
                payload.instance_id.clone(),
                InstanceState {
                    status: payload.status.clone(),
                    ..Default::default()
                },
            );
        }
        drop(state);

        Ok(())
    }

    /// Handle an inbound `AgentToAgentRequest` from a peer runtime
    /// (proxied by pekohub). Issue #29 Slice C.
    ///
    /// Steps:
    /// 1. Derive the caller's `VerifyingKey` from `caller_runtime_id`
    ///    (did:key is self-certifying).
    /// 2. Re-verify the signature on the canonical pre-image — the
    ///    hub's source-allowlist is the primary gate; this is
    ///    defense in depth against a hub bug or a stale forwarder.
    /// 3. Look up the local agent by `target_agent_did`.
    /// 4. Build a `MessageRequest` with `caller_principal =
    ///    Principal::Agent(caller_agent_did)` (issue #24 + #28).
    /// 5. Dispatch via `StatelessAgentService`.
    /// 6. Serialize the result to `A2aSendResult` and send back via
    ///    the same tunnel as an `AgentToAgentResponse`.
    ///
    /// Every error path sends a structured `HubA2AErrorResponse`
    /// back to the caller so the caller can distinguish "target
    /// not found" from "target rejected me" from "I'm broken"
    /// rather than waiting for a timeout.
    #[allow(clippy::too_many_arguments)]
    async fn handle_inbound_agent_to_agent_request(
        &self,
        handle: TunnelHandle,
        request_id: String,
        caller_runtime_id: String,
        caller_agent_did: String,
        target_agent_did: String,
        session_id: Option<String>,
        message: String,
        team: Option<String>,
        signature: String,
    ) -> anyhow::Result<()> {
        // 1. Derive the verifying key from the caller's runtime_id.
        let verifying_key = match did_key_to_verifying_key(&caller_runtime_id) {
            Ok(k) => k,
            Err(e) => {
                warn!(
                    "inbound AgentToAgentRequest: invalid caller_runtime_id {caller_runtime_id}: {e}"
                );
                return self
                    .send_hub_error(
                        &handle,
                        &request_id,
                        "internal_error",
                        &format!("invalid caller_runtime_id: {e}"),
                    )
                    .await;
            }
        };

        // 2. Re-verify the signature on the canonical pre-image.
        let signed = SignedFields {
            request_id: &request_id,
            caller_runtime_id: &caller_runtime_id,
            caller_agent_did: &caller_agent_did,
            target_agent_did: &target_agent_did,
            message: &message,
            session_id: session_id.as_deref(),
            team: team.as_deref(),
        };
        if let Err(e) = verify_request(&verifying_key, signed, &signature) {
            warn!(
                "inbound AgentToAgentRequest: signature verification failed for caller_runtime_id={caller_runtime_id}: {e}"
            );
            return self
                .send_hub_error(
                    &handle,
                    &request_id,
                    "forbidden",
                    &format!("signature did not verify: {e}"),
                )
                .await;
        }

        // 3. Look up the local agent by target_agent_did.
        let local_agent_name = match self.find_local_agent_by_did(&target_agent_did).await {
            Some(name) => name,
            None => {
                return self
                    .send_hub_error(
                        &handle,
                        &request_id,
                        "target_not_found",
                        &format!(
                            "no local agent has agent_did={target_agent_did} (request_id={request_id})"
                        ),
                    )
                    .await;
            }
        };

        // Slice D: emit the inbound-receive audit event now that
        // the request has been verified, the agent has been
        // located, and we're about to dispatch. The session_id is
        // best-effort empty here (the dispatcher doesn't have
        // session context); a future PR can thread it through.
        let local_runtime_id = self.app_state.runtime_identity().runtime_did.clone();
        let received_event = a2a_audit::build_a2a_received_inbound(
            "", // session_id
            &request_id,
            &caller_runtime_id,
            &caller_agent_did,
            // Note: at this point we don't know the *original
            // caller's* runtime_id beyond `caller_runtime_id`
            // (the local runtime IS the target). The audit row
            // records the local runtime's id as `runtime_id_target`.
            &local_runtime_id,
            &target_agent_did,
            &message,
        );
        a2a_audit::emit_a2a_received(&received_event);

        // 4 + 5. Build the request and dispatch.
        let caller_principal = Principal::Agent(caller_agent_did.clone());
        let request = A2aMessageRequest::new(&local_agent_name, message.clone())
            .with_session_opt(session_id.clone())
            .with_team_opt(team.clone())
            .with_user("")
            .with_caller_agent_opt(Some(caller_agent_did.clone()))
            .with_caller_principal(caller_principal);

        let agent_service = self.app_state.agent_service();
        let result = agent_service.execute_message(request).await;

        // 6. Serialize and respond.
        let a2a_result = match result {
            Ok(msg) => A2aSendResult {
                success: msg.success,
                response: msg.content,
                session_id: msg.session_id,
                iterations: Some(msg.iterations),
                tool_calls: if msg.tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        msg.tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "name": tc.name,
                                    "parameters": tc.parameters,
                                    "result": tc.result,
                                })
                            })
                            .collect(),
                    )
                },
                duration_ms: Some(msg.duration_ms),
                error: msg.error,
            },
            Err(e) => A2aSendResult {
                success: false,
                response: String::new(),
                session_id: String::new(),
                iterations: None,
                tool_calls: None,
                duration_ms: None,
                error: Some(e.to_string()),
            },
        };

        let payload = match serde_json::to_vec(&a2a_result) {
            Ok(p) => p,
            Err(e) => {
                return self
                    .send_hub_error(
                        &handle,
                        &request_id,
                        "internal_error",
                        &format!("failed to serialize A2aSendResult: {e}"),
                    )
                    .await;
            }
        };

        // Slice D: emit the response-side audit event before
        // sending. The local agent is the "caller" of the
        // response; the original caller is the "target".
        let response_preview = if a2a_result.success {
            a2a_result.response.clone()
        } else {
            a2a_result
                .error
                .clone()
                .unwrap_or_else(|| "(no error message)".to_string())
        };
        let sent_response_event = a2a_audit::build_a2a_sent_response(
            "", // session_id
            &request_id,
            &caller_runtime_id,
            &caller_agent_did,
            &local_runtime_id,
            &target_agent_did,
            &response_preview,
        );
        a2a_audit::emit_a2a_sent(&sent_response_event);

        handle.send(TunnelMessage::AgentToAgentResponse {
            request_id,
            payload,
        })?;
        Ok(())
    }

    /// Handle an inbound `AgentToAgentResponse` — the half of the
    /// round-trip that completes the `oneshot::Receiver` the
    /// outbound `A2aSendTool` is awaiting on. Issue #29 Slice C.
    async fn handle_inbound_agent_to_agent_response(
        &self,
        request_id: String,
        payload: Vec<u8>,
    ) -> anyhow::Result<()> {
        let pending = self.app_state.pending_a2a_responses();
        let delivered = pending.complete(&request_id, payload);
        if !delivered {
            // The caller already timed out, the request was
            // cancelled, or the request_id is spurious (e.g. a
            // pekohub test forwarding a synthetic response to a
            // nonexistent id). Logging as a warn is the right
            // signal — it's a peer contract violation, not a crash.
            warn!(
                "inbound AgentToAgentResponse: no pending a2a request for request_id={request_id} \
                 (probably already timed out or cancelled)"
            );
        }
        Ok(())
    }

    /// Synthesize a `HubA2AErrorResponse` and send it back to the
    /// caller over the live tunnel handle. Used by
    /// `handle_inbound_agent_to_agent_request` on every error
    /// path so the caller's `execute_remote` decodes a structured
    /// error (target_not_found / forbidden / internal_error)
    /// rather than a hang or a generic "remote a2a failed" string.
    async fn send_hub_error(
        &self,
        handle: &TunnelHandle,
        request_id: &str,
        code: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_vec(&HubA2AErrorResponse {
            kind: "error".to_string(),
            code: code.to_string(),
            message: message.to_string(),
        })
        .map_err(|e| anyhow::anyhow!("failed to serialize HubA2AErrorResponse: {e}"))?;
        handle.send(TunnelMessage::AgentToAgentResponse {
            request_id: request_id.to_string(),
            payload,
        })?;
        Ok(())
    }

    /// Look up the local agent that owns the given `agent_did`.
    /// Returns the agent's local name (used to dispatch via
    /// `MessageRequest::new(...)`). Returns `None` if no local
    /// agent has that DID, or the DID isn't set.
    ///
    /// O(N) over the local agent table; an `agent_did` index is a
    /// natural follow-up but unnecessary for the typical
    /// (small) number of agents a single runtime hosts.
    async fn find_local_agent_by_did(&self, agent_did: &str) -> Option<String> {
        let agent_mgmt = self.app_state.agent_mgmt_service();
        let agents = agent_mgmt.list_agents(None).await.ok()?;
        for summary in agents {
            // `common::types::agent::AgentSummary` carries the full
            // `AgentConfig`; the per-agent DID lives on
            // `summary.config.agent_did` (issue #28).
            if summary.config.agent_did.as_deref() == Some(agent_did) {
                return Some(summary.name);
            }
        }
        None
    }

    /// Check if a proxied request is allowed for the given agent/instance.
    ///
    /// Returns `Ok(())` if allowed, or an error message if denied.
    /// This is defense-in-depth: PekoHub already checks auth, but the runtime
    /// should also enforce its own ACL in case the tunnel is bypassed.
    async fn check_request_allowed(
        &self,
        agent_name: &str,
        bridge_payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);

        let state = self.state.read().await;
        let instance_state = match state.instance_state.get(&instance_id) {
            Some(s) => s.clone(),
            None => {
                // No state cached yet — agent was never announced or exposure
                // was never set. Default to allowing (backward compat).
                return Ok(());
            }
        };
        drop(state);

        match instance_state.exposure {
            InstanceExposure::Public => Ok(()),
            InstanceExposure::Unexposed => {
                anyhow::bail!("Agent is not exposed")
            }
            InstanceExposure::Private => {
                // Extract user ID from bridge payload (set by PekoHub)
                let user_id = bridge_payload
                    .get("headers")
                    .and_then(|h| h.get("x-pekohub-user-id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if user_id.is_empty() {
                    warn!(
                        agent_name,
                        "Private instance request denied: missing x-pekohub-user-id in bridge payload"
                    );
                    anyhow::bail!("Authentication required")
                }

                if instance_state.allowed_users.iter().any(|u| u == user_id) {
                    Ok(())
                } else {
                    warn!(
                        agent_name,
                        user_id, "Private instance request denied: user not in allowed_users"
                    );
                    anyhow::bail!("Forbidden")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::{AppState, DaemonConfigSnapshot};
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    async fn create_test_app_state() -> AppState {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let config = DaemonConfigSnapshot {
            data_dir: data_dir.clone(),
            config_dir: data_dir.clone(),
            log_level: "info".to_string(),
        };
        AppState::with_data_dir(
            temp_dir.path().to_path_buf(),
            "127.0.0.1".to_string(),
            11435,
            config,
            data_dir,
        )
        .await
        .unwrap()
    }

    fn mock_tunnel_handle() -> (TunnelHandle, mpsc::UnboundedReceiver<TunnelMessage>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (TunnelHandle::new(tx), rx)
    }

    // ─── Issue #17: caller resolution from bridge payload ──────────────────

    /// PekoHub sets `x-pekohub-user-id` on every proxied request. When
    /// no JWT validator is configured (the back-compat case), the
    /// dispatcher uses that value as the `MessageRequest::user` so
    /// downstream attribution (audit log, tool hooks) sees the real
    /// pekohub user, not the literal `"web"` placeholder.
    #[tokio::test]
    async fn resolve_bridge_caller_extracts_user_from_headers() {
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": "user-42"},
        });
        assert_eq!(resolve_bridge_caller(&payload, None).await, "user-42");
    }

    /// Missing header → `"anonymous"` (not `"web"`, not the empty
    /// string). Downstream code treats `"anonymous"` as "no real user
    /// asserted", so per-user permission checks stay conservative.
    #[tokio::test]
    async fn resolve_bridge_caller_falls_back_to_anonymous_when_header_missing() {
        let payload = serde_json::json!({"body": {"message": "hi"}});
        assert_eq!(resolve_bridge_caller(&payload, None).await, "anonymous");
    }

    /// Empty string header → `"anonymous"`. Defends against PekoHub
    /// bugs that emit `x-pekohub-user-id:` (empty value).
    #[tokio::test]
    async fn resolve_bridge_caller_falls_back_to_anonymous_when_header_empty() {
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": ""},
        });
        assert_eq!(resolve_bridge_caller(&payload, None).await, "anonymous");
    }

    /// Whitespace-only header → `"anonymous"`. Catches header values
    /// that look populated to a JSON parse but are semantically empty.
    #[tokio::test]
    async fn resolve_bridge_caller_falls_back_to_anonymous_when_header_whitespace() {
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": "   "},
        });
        assert_eq!(resolve_bridge_caller(&payload, None).await, "anonymous");
    }

    /// Non-string header (e.g. number) → `"anonymous"`. Catches PekoHub
    /// sending a typed value the runtime can't attribute.
    #[tokio::test]
    async fn resolve_bridge_caller_falls_back_to_anonymous_when_header_not_string() {
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": 12345},
        });
        assert_eq!(resolve_bridge_caller(&payload, None).await, "anonymous");
    }

    // ─── Issue #17: JWT wiring (signed identity) ───────────────────────────

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};

    /// Build a `(validator, signing_key)` pair whose `validator` accepts
    /// tokens signed by `signing_key` against the runtime DID
    /// `did:key:z6MkTestRuntime` and the issuer `pekohub`.
    fn ed25519_validator() -> (crate::auth::jwt::JwtValidator, SigningKey) {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill(&mut bytes);
        let signing_key = SigningKey::from_bytes(&bytes);
        let verifying_key = signing_key.verifying_key();

        let x = URL_SAFE_NO_PAD.encode(verifying_key.to_bytes());
        let jwks = crate::auth::jwt::JwksResponse {
            keys: vec![crate::auth::jwt::JwkEntry {
                kty: "OKP".to_string(),
                kid: Some("test-key".to_string()),
                n: None,
                e: None,
                x: Some(x),
                crv: Some("Ed25519".to_string()),
                extra: std::collections::HashMap::new(),
            }],
        };
        let validator = crate::auth::jwt::JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );
        (validator, signing_key)
    }

    /// Mint an EdDSA JWT for the given sub, with the audience/issuer
    /// expected by `ed25519_validator()`.
    fn mint_jwt(signing_key: &SigningKey, sub: &str) -> String {
        let header = serde_json::json!({"alg": "EdDSA", "typ": "JWT", "kid": "test-key"});
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": sub,
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });
        let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims.to_string());
        let message = format!("{header_b64}.{claims_b64}");
        let signature = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
        format!("{message}.{sig_b64}")
    }

    /// When PekoHub sends an `Authorization: Bearer <jwt>` and a
    /// `JwtValidator` is configured, the validated `sub` claim is the
    /// caller (issue #17 acceptance criteria).
    #[tokio::test]
    async fn resolve_bridge_caller_uses_validated_jwt_sub() {
        let (validator, signing_key) = ed25519_validator();
        let jwt = mint_jwt(&signing_key, "user-jwt");

        let payload = serde_json::json!({
            "headers": {
                "Authorization": format!("Bearer {jwt}"),
                // Header disagrees with the JWT — but the JWT wins.
                "x-pekohub-user-id": "user-hub",
            },
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            "user-jwt"
        );
    }

    /// Tampered JWT (signature doesn't verify) → falls back to the
    /// unverified `x-pekohub-user-id` header (issue #17 acceptance
    /// criteria: "unit-tested with at least one positive and one
    /// tampered-signature case").
    #[tokio::test]
    async fn resolve_bridge_caller_falls_back_on_tampered_jwt() {
        let (validator, _signing_key) = ed25519_validator();
        // Sign with a *different* key — the signature won't verify.
        let mut bytes = [0u8; 32];
        bytes[0] = 0xAB;
        let wrong_key = SigningKey::from_bytes(&bytes);
        let wrong_jwt = mint_jwt(&wrong_key, "user-tampered");

        let payload = serde_json::json!({
            "headers": {
                "Authorization": format!("Bearer {wrong_jwt}"),
                "x-pekohub-user-id": "user-hub-fallback",
            },
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            "user-hub-fallback"
        );
    }

    /// JWT present but no validator configured → falls back to the
    /// unverified hub header (back-compat for runtimes that haven't
    /// enabled pekohub JWT validation).
    #[tokio::test]
    async fn resolve_bridge_caller_falls_back_when_no_validator_configured() {
        let (_validator, signing_key) = ed25519_validator();
        let jwt = mint_jwt(&signing_key, "user-jwt");

        let payload = serde_json::json!({
            "headers": {
                "Authorization": format!("Bearer {jwt}"),
                "x-pekohub-user-id": "user-hub",
            },
        });
        assert_eq!(resolve_bridge_caller(&payload, None).await, "user-hub");
    }

    /// Header-only (no JWT) → uses the hub header (unverified). This is
    /// the back-compat path for deployments that haven't enabled
    /// pekohub JWT validation yet.
    #[tokio::test]
    async fn resolve_bridge_caller_uses_header_when_no_jwt() {
        let (validator, _signing_key) = ed25519_validator();
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": "user-hub"},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            "user-hub"
        );
    }

    /// `Authorization` header is case-insensitive (RFC 7230) — the
    /// lowercase variant `authorization` must also be recognized.
    #[tokio::test]
    async fn resolve_bridge_caller_accepts_lowercase_authorization_header() {
        let (validator, signing_key) = ed25519_validator();
        let jwt = mint_jwt(&signing_key, "user-jwt");

        let payload = serde_json::json!({
            "headers": {
                "authorization": format!("Bearer {jwt}"),
            },
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            "user-jwt"
        );
    }

    #[tokio::test]
    async fn test_handle_message_stores_tunnel_handle_synchronously() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, _rx) = mock_tunnel_handle();

        // Before handle_message, tunnel_handle should be None
        {
            let state = dispatcher.state.read().await;
            assert!(state.tunnel_handle.is_none());
        }

        // Call handle_message and await it — the handle should be stored
        // synchronously before the method returns
        dispatcher
            .handle_message(TunnelMessage::Heartbeat { seq: 1 }, handle.clone())
            .await;

        // After awaiting handle_message, the handle must be available
        let state = dispatcher.state.read().await;
        assert!(state.tunnel_handle.is_some());
    }

    #[tokio::test]
    async fn test_set_instance_status_sends_status_update() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, mut rx) = mock_tunnel_handle();

        // Seed the tunnel handle
        {
            let mut state = dispatcher.state.write().await;
            state.tunnel_handle = Some(handle);
        }

        dispatcher
            .set_instance_status("test-agent", InstanceStatus::Busy)
            .await
            .unwrap();

        // Verify a StatusUpdate message was sent
        let msg = rx.recv().await.expect("Expected a message on the channel");
        match msg {
            TunnelMessage::StatusUpdate { payload } => {
                assert_eq!(payload.status, InstanceStatus::Busy);
                // instance_id is a UUIDv5, so just verify it's non-empty
                assert!(!payload.instance_id.is_empty());
            }
            other => panic!("Expected StatusUpdate, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_instance_exposure_sends_exposure_update() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, mut rx) = mock_tunnel_handle();

        // Seed the tunnel handle
        {
            let mut state = dispatcher.state.write().await;
            state.tunnel_handle = Some(handle);
        }

        dispatcher
            .set_instance_exposure("test-agent", InstanceExposure::Public)
            .await
            .unwrap();

        // Verify an ExposureUpdate message was sent
        let msg = rx.recv().await.expect("Expected a message on the channel");
        match msg {
            TunnelMessage::ExposureUpdate { payload } => {
                assert_eq!(payload.exposure, InstanceExposure::Public);
                assert!(!payload.instance_id.is_empty());
            }
            other => panic!("Expected ExposureUpdate, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_get_instance_status_returns_default_online_for_unknown() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);

        let status = dispatcher.get_instance_status("unknown-agent").await;
        assert_eq!(status, InstanceStatus::Online);
    }

    #[tokio::test]
    async fn test_handle_exposure_update_updates_local_state() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, _rx) = mock_tunnel_handle();

        // Seed the tunnel handle so re-announce can proceed (it will fail
        // at list_agents and return Ok, but local state must still be updated)
        {
            let mut state = dispatcher.state.write().await;
            state.tunnel_handle = Some(handle);
        }

        let instance_id = dispatcher.instance_id("test-agent");
        let payload = ExposureUpdatePayload {
            instance_id: instance_id.clone(),
            exposure: InstanceExposure::Public,
            allowed_user_ids: Some(vec!["user-1".to_string()]),
        };

        dispatcher.handle_exposure_update(payload).await.unwrap();

        // Verify local state was updated
        let state = dispatcher.state.read().await;
        let entry = state
            .instance_state
            .get(&instance_id)
            .expect("Instance state should exist");
        assert_eq!(entry.exposure, InstanceExposure::Public);
        assert_eq!(entry.allowed_users, vec!["user-1".to_string()]);
    }

    #[tokio::test]
    async fn test_set_instance_status_updates_local_state() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, _rx) = mock_tunnel_handle();

        {
            let mut state = dispatcher.state.write().await;
            state.tunnel_handle = Some(handle);
        }

        dispatcher
            .set_instance_status("my-agent", InstanceStatus::Error)
            .await
            .unwrap();

        let status = dispatcher.get_instance_status("my-agent").await;
        assert_eq!(status, InstanceStatus::Error);
    }

    #[tokio::test]
    async fn test_set_instance_exposure_updates_local_state() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, _rx) = mock_tunnel_handle();

        {
            let mut state = dispatcher.state.write().await;
            state.tunnel_handle = Some(handle);
        }

        dispatcher
            .set_instance_exposure("my-agent", InstanceExposure::Unexposed)
            .await
            .unwrap();

        let instance_id = dispatcher.instance_id("my-agent");
        let state = dispatcher.state.read().await;
        let entry = state.instance_state.get(&instance_id).unwrap();
        assert_eq!(entry.exposure, InstanceExposure::Unexposed);
    }

    #[tokio::test]
    async fn test_check_request_allowed_public_allows_any_request() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);

        let instance_id = dispatcher.instance_id("public-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Public,
                    allowed_users: vec![],
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "any-user"}});
        let result = dispatcher
            .check_request_allowed("public-agent", &bridge_payload)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_check_request_allowed_unexposed_denies_any_request() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);

        let instance_id = dispatcher.instance_id("unexposed-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Unexposed,
                    allowed_users: vec![],
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-123"}});
        let result = dispatcher
            .check_request_allowed("unexposed-agent", &bridge_payload)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Agent is not exposed"));
    }

    #[tokio::test]
    async fn test_check_request_allowed_private_allows_allowed_user() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);

        let instance_id = dispatcher.instance_id("private-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    allowed_users: vec!["user-123".to_string()],
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-123"}});
        let result = dispatcher
            .check_request_allowed("private-agent", &bridge_payload)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_check_request_allowed_private_without_user_id_denies() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);

        let instance_id = dispatcher.instance_id("private-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    allowed_users: vec!["user-123".to_string()],
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {}});
        let result = dispatcher
            .check_request_allowed("private-agent", &bridge_payload)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Authentication required"));
    }

    #[tokio::test]
    async fn test_check_request_allowed_private_with_non_allowed_user_id_denies() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);

        let instance_id = dispatcher.instance_id("private-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    allowed_users: vec!["user-123".to_string()],
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-999"}});
        let result = dispatcher
            .check_request_allowed("private-agent", &bridge_payload)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Forbidden"));
    }

    // -- Issue #29 (Slice C): inbound AgentToAgentRequest + Response -----

    /// `handle_inbound_agent_to_agent_request` rejects a request with
    /// a malformed caller_runtime_id (cannot be parsed as a did:key)
    /// by sending back an `internal_error` `HubA2AErrorResponse`
    /// rather than crashing the dispatcher.
    #[tokio::test]
    async fn test_inbound_agent_to_agent_request_rejects_malformed_caller_did() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, mut rx) = mock_tunnel_handle();

        dispatcher
            .handle_inbound_agent_to_agent_request(
                handle,
                "req-malformed".to_string(),
                "did:peko:agent:not-a-real-did-key".to_string(), // not a did:key form
                "did:peko:agent:caller".to_string(),
                "did:peko:agent:target".to_string(),
                None,
                "hi".to_string(),
                None,
                "sig".to_string(),
            )
            .await
            .expect("handler must not panic; errors are reported via the response");

        // The handler should have sent back a structured
        // HubA2AErrorResponse. Drain the response and check the shape.
        let response = rx.recv().await.expect("response must be sent");
        let TunnelMessage::AgentToAgentResponse { request_id, payload } = response else {
            panic!("expected AgentToAgentResponse, got: {response:?}");
        };
        assert_eq!(request_id, "req-malformed");
        let err: HubA2AErrorResponse =
            serde_json::from_slice(&payload).expect("payload must be a HubA2AErrorResponse");
        assert_eq!(err.kind, "error");
        assert_eq!(err.code, "internal_error");
        assert!(
            err.message.contains("invalid caller_runtime_id"),
            "error must name the cause; got: {}",
            err.message
        );
    }

    /// `handle_inbound_agent_to_agent_request` rejects a request with
    /// an invalid signature (key is well-formed but signature bytes
    /// don't verify) by sending back a `forbidden`
    /// `HubA2AErrorResponse`.
    #[tokio::test]
    async fn test_inbound_agent_to_agent_request_rejects_bad_signature() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let (handle, mut rx) = mock_tunnel_handle();

        // Use a known-good did:key (from a generated keypair) but
        // sign with a DIFFERENT key — signature must not verify.
        let kp_caller = crate::identity::keys::KeyPair::generate();
        let kp_attacker = crate::identity::keys::KeyPair::generate();
        let caller_did = crate::tunnel::verifying_key_to_did_key(&kp_caller.verifying_key);
        let signed = crate::tunnel::SignedFields {
            request_id: "req-bad-sig",
            caller_runtime_id: &caller_did,
            caller_agent_did: "did:peko:agent:caller",
            target_agent_did: "did:peko:agent:target",
            message: "hi",
            session_id: None,
            team: None,
        };
        let sig = crate::tunnel::sign_request(&kp_attacker.signing_key, signed);

        dispatcher
            .handle_inbound_agent_to_agent_request(
                handle,
                "req-bad-sig".to_string(),
                caller_did,
                "did:peko:agent:caller".to_string(),
                "did:peko:agent:target".to_string(),
                None,
                "hi".to_string(),
                None,
                sig,
            )
            .await
            .expect("handler must not panic");

        let response = rx.recv().await.expect("response must be sent");
        let TunnelMessage::AgentToAgentResponse { request_id, payload } = response else {
            panic!("expected AgentToAgentResponse, got: {response:?}");
        };
        assert_eq!(request_id, "req-bad-sig");
        let err: HubA2AErrorResponse =
            serde_json::from_slice(&payload).expect("payload must decode");
        assert_eq!(err.code, "forbidden");
        assert!(
            err.message.contains("signature did not verify"),
            "error must name the cause; got: {}",
            err.message
        );
    }

    /// `handle_inbound_agent_to_agent_response` completes the
    /// matching pending oneshot on the `PendingA2aResponses`
    /// registry so the outbound `A2aSendTool` awaiter unblocks.
    #[tokio::test]
    async fn test_inbound_agent_to_agent_response_completes_pending() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        let pending = dispatcher.app_state.pending_a2a_responses();

        // Register a waiter for a known request_id, then send
        // the matching response through the dispatcher.
        let rx = pending
            .register("req-1")
            .expect("register must succeed for a fresh request_id");

        dispatcher
            .handle_inbound_agent_to_agent_response(
                "req-1".to_string(),
                b"hello".to_vec(),
            )
            .await
            .expect("handler must not panic");

        let delivered = rx.await.expect("waiter must complete");
        assert_eq!(delivered, b"hello");
    }

    /// `handle_inbound_agent_to_agent_response` for a request_id with
    /// no pending waiter is a no-op (logged as a warn). Catches the
    /// failure mode where a stale or duplicate response would
    /// panic.
    #[tokio::test]
    async fn test_inbound_agent_to_agent_response_unknown_request_id_is_noop() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);
        dispatcher
            .handle_inbound_agent_to_agent_response(
                "unknown-request-id".to_string(),
                b"orphan".to_vec(),
            )
            .await
            .expect("handler must not panic on unknown id");
    }

    /// `handle_message` publishes the live `TunnelHandle` to
    /// `AppState.tunnel_handle_slot()` so the outbound
    /// `CrossRuntimeA2aCtx` can send on the most-recent handle on
    /// every reconnect.
    #[tokio::test]
    async fn test_handle_message_publishes_handle_to_app_state_slot() {
        let app_state = create_test_app_state().await;
        let slot = app_state.tunnel_handle_slot();
        // The slot starts as None.
        {
            let g = slot.read().await;
            assert!(g.is_none(), "slot must start empty");
        }

        let dispatcher = TunnelDispatcher::new(app_state.clone());
        let (handle, _rx) = mock_tunnel_handle();
        dispatcher
            .handle_message(TunnelMessage::Heartbeat { seq: 1 }, handle)
            .await;

        // The slot should now be populated. Yield to let the
        // dispatch task finish (handle_message spawns it).
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let g = slot.read().await;
        assert!(g.is_some(), "handle slot must be filled by handle_message");
    }
}
