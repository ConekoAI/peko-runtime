//! Tunnel Request Dispatcher
//!
//! Bridges proxied requests from the PekoHub tunnel to the daemon's service layer.
//! Handles chat execution, streaming responses, and instance lifecycle messages.

use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, error, info, warn};

/// Namespace UUID for generating stable instance IDs from (runtime_did, agent_name).
/// This is a fixed UUIDv4 that acts as the namespace for UUIDv5 generation.
const INSTANCE_ID_NAMESPACE: uuid::Uuid = uuid::uuid!("a1b2c3d4-e5f6-47a8-b9c0-d1e2f3a4b5c6");

/// Maximum number of inbound tunnel messages dispatched concurrently.
///
/// Every inbound message is handed to a spawned dispatch task. Without a
/// cap, a peer (or a misbehaving hub) that floods the tunnel could spawn
/// unbounded tasks and exhaust memory/CPU. The dispatcher holds a shared
/// [`Semaphore`] with this many permits; the tunnel read loop blocks on
/// `acquire` once they are exhausted, applying backpressure instead of
/// queueing work without bound.
const MAX_CONCURRENT_DISPATCHES: usize = 64;

use crate::auth::Subject;
use crate::daemon::state::AppState;

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
use crate::tunnel::principal_send_tool::{HubErrorResponse, PrincipalSendResult};

use crate::auth::ownership::Permission;

/// Errors returned by [`resolve_bridge_caller`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BridgeCallerError {
    /// A JWT was presented but could not be validated.
    InvalidJwt,
    /// No verified or unverified caller could be determined.
    NoCaller,
}

impl std::fmt::Display for BridgeCallerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeCallerError::InvalidJwt => write!(f, "invalid JWT"),
            BridgeCallerError::NoCaller => write!(f, "no caller identity provided"),
        }
    }
}

impl std::error::Error for BridgeCallerError {}

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
/// pekohub JWT validation yet).
///
/// If a JWT is present but validation fails, or if no validator is
/// configured, the function returns [`BridgeCallerError::InvalidJwt`] so
/// the request can be rejected instead of silently trusting a tampered
/// token. If no JWT and no header are present, returns
/// [`BridgeCallerError::NoCaller`].
pub(crate) async fn resolve_bridge_caller(
    bridge_payload: &serde_json::Value,
    jwt_validator: Option<&crate::auth::jwt::JwtValidator>,
) -> Result<String, BridgeCallerError> {
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
                    return Ok(validated.sub);
                }
                Err(e) => {
                    warn!("JWT validation failed ({}); rejecting request", e);
                    return Err(BridgeCallerError::InvalidJwt);
                }
            }
        }
        warn!(
            "Authorization: Bearer <jwt> present but no JWT validator configured; \
             rejecting request"
        );
        return Err(BridgeCallerError::InvalidJwt);
    }

    // 2. Fall back to the unverified hub-asserted header only when no JWT was present.
    header_user(bridge_payload).ok_or(BridgeCallerError::NoCaller)
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
    /// Typed allow-list (ADR-041) — `User` and `Principal` subjects
    /// who can chat with this instance at `private` exposure.
    pub allowed_principals: Vec<crate::auth::Subject>,
    /// Current instance status
    pub status: InstanceStatus,
}

impl Default for InstanceState {
    fn default() -> Self {
        Self {
            exposure: InstanceExposure::Private,
            allowed_principals: Vec::new(),
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
    /// Bounds the number of concurrently-dispatched inbound messages.
    /// Shared across clones (each inbound message clones the dispatcher
    /// into its task) so the cap is global, not per-task. See
    /// [`MAX_CONCURRENT_DISPATCHES`].
    inbound_semaphore: Arc<Semaphore>,
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
            inbound_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_DISPATCHES)),
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
        // Acquire a dispatch permit before spawning. When all
        // `MAX_CONCURRENT_DISPATCHES` permits are in use this `await`
        // parks the tunnel read loop, backpressuring the hub instead of
        // spawning unbounded tasks under a flood. The semaphore is never
        // closed, so the only error is unreachable; degrade by dropping
        // the message rather than panicking if it ever occurs.
        let permit = match Arc::clone(&self.inbound_semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                error!("Tunnel dispatch semaphore closed; dropping inbound message");
                return;
            }
        };
        tokio::spawn(async move {
            // Hold the permit for the lifetime of the dispatch so the
            // slot is released only when this message is fully handled.
            let _permit = permit;
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

    /// Compute the typed `allowedPrincipals` list for a Principal.
    ///
    /// Filters for `Chat` permission grants where the `subject` is a
    /// *named* caller — a `User` or another `Principal` — and normalises
    /// the wire id (PekoHub post-#19 reads `allowedPrincipals: Vec<Subject>`
    /// and matches on `{kind, id}` rather than on a bare id string).
    ///
    /// Both `User` and `Principal` subjects are retained: dropping
    /// `Principal` grants here would silently strip agent-to-agent (A2A,
    /// issue #29) callers from the hub's allow list, so a Principal
    /// granted `Chat` could never reach a `Private` instance. `User` ids
    /// have the legacy `user:` prefix stripped; `Principal` ids are DIDs
    /// and travel verbatim.
    ///
    /// `Public` is not a named caller and is dropped — the principal-tier
    /// allow list only authorises identified subjects.
    ///
    /// Returns `Some(vec)` even when empty so re-announcing an
    /// instance clears PekoHub's allow list when the last grant is
    /// revoked.
    fn compute_allowed_principals(
        config: &crate::principal::PrincipalConfig,
    ) -> Option<Vec<crate::auth::Subject>> {
        use crate::auth::Subject;
        let principals: Vec<Subject> = config
            .permissions
            .iter()
            .filter(|g| g.permission.covers(&Permission::Chat))
            .filter_map(|g| match &g.subject {
                Subject::User(id) => {
                    // Strip the `user:` prefix if present; the wire
                    // subject-id form is the bare id.
                    let bare = id
                        .strip_prefix("user:")
                        .map(String::from)
                        .unwrap_or_else(|| id.clone());
                    Some(Subject::User(bare))
                }
                // Named A2A caller — the id is a DID, used verbatim.
                Subject::Principal(did) => Some(Subject::Principal(did.clone())),
                // Unauthenticated; never a named allow-list entry.
                Subject::Public => None,
            })
            .collect();
        Some(principals)
    }

    /// Send initial instance announcements for all local Principals.
    pub async fn announce_instances(&self, handle: &TunnelHandle) -> anyhow::Result<()> {
        let principal_manager = self.app_state.principal_manager();
        let principals = principal_manager.list_all().await;

        for principal in principals {
            let name = principal.name().await;
            let did = principal.did().await;
            let exposure = principal.exposure().await;
            let (allowed_principals, transport_preference) = {
                let config = principal.config.read().await;
                (
                    Self::compute_allowed_principals(&config),
                    config.transport_preference,
                )
            };
            let instance_id = self.instance_id(&name);
            let payload = InstanceAnnouncePayload {
                id: instance_id.clone(),
                instance_type: InstanceType::Principal,
                name: name.clone(),
                agent_did: None,
                principal_did: Some(did.0.clone()),
                bundle_ref: None,
                runtime_display_name: Some(self.runtime_display_name.clone()),
                status: InstanceStatus::Online,
                exposure: exposure.clone(),
                allowed_principals: allowed_principals.clone(),
                capabilities: None,
                metadata: None,
                transport_preference: Some(transport_preference),
                runtime_direct_endpoint: self
                    .app_state
                    .peko_config
                    .network
                    .direct
                    .advertise_endpoint
                    .clone(),
            };

            // Seed local instance state cache with default Online status and the
            // Principal's configured exposure.
            let mut state = self.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure,
                    allowed_principals: allowed_principals.clone().unwrap_or_default(),
                    status: InstanceStatus::Online,
                },
            );
            drop(state);

            if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce { payload }) {
                warn!("Failed to announce instance {}: {}", name, e);
            } else {
                debug!("Announced principal instance: {}", name);
            }
        }

        Ok(())
    }

    /// Announce a single Principal instance through the tunnel.
    ///
    /// Used when a new Principal is created after the tunnel is already connected.
    pub async fn announce_single_instance(&self, principal_name: &str) -> anyhow::Result<()> {
        let handle = {
            let state = self.state.read().await;
            match state.tunnel_handle.clone() {
                Some(h) => h,
                None => {
                    debug!(
                        "No tunnel handle available; skipping instance announce for {}",
                        principal_name
                    );
                    return Ok(());
                }
            }
        };

        let principal_manager = self.app_state.principal_manager();
        let principal = match principal_manager.get_by_name(principal_name).await {
            Some(p) => p,
            None => {
                warn!(
                    "Principal {} not found; cannot announce instance",
                    principal_name
                );
                return Ok(());
            }
        };

        let name = principal.name().await;
        let did = principal.did().await;
        let exposure = principal.exposure().await;
        let (allowed_principals, transport_preference) = {
            let config = principal.config.read().await;
            (
                Self::compute_allowed_principals(&config),
                config.transport_preference,
            )
        };
        let instance_id = self.instance_id(&name);
        let payload = InstanceAnnouncePayload {
            id: instance_id.clone(),
            instance_type: InstanceType::Principal,
            name: name.clone(),
            agent_did: None,
            principal_did: Some(did.0.clone()),
            bundle_ref: None,
            runtime_display_name: Some(self.runtime_display_name.clone()),
            status: InstanceStatus::Online,
            exposure: exposure.clone(),
            allowed_principals: allowed_principals.clone(),
            capabilities: None,
            metadata: None,
            transport_preference: Some(transport_preference),
            runtime_direct_endpoint: self
                .app_state
                .peko_config
                .network
                .direct
                .advertise_endpoint
                .clone(),
        };

        // Seed local instance state cache
        let mut state = self.state.write().await;
        state.instance_state.insert(
            instance_id,
            InstanceState {
                exposure,
                allowed_principals: allowed_principals.clone().unwrap_or_default(),
                status: InstanceStatus::Online,
            },
        );
        drop(state);

        if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce { payload }) {
            warn!("Failed to announce instance {}: {}", principal_name, e);
        } else {
            debug!("Announced single principal instance: {}", principal_name);
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
        let principal_manager = self.app_state.principal_manager();
        let principals = principal_manager.list_all().await;

        let now = chrono::Utc::now().to_rfc3339();
        for principal in principals {
            let name = principal.name().await;
            let instance_id = self.instance_id(&name);
            let status = self.get_instance_status(&name).await;
            let payload = InstanceHeartbeatPayload {
                id: instance_id,
                status,
                timestamp: now.clone(),
            };
            if let Err(e) = handle.send(TunnelMessage::InstanceHeartbeat { payload }) {
                warn!("Failed to send instance heartbeat: {}", e);
            }
        }
        Ok(())
    }

    /// Main dispatch method
    async fn dispatch(&self, msg: TunnelMessage, handle: TunnelHandle) -> anyhow::Result<()> {
        match msg {
            TunnelMessage::ProxiedRequest {
                request_id,
                principal,
                payload,
            } => {
                self.handle_proxied_request(request_id, principal, payload, handle)
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
            // look up the local agent by `target_principal_did`,
            // attribute the dispatch under
            // `Subject::Principal(caller_principal_did)`, run it, and send
            // back an `AgentToAgentResponse` carrying the
            // `PrincipalSendResult` payload.
            TunnelMessage::AgentToAgentRequest {
                request_id,
                caller_runtime_id,
                caller_principal_did,
                target_principal_did,
                message,
                signature,
            } => {
                self.handle_inbound_agent_to_agent_request(
                    handle,
                    request_id,
                    caller_runtime_id,
                    caller_principal_did,
                    target_principal_did,
                    message,
                    signature,
                )
                .await?;
            }
            // Inbound `AgentToAgentResponse` for a request the
            // outbound `PrincipalSendTool` path registered in the pending
            // registry. Complete the oneshot so the outbound
            // `execute_remote` unblocks and decodes the payload.
            TunnelMessage::AgentToAgentResponse {
                request_id,
                payload,
            } => {
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
        principal_name: String,
        payload: Vec<u8>,
        handle: TunnelHandle,
    ) -> anyhow::Result<()> {
        debug!(
            "Handling proxied request {} for principal {}",
            request_id, principal_name
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
            .check_request_allowed(&principal_name, &bridge_payload)
            .await
        {
            warn!("Tunnel ACL denied request for {}: {}", principal_name, e);
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
        let caller_user =
            match resolve_bridge_caller(&bridge_payload, self.app_state.jwt_validator().as_ref())
                .await
            {
                Ok(caller) => caller,
                Err(e) => {
                    warn!(
                        "Tunnel caller resolution failed for {}: {}",
                        principal_name, e
                    );
                    return self
                        .send_error_response(&handle, &request_id, &format!("Forbidden: {}", e))
                        .await;
                }
            };
        let caller_principal = Subject::from_bridge_user(&caller_user);

        // The audit emit's `agent_did` positional argument is the
        // AuditEvent struct's legacy `agent_did: Option<String>`
        // field (column name kept for the Drizzle migration tracking
        // issue). The principal name is the runtime's view of the
        // same identifier — pass it through; downstream audit
        // consumers can re-key on the `caller` Subject kind.
        self.app_state
            .observability()
            .audit_with_caller(
                Some(&caller_principal),
                "tunnel_proxied_request",
                Some(&principal_name),
                serde_json::json!({
                    "request_id": &request_id,
                    "caller": &caller_user,
                }),
            )
            .await
            .ok();

        let principal_manager = self.app_state.principal_manager();
        let principal = match principal_manager.get_by_name(&principal_name).await {
            Some(p) => p,
            None => {
                warn!("Proxied request for unknown principal: {}", principal_name);
                return self
                    .send_error_response(&handle, &request_id, "Principal not found")
                    .await;
            }
        };

        // Real end-to-end streaming. We drive the principal through
        // `receive_streaming`, which preserves the same permission
        // checks, session recall, and per-peer serial queue as the
        // one-shot `receive` path, but emits `AgenticEvent`s as token
        // deltas are produced.
        //
        // The wire contract with PekoHub is intentionally simple: each
        // `StreamChunk` payload is a **raw UTF-8 text fragment** of the
        // assistant's answer (NOT JSON). The hub re-frames each fragment
        // into its own SSE `data:` envelope. A terminating `StreamEnd`
        // marks completion. This avoids the previous double-encoding bug
        // where the runtime JSON-wrapped chunks that the hub then wrapped
        // again.
        let channel = crate::principal::router::ChannelContext {
            kind: crate::principal::router::ChannelKind::Hub,
            streaming: true,
        };

        // Bounded channel: a slow tunnel back-pressures the root agent
        // (events drop on `try_send` failure rather than growing memory).
        let (event_tx, mut event_rx) =
            tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(256);
        let on_event: Box<dyn Fn(crate::engine::AgenticEvent) + Send + Sync> =
            Box::new(move |event| {
                let _ = event_tx.try_send(event);
            });

        // Run the principal in a background task. When it finishes, the
        // `event_tx` clone it holds is dropped, closing the channel and
        // signalling the drain loop below to stop.
        let pm = principal_manager.clone();
        let principal_id = principal.id.clone();
        let recv_handle = tokio::spawn(async move {
            pm.receive_streaming(principal_id, caller_principal, message, channel, on_event)
                .await
        });

        // Forward each token delta as a raw-text `StreamChunk`.
        let mut seq: u32 = 0;
        let mut streamed_any = false;
        while let Some(event) = event_rx.recv().await {
            let delta = match event {
                crate::engine::AgenticEvent::AssistantDelta { text, .. } => text,
                crate::engine::AgenticEvent::AssistantText { text, .. } => text,
                _ => continue,
            };
            if delta.is_empty() {
                continue;
            }
            if let Err(e) = handle.send_stream_chunk(request_id.clone(), seq, delta.into_bytes()) {
                warn!("Failed to send stream chunk; aborting stream: {}", e);
                recv_handle.abort();
                let _ = handle.send_stream_end(request_id);
                return Ok(());
            }
            seq += 1;
            streamed_any = true;
        }

        // Channel closed → the principal task finished. Recover the
        // authoritative final answer.
        let result = match recv_handle.await {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "Principal streaming task panicked for {}: {}",
                    principal_name, e
                );
                return self
                    .send_error_response(&handle, &request_id, "Execution failed")
                    .await;
            }
        };

        match result {
            Ok(response) => {
                // If no deltas were streamed (e.g. the queued path, or a
                // router that produced no incremental events), fall back
                // to sending the authoritative content as a single chunk
                // so the caller still receives the full answer.
                if !streamed_any && !response.content.is_empty() {
                    if let Err(e) = handle.send_stream_chunk(
                        request_id.clone(),
                        seq,
                        response.content.into_bytes(),
                    ) {
                        warn!("Failed to send fallback chunk: {}", e);
                    }
                }
                if let Err(e) = handle.send_stream_end(request_id) {
                    warn!("Failed to send stream end: {}", e);
                }
            }
            Err(e) => {
                warn!("Principal execution failed for {}: {}", principal_name, e);
                return self
                    .send_error_response(&handle, &request_id, &format!("Execution failed: {}", e))
                    .await;
            }
        }

        Ok(())
    }

    /// Send an error response back through the tunnel.
    ///
    /// Errors are sent as a single raw-text `StreamChunk` followed by a
    /// `StreamEnd`, matching the raw-text streaming contract used by the
    /// happy path. The hub surfaces the text to the SSE consumer.
    async fn send_error_response(
        &self,
        handle: &TunnelHandle,
        request_id: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        if let Err(e) =
            handle.send_stream_chunk(request_id.to_string(), 0, message.as_bytes().to_vec())
        {
            warn!("Failed to send error response chunk: {}", e);
        }
        if let Err(e) = handle.send_stream_end(request_id.to_string()) {
            warn!("Failed to send error stream end: {}", e);
        }
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

    /// Re-push the current allow-list for a Principal instance to PekoHub by
    /// re-announcing the instance. The `allowed_principals` list is freshly
    /// derived from the Principal's `permissions`. Used by the
    /// permit/revoke IPC paths so that PekoHub's `canChat` ACL and the
    /// runtime's defense-in-depth `instance_state.allowed_principals` cache
    /// are kept in sync with the local config without requiring a daemon
    /// restart.
    ///
    /// No-ops if:
    /// - the Principal has no cached `instance_state` (tunnel not yet
    ///   connected, or instance never announced). The next
    ///   `announce_instances` after `TunnelReady` will pick up the
    ///   latest config.
    /// - the current exposure is not `Private` (Public/Unexposed
    ///   Principals don't carry an `allowed_principals` list, and we must not
    ///   silently flip the exposure as a side effect of a permit call).
    pub async fn refresh_instance_allowed_principals(
        &self,
        principal_name: &str,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(principal_name);
        let exposure = {
            let state = self.state.read().await;
            state
                .instance_state
                .get(&instance_id)
                .map(|s| s.exposure.clone())
        };
        match exposure {
            Some(e) if e == InstanceExposure::Private => {}
            Some(e) => {
                debug!(
                    "Skipping allowed_principals refresh for {}: exposure is {:?}, not Private",
                    principal_name, e
                );
                return Ok(());
            }
            None => {
                debug!(
                    "Skipping allowed_principals refresh for {}: no cached instance state \
                     (tunnel not yet connected or instance not announced)",
                    principal_name
                );
                return Ok(());
            }
        };
        self.announce_single_instance(principal_name).await
    }

    /// Build and send an `ExposureUpdate` for the given Principal, with
    /// `allowed_principals` re-derived from the live Principal config.
    /// The caller is responsible for ensuring the current exposure is
    /// meaningful (i.e. `Private`) and that local `instance_state`
    /// reflects the desired exposure before invoking.
    async fn send_exposure_update(
        &self,
        principal_name: &str,
        exposure: InstanceExposure,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(principal_name);
        let allowed_principals = if exposure == InstanceExposure::Private {
            let principal_manager = self.app_state.principal_manager();
            match principal_manager.get_by_name(principal_name).await {
                Some(principal) => {
                    let config = principal.config.read().await;
                    Self::compute_allowed_principals(&config)
                }
                None => {
                    warn!(
                        "Principal {} not found; cannot compute allowed principals",
                        principal_name
                    );
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
                allowed_principals: allowed_principals.clone(),
            };
            if let Err(e) = handle.send(TunnelMessage::ExposureUpdate { payload }) {
                warn!(
                    "Failed to send exposure update for {}: {}",
                    principal_name, e
                );
                return Err(e.into());
            }
            debug!(
                "Sent exposure update for {}: {:?}",
                principal_name, exposure
            );
        } else {
            debug!(
                "No tunnel handle, exposure update for {} is dropped (will be re-announced on next TunnelReady)",
                principal_name
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
            entry.allowed_principals = payload.allowed_principals.clone().unwrap_or_default();
        } else {
            state.instance_state.insert(
                payload.instance_id.clone(),
                InstanceState {
                    exposure: payload.exposure.clone(),
                    allowed_principals: payload.allowed_principals.clone().unwrap_or_default(),
                    status: InstanceStatus::Online,
                },
            );
        }
        drop(state);

        // Re-announce the Principal instance to confirm the change.
        let principal_manager = self.app_state.principal_manager();
        let principals = principal_manager.list_all().await;

        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };

        if let Some(handle) = handle {
            for principal in principals {
                let name = principal.name().await;
                let instance_id = self.instance_id(&name);
                if instance_id == payload.instance_id {
                    let did = principal.did().await;
                    let status = self.get_instance_status(&name).await;
                    let transport_preference = principal.config.read().await.transport_preference;
                    let announce_payload = InstanceAnnouncePayload {
                        id: instance_id,
                        instance_type: InstanceType::Principal,
                        name: name.clone(),
                        agent_did: None,
                        principal_did: Some(did.0),
                        bundle_ref: None,
                        runtime_display_name: Some(self.runtime_display_name.clone()),
                        status,
                        exposure: payload.exposure.clone(),
                        allowed_principals: payload.allowed_principals.clone(),
                        capabilities: None,
                        metadata: None,
                        transport_preference: Some(transport_preference),
                        runtime_direct_endpoint: self
                            .app_state
                            .peko_config
                            .network
                            .direct
                            .advertise_endpoint
                            .clone(),
                    };
                    if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce {
                        payload: announce_payload,
                    }) {
                        warn!(
                            "Failed to re-announce instance {} after exposure update: {}",
                            name, e
                        );
                    } else {
                        debug!(
                            "Re-announced principal instance {} after exposure update",
                            name
                        );
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
    /// 3. Look up the local agent by `target_principal_did`.
    /// 4. Build a `MessageRequest` with `caller_principal =
    ///    Subject::Principal(caller_principal_did)` (issue #24 + #28).
    /// 5. Dispatch via `StatelessAgentService`.
    /// 6. Serialize the result to `PrincipalSendResult` and send back via
    ///    the same tunnel as an `AgentToAgentResponse`.
    ///
    /// Every error path sends a structured `HubErrorResponse`
    /// back to the caller so the caller can distinguish "target
    /// not found" from "target rejected me" from "I'm broken"
    /// rather than waiting for a timeout.
    #[allow(clippy::too_many_arguments)]
    async fn handle_inbound_agent_to_agent_request(
        &self,
        handle: TunnelHandle,
        request_id: String,
        caller_runtime_id: String,
        caller_principal_did: String,
        target_principal_did: String,
        message: String,
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
        //
        // NOTE: `SignedFields` no longer includes `session_id`. ADR-042
        // dropped `session_id` from the cross-runtime wire envelope; it
        // remains local-storage correlation only (see
        // `tunnel::a2a_audit`) and is NOT signed.
        let signed = SignedFields {
            request_id: &request_id,
            caller_runtime_id: &caller_runtime_id,
            caller_principal_did: &caller_principal_did,
            target_principal_did: &target_principal_did,
            message: &message,
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

        // 3. Look up the local Principal by target_principal_did (which is the
        // Principal's stable DID in the new single-actor model).
        let principal_manager = self.app_state.principal_manager();
        let local_principal = match principal_manager.find_by_did(&target_principal_did).await {
            Some(p) => p,
            None => {
                return self
                    .send_hub_error(
                        &handle,
                        &request_id,
                        "target_not_found",
                        &format!(
                            "no local principal has did={target_principal_did} (request_id={request_id})"
                        ),
                    )
                    .await;
            }
        };

        // Slice D: emit the inbound-receive audit event now that
        // the request has been verified and the Principal has been located.
        let local_runtime_id = self.app_state.runtime_identity().runtime_did.clone();
        let received_event = a2a_audit::build_a2a_received_inbound(
            "", // session_id
            &request_id,
            &caller_runtime_id,
            &caller_principal_did,
            &local_runtime_id,
            &target_principal_did,
            &message,
        );
        a2a_audit::emit_a2a_received(&received_event);

        // 4 + 5. Dispatch to the Principal.
        let caller_principal = Subject::Principal(caller_principal_did.clone().into());
        let channel = crate::principal::router::ChannelContext {
            kind: crate::principal::router::ChannelKind::A2a,
            streaming: false,
        };

        let result = principal_manager
            .receive(
                local_principal.id.clone(),
                caller_principal,
                message.clone(),
                channel,
            )
            .await;

        // 6. Serialize and respond.
        //
        // `PrincipalSendResult.session_id` is local-storage correlation
        // for the receiving runtime. Cross-runtime, the inbound
        // dispatcher never sees the caller's session_id (it was dropped
        // from the wire envelope per ADR-042), so the response carries
        // an empty string — the receiving runtime may internally index
        // the exchange for its own audit log, but the value is not
        // round-tripped back through the response payload.
        let a2a_result = match result {
            Ok(response) => PrincipalSendResult {
                success: true,
                response: response.content,
                session_id: String::new(),
                iterations: None,
                tool_calls: None,
                duration_ms: None,
                error: None,
            },
            Err(e) => PrincipalSendResult {
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
                        &format!("failed to serialize PrincipalSendResult: {e}"),
                    )
                    .await;
            }
        };

        // Slice D: emit the response-side audit event before sending.
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
            &caller_principal_did,
            &local_runtime_id,
            &target_principal_did,
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
    /// outbound `PrincipalSendTool` is awaiting on. Issue #29 Slice C.
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

    /// Synthesize a `HubErrorResponse` and send it back to the
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
        let payload = serde_json::to_vec(&HubErrorResponse {
            kind: "error".to_string(),
            code: code.to_string(),
            message: message.to_string(),
        })
        .map_err(|e| anyhow::anyhow!("failed to serialize HubErrorResponse: {e}"))?;
        handle.send(TunnelMessage::AgentToAgentResponse {
            request_id: request_id.to_string(),
            payload,
        })?;
        Ok(())
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
                // was never set. Default to denying (security fix).
                anyhow::bail!("Instance state not yet available; request denied");
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

                if instance_state
                    .allowed_principals
                    .iter()
                    .any(|s| matches!(s, crate::auth::Subject::User(id) if id == user_id))
                {
                    Ok(())
                } else {
                    warn!(
                        agent_name,
                        user_id, "Private instance request denied: user not in allowed_principals"
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
    use crate::auth::{Permission, PermissionGrant, Subject};
    use crate::daemon::state::{AppState, DaemonConfigSnapshot};
    use crate::principal::config::{
        PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use crate::principal::Capabilities;
    use crate::tunnel::protocol::{InstanceExposure, InstanceType};
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

    fn mock_tunnel_handle() -> (TunnelHandle, mpsc::Receiver<TunnelMessage>) {
        let (tx, rx) = mpsc::channel(crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE);
        (TunnelHandle::new(tx), rx)
    }

    fn chat_grant(subject: Subject) -> PermissionGrant {
        PermissionGrant {
            subject,
            permission: Permission::Chat,
            granted_at: "2026-06-30T00:00:00Z".to_string(),
            granted_by: Subject::User("user:owner".to_string()),
        }
    }

    /// `compute_allowed_principals` keeps both `User` and `Principal`
    /// Chat grants (A2A callers must survive), strips the legacy `user:`
    /// prefix from user ids, drops `Public`, and drops non-`Chat` grants.
    #[test]
    fn compute_allowed_principals_keeps_users_and_principals() {
        let config = test_principal_config(
            "p",
            Subject::User("user:owner".to_string()),
            vec![
                chat_grant(Subject::User("user:alice".to_string())),
                chat_grant(Subject::Principal("did:peko:agent:bob".to_string().into())),
                chat_grant(Subject::Public),
                // Non-Chat grant must be ignored entirely.
                PermissionGrant {
                    subject: Subject::User("user:carol".to_string()),
                    permission: Permission::ViewSettings,
                    granted_at: "2026-06-30T00:00:00Z".to_string(),
                    granted_by: Subject::User("user:owner".to_string()),
                },
            ],
            InstanceExposure::Private,
        );

        let allowed = TunnelDispatcher::compute_allowed_principals(&config).expect("always Some");

        // Alice survives with the `user:` prefix stripped.
        assert!(
            allowed.contains(&Subject::User("alice".to_string())),
            "user grant must survive prefix-stripped; got {allowed:?}"
        );
        // Bob (A2A principal) must survive verbatim — this is the #1 fix.
        assert!(
            allowed.contains(&Subject::Principal("did:peko:agent:bob".to_string().into())),
            "principal grant must survive; got {allowed:?}"
        );
        // Public is never a named allow-list entry.
        assert!(
            !allowed.iter().any(|s| matches!(s, Subject::Public)),
            "public must be dropped; got {allowed:?}"
        );
        // Carol's ViewSettings grant must not leak in.
        assert!(
            !allowed.iter().any(|s| s.subject_id() == "carol"),
            "non-Chat grant must be dropped; got {allowed:?}"
        );
        assert_eq!(allowed.len(), 2, "exactly alice + bob; got {allowed:?}");
    }

    /// Build a minimal `PrincipalConfig` for dispatcher tests.
    fn test_principal_config(
        name: &str,
        owner: Subject,
        permissions: Vec<PermissionGrant>,
        exposure: InstanceExposure,
    ) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: None,
            owner,
            identity: PrincipalIdentityConfig {
                display_name: Some(name.to_string()),
                description: Some(format!("Test principal {name}")),
                avatar: None,
            },
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            capabilities: Capabilities::default(),
            exposure,
            status: None,
            permissions,
            preferred_provider_id: None,
            preferred_model_id: None,
            transport_preference: Default::default(),
        }
    }

    /// Create a Principal inside the given test `AppState`, including a
    /// default agent prompt so `PrincipalManager::create` succeeds.
    async fn create_test_principal(
        app_state: &AppState,
        name: &str,
        owner: Subject,
        permissions: Vec<PermissionGrant>,
        exposure: InstanceExposure,
    ) -> std::sync::Arc<crate::principal::Principal> {
        let workspace = app_state.config.data_dir.join("principals").join(name);
        let agents_dir = workspace.join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        tokio::fs::write(
            agents_dir.join("primary.md"),
            format!("---\ndescription: \"Test agent for {name}\"\n---\n\nYou are {name}.\n"),
        )
        .await
        .unwrap();

        let config = test_principal_config(name, owner, permissions, exposure);
        app_state
            .principal_manager()
            .create(config)
            .await
            .expect("principal should be created")
    }

    // ─── Phase 2: Principal tunnel exposure ───────────────────────────────

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
        assert_eq!(
            resolve_bridge_caller(&payload, None).await.unwrap(),
            "user-42"
        );
    }

    /// Missing header → rejected (no anonymous callers).
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_anonymous_when_header_missing() {
        let payload = serde_json::json!({"body": {"message": "hi"}});
        assert_eq!(
            resolve_bridge_caller(&payload, None).await,
            Err(BridgeCallerError::NoCaller)
        );
    }

    /// Empty string header → rejected.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_anonymous_when_header_empty() {
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": ""},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, None).await,
            Err(BridgeCallerError::NoCaller)
        );
    }

    /// Whitespace-only header → rejected.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_anonymous_when_header_whitespace() {
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": "   "},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, None).await,
            Err(BridgeCallerError::NoCaller)
        );
    }

    /// Non-string header (e.g. number) → rejected.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_anonymous_when_header_not_string() {
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": 12345},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, None).await,
            Err(BridgeCallerError::NoCaller)
        );
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
            resolve_bridge_caller(&payload, Some(&validator))
                .await
                .unwrap(),
            "user-jwt"
        );
    }

    /// Tampered JWT (signature doesn't verify) → rejected instead of
    /// falling back to the unverified header.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_tampered_jwt() {
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
            Err(BridgeCallerError::InvalidJwt)
        );
    }

    /// JWT present but no validator configured → rejected instead of
    /// trusting the unverified hub header.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_when_no_validator_configured() {
        let (_validator, signing_key) = ed25519_validator();
        let jwt = mint_jwt(&signing_key, "user-jwt");

        let payload = serde_json::json!({
            "headers": {
                "Authorization": format!("Bearer {jwt}"),
                "x-pekohub-user-id": "user-hub",
            },
        });
        assert_eq!(
            resolve_bridge_caller(&payload, None).await,
            Err(BridgeCallerError::InvalidJwt)
        );
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
            resolve_bridge_caller(&payload, Some(&validator))
                .await
                .unwrap(),
            "user-hub"
        );
    }

    /// No JWT and no header → rejected (no anonymous callers).
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_anonymous() {
        let (validator, _signing_key) = ed25519_validator();
        let payload = serde_json::json!({"headers": {}});
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            Err(BridgeCallerError::NoCaller)
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
            resolve_bridge_caller(&payload, Some(&validator))
                .await
                .unwrap(),
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
            allowed_principals: Some(vec![crate::auth::Subject::User("user-1".to_string())]),
        };

        dispatcher.handle_exposure_update(payload).await.unwrap();

        // Verify local state was updated
        let state = dispatcher.state.read().await;
        let entry = state
            .instance_state
            .get(&instance_id)
            .expect("Instance state should exist");
        assert_eq!(entry.exposure, InstanceExposure::Public);
        assert_eq!(
            entry.allowed_principals,
            vec![crate::auth::Subject::User("user-1".to_string())]
        );
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
                    allowed_principals: vec![],
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
                    allowed_principals: vec![],
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
                    allowed_principals: vec![crate::auth::Subject::User("user-123".to_string())],
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
                    allowed_principals: vec![crate::auth::Subject::User("user-123".to_string())],
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
                    allowed_principals: vec![crate::auth::Subject::User("user-123".to_string())],
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

    #[tokio::test]
    async fn test_check_request_allowed_missing_state_denies() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state);

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-123"}});
        let result = dispatcher
            .check_request_allowed("unknown-agent", &bridge_payload)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Instance state not yet available"));
    }

    // -- Issue #29 (Slice C): inbound AgentToAgentRequest + Response -----

    /// `handle_inbound_agent_to_agent_request` rejects a request with
    /// a malformed caller_runtime_id (cannot be parsed as a did:key)
    /// by sending back an `internal_error` `HubErrorResponse`
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
                "hi".to_string(),
                "sig".to_string(),
            )
            .await
            .expect("handler must not panic; errors are reported via the response");

        // The handler should have sent back a structured
        // HubErrorResponse. Drain the response and check the shape.
        let response = rx.recv().await.expect("response must be sent");
        let TunnelMessage::AgentToAgentResponse {
            request_id,
            payload,
        } = response
        else {
            panic!("expected AgentToAgentResponse, got: {response:?}");
        };
        assert_eq!(request_id, "req-malformed");
        let err: HubErrorResponse =
            serde_json::from_slice(&payload).expect("payload must be a HubErrorResponse");
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
    /// `HubErrorResponse`.
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
            caller_principal_did: "did:peko:agent:caller",
            target_principal_did: "did:peko:agent:target",
            message: "hi",
        };
        let sig = crate::tunnel::sign_request(&kp_attacker.signing_key, signed);

        dispatcher
            .handle_inbound_agent_to_agent_request(
                handle,
                "req-bad-sig".to_string(),
                caller_did,
                "did:peko:agent:caller".to_string(),
                "did:peko:agent:target".to_string(),
                "hi".to_string(),
                sig,
            )
            .await
            .expect("handler must not panic");

        let response = rx.recv().await.expect("response must be sent");
        let TunnelMessage::AgentToAgentResponse {
            request_id,
            payload,
        } = response
        else {
            panic!("expected AgentToAgentResponse, got: {response:?}");
        };
        assert_eq!(request_id, "req-bad-sig");
        let err: HubErrorResponse = serde_json::from_slice(&payload).expect("payload must decode");
        assert_eq!(err.code, "forbidden");
        assert!(
            err.message.contains("signature did not verify"),
            "error must name the cause; got: {}",
            err.message
        );
    }

    /// `handle_inbound_agent_to_agent_response` completes the
    /// matching pending oneshot on the `PendingA2aResponses`
    /// registry so the outbound `PrincipalSendTool` awaiter unblocks.
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
            .handle_inbound_agent_to_agent_response("req-1".to_string(), b"hello".to_vec())
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

    // ─── Phase 2: Principal tunnel exposure tests ─────────────────────────

    /// Principals are announced over the tunnel as `InstanceType::Principal`
    /// with their stable DID populated.
    #[tokio::test]
    async fn announce_principal_instance() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state.clone());
        let (handle, mut rx) = mock_tunnel_handle();

        let principal = create_test_principal(
            &app_state,
            "announce-me",
            Subject::User("user:owner".to_string()),
            vec![],
            InstanceExposure::Public,
        )
        .await;
        let did = principal.did().await;

        dispatcher.announce_instances(&handle).await.unwrap();

        let msg = rx.recv().await.expect("announce message must be sent");
        let TunnelMessage::InstanceAnnounce { payload } = msg else {
            panic!("expected InstanceAnnounce, got: {msg:?}");
        };
        assert_eq!(payload.instance_type, InstanceType::Principal);
        assert_eq!(payload.name, "announce-me");
        assert_eq!(payload.principal_did, Some(did.0));
        assert_eq!(payload.agent_did, None);

        let instance_id = dispatcher.instance_id("announce-me");
        let state = dispatcher.state.read().await;
        let cached = state
            .instance_state
            .get(&instance_id)
            .expect("state seeded");
        assert_eq!(cached.exposure, InstanceExposure::Public);
    }

    /// A `ProxiedRequest` from PekoHub is routed to the matching Principal
    /// and the response (or execution error) is streamed back.
    #[tokio::test]
    async fn proxied_request_routes_to_principal() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state.clone());
        let (handle, mut rx) = mock_tunnel_handle();

        create_test_principal(
            &app_state,
            "web-bound",
            Subject::User("user:test-user".to_string()),
            vec![],
            InstanceExposure::Public,
        )
        .await;

        let bridge_payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": "test-user"},
            "body": {"message": "hello principal"},
        });

        dispatcher
            .handle_proxied_request(
                "req-web".to_string(),
                "web-bound".to_string(),
                bridge_payload.to_string().into_bytes(),
                handle,
            )
            .await
            .expect("handler must not panic");

        let mut got_chunk = false;
        while let Some(msg) = rx.recv().await {
            match msg {
                TunnelMessage::StreamChunk { request_id, .. } => {
                    assert_eq!(request_id, "req-web");
                    got_chunk = true;
                }
                TunnelMessage::StreamEnd { request_id } => {
                    assert_eq!(request_id, "req-web");
                    break;
                }
                _ => {}
            }
        }
        assert!(
            got_chunk,
            "proxied request must produce at least one stream chunk"
        );
    }

    /// An inbound `AgentToAgentRequest` addressed to a Principal's stable DID
    /// is routed to that Principal and a structured response is sent back.
    #[tokio::test]
    async fn inbound_a2a_routes_to_principal_by_did() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(app_state.clone());
        let (handle, mut rx) = mock_tunnel_handle();

        let kp_caller = crate::identity::keys::KeyPair::generate();
        let caller_runtime_id = crate::tunnel::verifying_key_to_did_key(&kp_caller.verifying_key);
        let caller_principal_did = "did:peko:agent:caller".to_string();

        let principal = create_test_principal(
            &app_state,
            "a2a-target",
            Subject::User("user:owner".to_string()),
            vec![PermissionGrant {
                subject: Subject::Principal(caller_principal_did.clone().into()),
                permission: Permission::Chat,
                granted_at: "2026-06-27T00:00:00Z".to_string(),
                granted_by: Subject::User("user:owner".to_string()),
            }],
            InstanceExposure::Public,
        )
        .await;
        let target_principal_did = principal.did().await.0;

        let signed = crate::tunnel::SignedFields {
            request_id: "req-a2a",
            caller_runtime_id: &caller_runtime_id,
            caller_principal_did: &caller_principal_did,
            target_principal_did: &target_principal_did,
            message: "ping",
        };
        let sig = crate::tunnel::sign_request(&kp_caller.signing_key, signed);

        dispatcher
            .handle_inbound_agent_to_agent_request(
                handle,
                "req-a2a".to_string(),
                caller_runtime_id,
                caller_principal_did,
                target_principal_did,
                "ping".to_string(),
                sig,
            )
            .await
            .expect("handler must not panic");

        let response = rx.recv().await.expect("response must be sent");
        let TunnelMessage::AgentToAgentResponse {
            request_id,
            payload,
        } = response
        else {
            panic!("expected AgentToAgentResponse, got: {response:?}");
        };
        assert_eq!(request_id, "req-a2a");
        let result: PrincipalSendResult =
            serde_json::from_slice(&payload).expect("payload must decode");
        assert!(
            result.error.is_some() || !result.response.is_empty() || result.success,
            "A2A result should reflect that the Principal was reached; got: {result:?}"
        );
    }
}
