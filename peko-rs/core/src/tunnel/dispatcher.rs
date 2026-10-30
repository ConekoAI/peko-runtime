//! Tunnel Request Dispatcher
//!
//! Bridges proxied requests from the PekoHub tunnel to the daemon's service layer.
//! Handles chat execution, streaming responses, and instance lifecycle messages.

use peko_channel::ChannelPort;
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

use peko_auth::ActorRef;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

use super::host::TunnelHost;
use super::protocol::{
    ExposureUpdatePayload, InstanceAnnouncePayload, InstanceExposure, InstanceHeartbeatPayload,
    InstanceStatus, InstanceType, StatusUpdatePayload, TunnelMessage,
};
use super::TunnelHandle;
use super::{
    did_key::did_key_to_verifying_key,
    tunnel_channel_audit,
    tunnel_channel_signature::{
        verify_author_signature, verify_channel_event, verify_channel_invite,
    },
};

use peko_auth::ownership::Permission;

/// Errors returned by [`resolve_bridge_caller`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BridgeCallerError {
    /// A JWT was presented but could not be validated.
    InvalidJwt,
    /// The validated JWT is missing the typed `kind` claim (ADR-058
    /// D5), or the claim is not `"user"` / `"visitor"`, or the `sub`
    /// would alias another identity namespace.
    InvalidKind,
    /// No verified or unverified caller could be determined.
    NoCaller,
}

impl std::fmt::Display for BridgeCallerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeCallerError::InvalidJwt => write!(f, "invalid JWT"),
            BridgeCallerError::InvalidKind => {
                write!(f, "JWT is missing a valid typed `kind` claim")
            }
            BridgeCallerError::NoCaller => write!(f, "no caller identity provided"),
        }
    }
}

impl std::error::Error for BridgeCallerError {}

/// Resolve the calling subject from a PekoHub-proxied bridge payload
/// (issue #17, ADR-058 D5).
///
/// PekoHub is the security boundary: it must set `Authorization: Bearer <jwt>`
/// on every proxied request. When a JWT is present and a `JwtValidator` is
/// configured, this function validates the JWT (signature, audience,
/// issuer, expiry) and maps the validated `(kind, sub)` claims into a
/// typed [`ActorRef`] via [`ActorRef::from_bridge_claim`]:
///
/// - `kind: "user"` → `ActorRef::User(sub)` (a hub account id);
/// - `kind: "visitor"` → `ActorRef::Visitor(sub)` (a hub-minted
///   anonymous id — never a user, principal, or the reserved `local`).
///
/// ADR-058 D5 made the `kind` claim mandatory (fail-closed): a
/// validated token without a recognized `kind` is rejected with
/// [`BridgeCallerError::InvalidKind`], and a `sub` containing `:` (or
/// equal to `local` for visitors) is rejected as a namespace-aliasing
/// attempt. A bridge token can never again produce a principal
/// subject — the retired `from_bridge_user` string coercion is gone.
///
/// If a JWT is present but validation fails, or if no validator is
/// configured for a JWT-present request, the function returns
/// [`BridgeCallerError::InvalidJwt`] so the request can be rejected
/// instead of silently trusting a tampered token.
pub(crate) async fn resolve_bridge_caller(
    bridge_payload: &serde_json::Value,
    jwt_validator: Option<&peko_auth::jwt::JwtValidator>,
) -> Result<ActorRef, BridgeCallerError> {
    // ADR-057 + ADR-058 D5: the caller identity is ALWAYS a signed
    // EdDSA bridge token minted by PekoHub (typed `kind` claim + `sub`,
    // aud = this runtime's DID). A runtime with a tunnel configured
    // rejects every bridge request that does not carry a token its
    // validator accepts.
    let Some(jwt) = extract_bearer_jwt(bridge_payload) else {
        return Err(BridgeCallerError::NoCaller);
    };
    let Some(validator) = jwt_validator else {
        warn!(
            "Authorization: Bearer <jwt> present but no JWT validator configured; \
             rejecting request"
        );
        return Err(BridgeCallerError::InvalidJwt);
    };
    match validator.validate(&jwt).await {
        Ok(validated) => {
            let Some(kind) = validated.kind.as_deref() else {
                warn!(
                    "validated bridge JWT carries no `kind` claim; rejecting request (ADR-058 D5)"
                );
                return Err(BridgeCallerError::InvalidKind);
            };
            ActorRef::from_bridge_claim(kind, &validated.sub).map_err(|e| {
                warn!(
                    "validated bridge JWT failed the typed-claim gate ({}); rejecting request",
                    e
                );
                BridgeCallerError::InvalidKind
            })
        }
        Err(e) => {
            warn!("JWT validation failed ({}); rejecting request", e);
            Err(BridgeCallerError::InvalidJwt)
        }
    }
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
    /// Current instance status
    pub status: InstanceStatus,
}

impl Default for InstanceState {
    fn default() -> Self {
        Self {
            exposure: InstanceExposure::Private,
            status: InstanceStatus::Online,
        }
    }
}

/// Shared dispatcher state for instance lifecycle management
#[derive(Debug, Default)]
pub struct TunnelDispatcherState {
    /// Whether the tunnel is authenticated and ready
    pub registration_receipts: std::collections::HashMap<String, Result<String, String>>,
    /// ADR-081 D4: Hub's answers to transfer requests, by request ID.
    pub transfer_receipts: std::collections::HashMap<String, Result<(), String>>,
    pub pending_registrations: std::collections::HashMap<String, String>,
    pub ready: bool,
    /// Heartbeat interval from server (seconds)
    pub heartbeat_interval_secs: u32,
    /// Local instance state cache: instance_id -> state (updated by exposure_update and status_update)
    pub instance_state: std::collections::HashMap<String, InstanceState>,
    /// Current tunnel handle for sending messages
    pub tunnel_handle: Option<TunnelHandle>,
    /// The connection's instance heartbeat task; replaced on every
    /// `TunnelReady`, stopped on disconnect.
    pub heartbeat_task: Option<tokio::task::AbortHandle>,
}

/// Dispatches tunnel messages to daemon services.
///
/// `Clone` is a shallow copy: the `Arc<RwLock<...>>` is shared, which is
/// intentional because the dispatcher is moved into spawned tasks.
#[derive(Clone)]
pub struct TunnelDispatcher {
    host: Arc<dyn TunnelHost>,
    state: Arc<RwLock<TunnelDispatcherState>>,
    runtime_display_name: String,
    /// Slot the dispatcher writes the live tunnel handle to on
    /// every inbound message. The cross-runtime channel ctx
    /// (`CrossRuntimeChannelCtx`) holds a clone of this `Arc` and
    /// reads the handle when fanning out outbound channel envelopes,
    /// so the outbound path always uses the most-recent handle without
    /// having to be re-built on reconnect. `None` until the first
    /// inbound message lands.
    tunnel_handle_slot: Arc<tokio::sync::RwLock<Option<TunnelHandle>>>,
    /// Bounds the number of concurrently-dispatched inbound messages.
    /// Shared across clones (each inbound message clones the dispatcher
    /// into its task) so the cap is global, not per-task. See
    /// [`MAX_CONCURRENT_DISPATCHES`].
    inbound_semaphore: Arc<Semaphore>,
    registration_lock: Arc<tokio::sync::Mutex<()>>,
}

/// A principal's Hub ID, from the actor registry (ADR-081 §3).
async fn manager_hub_id(
    host: &Arc<dyn TunnelHost>,
    principal: &crate::principal::Principal,
) -> Option<String> {
    host.tunnel_channel_port()
        .actor_info(&ActorRef::from(&principal.id))
        .await
        .hub_id
}

impl TunnelDispatcher {
    /// Create a new tunnel dispatcher bound to the given host (the daemon's
    /// `AppState`, behind the `TunnelHost` seam — F5).
    pub fn new(host: Arc<dyn TunnelHost>) -> Self {
        // Pull the tunnel handle slot FIRST (clones the `Arc`,
        // not the handle) so the field move into the struct
        // below is the final use of the host for that value.
        let tunnel_handle_slot = host.tunnel_handle_slot();
        let runtime_display_name = host.runtime_display_name();
        Self {
            host,
            state: Arc::new(RwLock::new(TunnelDispatcherState::default())),
            runtime_display_name,
            // Mirror the host's slot so the dispatcher publishes
            // and the ctx reads from the SAME `Arc<RwLock<...>>`. The
            // getter (`tunnel_handle_slot`) returns the exact `Arc`;
            // cloning it here is a no-op refcount bump.
            tunnel_handle_slot,
            inbound_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_DISPATCHES)),
            registration_lock: Arc::new(tokio::sync::Mutex::new(())),
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
        // outbound `CrossRuntimeChannelCtx` fan-out can send on the
        // most-recent tunnel. Doing this synchronously (not under the
        // spawn boundary) means the ctx sees the new handle before any
        // channel fan-out started by the inbound message could race.
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
        if let Some(task) = state.heartbeat_task.take() {
            task.abort();
        }
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
        let name = format!("{}:{}", self.host.runtime_did(), agent_name);
        uuid::Uuid::new_v5(&INSTANCE_ID_NAMESPACE, name.as_bytes()).to_string()
    }

    /// ADR-058 D4: mint the `principalPop` proof-of-possession for an
    /// `instance_announce` — a compact JWS (EdDSA) over
    /// `{"runtimeId":..,"principalDid":..,"iat":..,"exp":..}` with
    /// `iat = now`, `exp = now + 300`, signed with the principal's own
    /// vault-backed key (D1). Returns `None` for legacy non-`did:key`
    /// principals or when no vault key is loadable — those announces
    /// go out without a PoP and the hub treats them as unverified
    /// (hub policy, per the ADR).
    async fn principal_pop(&self, principal_did: &str) -> Option<String> {
        if !principal_did.starts_with("did:key:") {
            return None;
        }
        let key = self
            .host
            .principal_signing_keys()
            .signing_key_for_did(principal_did)
            .await?;
        let now = chrono::Utc::now().timestamp();
        let payload = serde_json::json!({
            "runtimeId": self.host.runtime_did(),
            "principalDid": principal_did,
            "iat": now,
            "exp": now + 300,
        });
        Some(super::tunnel_channel_signature::jws_sign(
            &key,
            payload.to_string().as_bytes(),
        ))
    }

    /// Send initial instance announcements for all local Principals.
    pub async fn announce_instances(&self, handle: &TunnelHandle) -> anyhow::Result<()> {
        let principal_manager = self.host.principal_manager();
        let principals = principal_manager.list_all().await;

        for principal in principals {
            if manager_hub_id(&self.host, &principal).await.is_none() {
                continue;
            }
            let name = principal.name().await;
            let did = principal.did().await;
            let exposure: InstanceExposure = principal.exposure().await.into();
            let instance_id = self.instance_id(&name);
            let payload = InstanceAnnouncePayload {
                registration_id: None,
                local_id: principal.id.0.clone(),
                identity_key: self
                    .host
                    .tunnel_channel_port()
                    .actor_info(&ActorRef::from(&principal.id))
                    .await
                    .identity_key,
                id: instance_id.clone(),
                instance_type: InstanceType::Principal,
                name: name.clone(),
                agent_did: None,
                principal_did: Some(did.0.clone()),
                principal_pop: self.principal_pop(&did.0).await,
                owner: self.hub_owner(&principal).await,
                bundle_ref: None,
                runtime_display_name: Some(self.runtime_display_name.clone()),
                status: InstanceStatus::Online,
                exposure: exposure.clone(),
                capabilities: None,
                metadata: None,
                avatar: principal.config.read().await.identity.avatar.clone(),
                // B5: `runtime_direct_endpoint` field dropped from payload.
            };

            // Seed local instance state cache with default Online status and the
            // Principal's configured exposure.
            let mut state = self.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure,
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
    /// The principal's owner as a Hub account, when it has one (ADR-081
    /// D4). Hub's wire form is the bare account UUID.
    async fn hub_owner(&self, principal: &crate::principal::Principal) -> Option<ActorRef> {
        let owner = principal.config.read().await.owner.clone();
        let info = self.host.tunnel_channel_port().actor_info(&owner).await;
        match (info.kind.as_str(), info.hub_id) {
            ("user", Some(account)) => Some(ActorRef::User(account.into())),
            _ => None,
        }
    }

    pub async fn announce_single_instance(&self, principal_name: &str) -> anyhow::Result<()> {
        self.announce_registration(principal_name, None).await
    }

    async fn announce_registration(
        &self,
        principal_name: &str,
        registration_id: Option<String>,
    ) -> anyhow::Result<()> {
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

        let principal_manager = self.host.principal_manager();
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

        if registration_id.is_none() && manager_hub_id(&self.host, &principal).await.is_none() {
            return Ok(());
        }
        let name = principal.name().await;
        let did = principal.did().await;
        let exposure: InstanceExposure = principal.exposure().await.into();
        let instance_id = self.instance_id(&name);
        let payload = InstanceAnnouncePayload {
            registration_id,
            local_id: principal.id.0.clone(),
            identity_key: self
                .host
                .tunnel_channel_port()
                .actor_info(&ActorRef::from(&principal.id))
                .await
                .identity_key,
            id: instance_id.clone(),
            instance_type: InstanceType::Principal,
            name: name.clone(),
            agent_did: None,
            principal_did: Some(did.0.clone()),
            principal_pop: self.principal_pop(&did.0).await,
            owner: self.hub_owner(&principal).await,
            bundle_ref: None,
            runtime_display_name: Some(self.runtime_display_name.clone()),
            status: InstanceStatus::Online,
            exposure: exposure.clone(),
            capabilities: None,
            metadata: None,
            avatar: principal.config.read().await.identity.avatar.clone(),
            // B5: `runtime_direct_endpoint` field dropped from payload.
        };

        // Seed local instance state cache
        let mut state = self.state.write().await;
        state.instance_state.insert(
            instance_id,
            InstanceState {
                exposure,
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

    /// Start periodic instance heartbeat task. The Hub marks an instance
    /// offline when it has not heard a heartbeat for 90 s, so the period
    /// is the Hub's `heartbeat_interval_secs` capped at 60 s (30 s when
    /// the Hub sends none).
    pub fn spawn_heartbeat_task(
        &self,
        handle: TunnelHandle,
        interval_secs: u32,
    ) -> tokio::task::JoinHandle<()> {
        let dispatcher = self.clone();
        let period = match interval_secs {
            0 => 30,
            secs => secs.clamp(5, 60),
        };
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(u64::from(period)));
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
        let principal_manager = self.host.principal_manager();
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
            TunnelMessage::PrincipalRegistrationRejected {
                registration_id,
                message,
            } => {
                self.state
                    .write()
                    .await
                    .registration_receipts
                    .insert(registration_id, Err(message));
            }
            TunnelMessage::PrincipalUnlinked { local_id, hub_id } => {
                self.unlink_principal(&local_id, &hub_id).await?;
            }
            TunnelMessage::PrincipalOwnerChanged {
                request_id,
                local_id,
                hub_id,
                owner_id,
            } => {
                let synced = self
                    .sync_owner_from_hub(&local_id, &hub_id, &owner_id)
                    .await;
                match request_id {
                    Some(id) => {
                        self.state
                            .write()
                            .await
                            .transfer_receipts
                            .insert(id, synced.as_ref().map(|_| ()).map_err(ToString::to_string));
                    }
                    // Hub refused an announce naming another owner, or the
                    // owner transferred it on Hub: come back online under
                    // the recorded owner.
                    None => {
                        if let Ok(Some(name)) = &synced {
                            if let Err(e) = self.announce_single_instance(name).await {
                                warn!(principal = %name, "re-announce after owner sync failed: {e}");
                            }
                        }
                    }
                }
                synced?;
            }
            TunnelMessage::PrincipalTransferRejected {
                request_id,
                message,
            } => {
                self.state
                    .write()
                    .await
                    .transfer_receipts
                    .insert(request_id, Err(message));
            }
            TunnelMessage::PrincipalRegistered {
                registration_id,
                local_id,
                hub_id,
                signing_did,
            } => {
                let manager = self.host.principal_manager();
                let principal = manager
                    .get(peko_subject::PrincipalId(local_id))
                    .await
                    .ok_or_else(|| {
                        anyhow::anyhow!("registration receipt names an unknown principal")
                    })?;
                anyhow::ensure!(
                    principal.did().await.0 == signing_did,
                    "registration receipt signing identity mismatch"
                );
                let port: Arc<dyn ChannelPort> = self.host.tunnel_channel_port();
                let existing = port.actor_info(&ActorRef::from(&principal.id)).await;
                // Keep receipt validation and durable identity commit in one nonce lifecycle.
                let mut state = self.state.write().await;
                let pending = state.pending_registrations.get(&principal.id.0);
                let acknowledged = registration_id
                    .as_ref()
                    .is_some_and(|id| Some(id) == pending);
                anyhow::ensure!(
                    acknowledged || existing.hub_id.as_deref() == Some(&hub_id),
                    "Unsolicited registration receipt"
                );
                let config = principal.config.read().await;
                port.register_actor(
                    &ActorRef::from(&principal.id),
                    Some(&hub_id),
                    config.did.as_ref().map(|d| d.0.as_str()),
                    config.actor_key.as_deref(),
                    Some(&config.name),
                    if config.exposure == peko_auth::Exposure::Public {
                        peko_channel::Registration::Public
                    } else {
                        peko_channel::Registration::Private
                    },
                )
                .await?;
                drop(config);
                if acknowledged {
                    state
                        .registration_receipts
                        .insert(registration_id.expect("matching nonce"), Ok(hub_id));
                }
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
                // Resend channel envelopes the recipient has not confirmed.
                let port = self.host.tunnel_channel_port();
                tokio::spawn(async move { port.on_tunnel_ready().await });
                // Announce all instances after auth
                if let Err(e) = self.announce_instances(&handle).await {
                    warn!("Failed to announce instances: {}", e);
                }
                // Keep this connection's instances online at the Hub: it
                // marks an instance offline after 90 s without a heartbeat,
                // and public chat to an offline peko answers 503. The task
                // was never started, so every peko went offline ~90 s after
                // it announced. Started after the announce, so the first
                // (immediate) beat finds the instances registered.
                let task = self.spawn_heartbeat_task(handle.clone(), heartbeat_interval_secs);
                if let Some(previous) = self
                    .state
                    .write()
                    .await
                    .heartbeat_task
                    .replace(task.abort_handle())
                {
                    previous.abort();
                }
            }
            TunnelMessage::Disconnect { reason } => {
                info!("Tunnel disconnect: {}", reason);
                self.mark_disconnected().await;
            }
            // peko-channel cross-runtime PR-A commit 3: inbound
            // `TunnelChannelEvent` from a peer runtime (proxied by
            // pekohub). Verify the source runtime's signature against
            // the `source_runtime_id` they claim, emit the audit line,
            // and append the event to the local `events.jsonl` mirror
            // (the append lands in PR-B alongside
            // `AppState.channel_port`).
            TunnelMessage::TunnelChannelEvent {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                event,
                signature,
                author_signature,
            } => {
                self.handle_inbound_tunnel_channel_event(
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    channel_id,
                    event,
                    signature,
                    author_signature,
                )
                .await?;
            }
            // peko-channel cross-runtime PR-3a commit 3: inbound
            // `TunnelChannelInvite` from a peer runtime (the creator
            // of a cross-runtime channel). Verify the source
            // runtime's signature against the `source_runtime_id` they
            // claim, then bootstrap the local mirror via the host's
            // `dm_channel_mirror_bootstrap` (Phase 12a — resolves the
            // invited local principal, derives the receiver-local DM
            // binding, calls `join_remote`, and ensures a live
            // subscriber; the synthetic `ChannelEvent::Created` still
            // seeds `events.jsonl` so PR-2b's `peko-stream` listener
            // fires on the desktop).
            TunnelMessage::TunnelChannelInvite {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                creator,
                creator_did,
                name,
                kind,
                initial_members,
                signature,
                author_signature,
            } => {
                self.handle_inbound_tunnel_channel_invite(
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    channel_id,
                    creator,
                    creator_did,
                    name,
                    kind,
                    initial_members,
                    signature,
                    author_signature,
                )
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
                    .send_error_response(
                        &handle,
                        &request_id,
                        400,
                        "Invalid request payload",
                        false,
                    )
                    .await;
            }
        };

        // ADR-057 transport audit: resolve the caller FIRST, then run
        // the exposure ACL against the *resolved* identity. ADR-058 D5:
        // the resolved identity is a typed `ActorRef` mapped from the
        // bridge token's `(kind, sub)` claims — `user:<hub account>`
        // or `visitor:<hub-minted id>`, never a principal subject and
        // never the reserved `local` id.
        let caller_principal = match resolve_bridge_caller(
            &bridge_payload,
            self.host.jwt_validator().as_ref(),
        )
        .await
        {
            Ok(caller) => caller,
            Err(e) => {
                warn!(
                    "Tunnel caller resolution failed for {}: {}",
                    principal_name, e
                );
                return self
                    .send_error_response(
                        &handle,
                        &request_id,
                        403,
                        &format!("Forbidden: {}", e),
                        false,
                    )
                    .await;
            }
        };

        // The audit emit's `agent_did` positional argument is the
        // AuditEvent struct's legacy `agent_did: Option<String>`
        // field (column name kept for the Drizzle migration tracking
        // issue). The principal name is the runtime's view of the
        // same identifier — pass it through; downstream audit
        // consumers can re-key on the `caller` ActorRef kind.
        self.host
            .observability()
            .audit_with_caller(
                Some(&caller_principal),
                "tunnel_proxied_request",
                Some(&principal_name),
                serde_json::json!({
                    "request_id": &request_id,
                    "caller": caller_principal.to_string(),
                }),
            )
            .await
            .ok();

        // Defense-in-depth: enforce the local exposure ACL against the
        // resolved caller identity (see the reorder note above).
        if let Err(e) = self
            .check_request_allowed(&principal_name, &bridge_payload, &caller_principal)
            .await
        {
            warn!("Tunnel ACL denied request for {}: {}", principal_name, e);
            return self
                .send_error_response(
                    &handle,
                    &request_id,
                    403,
                    &format!("Forbidden: {}", e),
                    false,
                )
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
                .send_error_response(&handle, &request_id, 400, "Empty message", false)
                .await;
        }

        let principal_manager = self.host.principal_manager();
        let principal = match principal_manager.get_by_name(&principal_name).await {
            Some(p) => p,
            None => {
                warn!("Proxied request for unknown principal: {}", principal_name);
                return self
                    .send_error_response(&handle, &request_id, 404, "Principal not found", false)
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
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<peko_engine::AgenticEvent>(256);
        let on_event: Box<dyn Fn(peko_engine::AgenticEvent) + Send + Sync> =
            Box::new(move |event| {
                let _ = event_tx.try_send(event);
            });

        // Run the principal in a background task. When it finishes, the
        // `event_tx` clone it holds is dropped, closing the channel and
        // signalling the drain loop below to stop.
        let pm = principal_manager.clone();
        let principal_id = principal.id.clone();
        let recv_handle = tokio::spawn(async move {
            pm.receive_streaming(
                principal_id,
                caller_principal,
                message,
                channel,
                on_event,
                None,
            )
            .await
        });

        // Forward each token delta as a raw-text `StreamChunk`.
        // Forward each per-iteration boundary as a typed
        // `StreamIteration` so pekohub can project it into an SSE
        // `event: iteration` for web-chat iteration bubbles.
        let mut seq: u32 = 0;
        let mut streamed_any = false;
        while let Some(event) = event_rx.recv().await {
            // Iteration boundary is a content-free marker — emit it
            // BEFORE the chunk but don't break the chunk loop; the
            // hub re-projects to an SSE event line.
            if let peko_engine::AgenticEvent::IterationBoundary { iteration, .. } = event {
                if let Err(e) = handle.send_stream_iteration(request_id.clone(), iteration) {
                    warn!(
                        "Failed to send stream iteration marker; aborting stream: {}",
                        e
                    );
                    recv_handle.abort();
                    let _ = handle.send_stream_end(request_id);
                    return Ok(());
                }
                continue;
            }
            let delta = match event {
                peko_engine::AgenticEvent::AssistantDelta { text, .. } => text,
                peko_engine::AgenticEvent::AssistantText { text, .. } => text,
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
                    .send_error_response(
                        &handle,
                        &request_id,
                        500,
                        "Execution failed",
                        streamed_any,
                    )
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
                let (status, message) = execution_failure(&principal_name, &e);
                return self
                    .send_error_response(&handle, &request_id, status, &message, streamed_any)
                    .await;
            }
        }

        Ok(())
    }

    /// Refuse or fail a proxied request. Before any reply text has
    /// streamed, the refusal is a `proxied_response` with an HTTP
    /// `status` and the message as its body: PekoHub ends the chat stream
    /// with an error frame, so the client shows an error, not a reply
    /// from the peko. (A hub that predates that still writes the body
    /// as text and ends the stream, as it did before.) Once text has
    /// streamed, the message follows as text and the stream ends — a
    /// status response mid-stream would leave an older hub's stream open.
    async fn send_error_response(
        &self,
        handle: &TunnelHandle,
        request_id: &str,
        status: u16,
        message: &str,
        streamed: bool,
    ) -> anyhow::Result<()> {
        if !streamed {
            let payload = serde_json::json!({ "status": status, "body": message });
            if let Err(e) =
                handle.send_response(request_id.to_string(), payload.to_string().into_bytes())
            {
                warn!("Failed to send error response: {}", e);
            }
            return Ok(());
        }
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

    /// ADR-081 D7: Hub moved the principal's online link to another
    /// runtime. This copy keeps its local identity and history but goes
    /// offline-only: no Hub route, local-only exposure. A notice that does
    /// not match the principal's current Hub link is ignored.
    async fn unlink_principal(&self, local_id: &str, hub_id: &str) -> anyhow::Result<()> {
        let manager = self.host.principal_manager();
        let Some(principal) = manager
            .get(peko_subject::PrincipalId(local_id.into()))
            .await
        else {
            return Ok(());
        };
        let port: Arc<dyn ChannelPort> = self.host.tunnel_channel_port();
        let me = port.actor_info(&ActorRef::from(&principal.id)).await;
        if me.hub_id.as_deref() != Some(hub_id) {
            warn!(
                local_id,
                hub_id, "Ignoring an unlink notice that does not match this principal's Hub link"
            );
            return Ok(());
        }
        let config = principal.config.read().await;
        let name = config.name.clone();
        port.register_actor(
            &ActorRef::from(&principal.id),
            None,
            config.did.as_ref().map(|did| did.0.as_str()),
            config.actor_key.as_deref(),
            Some(&name),
            peko_channel::Registration::LocalOnly,
        )
        .await?;
        drop(config);
        manager
            .update_config(&name, |c| {
                c.exposure = peko_auth::Exposure::LocalOnly;
            })
            .await?;
        self.state
            .write()
            .await
            .instance_state
            .remove(&self.instance_id(&name));
        let _ = self
            .host
            .observability()
            .audit_security(
                "principal.online_link_moved",
                Some(local_id),
                serde_json::json!({ "hub_id": hub_id }),
            )
            .await;
        warn!(principal = %name, hub_id, "Principal is now online on another runtime; this copy is offline-only");
        Ok(())
    }

    /// Make `local_id`'s owner the one Hub records (ADR-081 D4): Hub is
    /// the authority on an online principal's owner, and the local config
    /// is its copy. A notice that does not match the principal's current
    /// Hub link is ignored. Returns the principal's name when it applied.
    async fn sync_owner_from_hub(
        &self,
        local_id: &str,
        hub_id: &str,
        owner_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let manager = self.host.principal_manager();
        let Some(principal) = manager
            .get(peko_subject::PrincipalId(local_id.into()))
            .await
        else {
            return Ok(None);
        };
        if manager_hub_id(&self.host, &principal).await.as_deref() != Some(hub_id) {
            warn!(
                local_id,
                hub_id, "Ignoring an owner notice that does not match this principal's Hub link"
            );
            return Ok(None);
        }
        anyhow::Context::context(uuid::Uuid::parse_str(owner_id), "Hub owner IDs are UUIDs")?;
        let owner = ActorRef::User(peko_subject::Route::hub(owner_id));
        let port: Arc<dyn ChannelPort> = self.host.tunnel_channel_port();
        let (name, previous) = {
            let config = principal.config.read().await;
            (config.name.clone(), config.owner.clone())
        };
        if port.key_of(&previous) != port.key_of(&owner) {
            manager
                .update_config(&name, |c| c.owner = owner.clone())
                .await?;
            let _ = self
                .host
                .observability()
                .audit_security(
                    "principal.owner_synced",
                    Some(local_id),
                    serde_json::json!({
                        "hub_id": hub_id,
                        "from": previous.canonical_address(),
                        "to": owner.canonical_address(),
                    }),
                )
                .await;
            warn!(principal = %name, owner = %owner, "Principal owner set to the owner PekoHub records");
        }
        Ok(Some(name))
    }

    /// Transfer an online principal through Hub (ADR-081 D4): Hub allows it
    /// when this runtime's account is the recorded owner, then confirms
    /// with the new owner, which [`Self::sync_owner_from_hub`] writes.
    pub async fn transfer_online(&self, name: &str, owner_account: &str) -> anyhow::Result<()> {
        let manager = self.host.principal_manager();
        let principal = manager
            .get_by_name(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Principal '{name}' not found"))?;
        let hub_id = manager_hub_id(&self.host, &principal)
            .await
            .ok_or_else(|| anyhow::anyhow!("the principal is not registered with PekoHub"))?;
        let handle = {
            let state = self.state.read().await;
            anyhow::ensure!(
                state.ready,
                "Transferring an online principal needs a connected PekoHub tunnel"
            );
            state.tunnel_handle.clone().ok_or_else(|| {
                anyhow::anyhow!("Transferring an online principal needs a connected PekoHub tunnel")
            })?
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        handle.send(TunnelMessage::PrincipalTransfer {
            request_id: request_id.clone(),
            local_id: principal.id.0.clone(),
            hub_id,
            owner_id: owner_account.to_string(),
        })?;
        let answer = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let Some(answer) = self
                    .state
                    .write()
                    .await
                    .transfer_receipts
                    .remove(&request_id)
                {
                    break answer;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("PekoHub did not answer the transfer request"))?;
        answer.map_err(|message| anyhow::anyhow!("PekoHub refused the transfer: {message}"))
    }

    pub async fn register_principal(
        &self,
        name: &str,
        exposure: InstanceExposure,
    ) -> anyhow::Result<Option<String>> {
        let _registration = self.registration_lock.lock().await;
        let manager = self.host.principal_manager();
        let principal = manager
            .get_by_name(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("principal not loaded"))?;
        if exposure == InstanceExposure::LocalOnly {
            if let Some(handle) = self.state.read().await.tunnel_handle.clone() {
                handle.send(TunnelMessage::InstanceDeregister {
                    payload: super::protocol::InstanceDeregisterPayload {
                        id: self.instance_id(name),
                    },
                })?;
            }
            let port: Arc<dyn ChannelPort> = self.host.tunnel_channel_port();
            let config = principal.config.read().await;
            port.register_actor(
                &ActorRef::from(&principal.id),
                None,
                config.did.as_ref().map(|did| did.0.as_str()),
                config.actor_key.as_deref(),
                Some(name),
                peko_channel::Registration::LocalOnly,
            )
            .await?;
            drop(config);
            manager
                .update_config(name, |c| {
                    c.exposure = peko_auth::Exposure::LocalOnly;
                })
                .await?;
        } else {
            anyhow::ensure!(
                self.state.read().await.ready,
                "Hub registration requires a connected, authenticated tunnel"
            );
            // ADR-081 D4: only a principal with a Hub owner goes online.
            anyhow::ensure!(
                self.hub_owner(&principal).await.is_some(),
                "An online principal needs a Hub owner: register this runtime \
                 (peko runtime register) so its local user has a Hub account, \
                 or transfer the principal to a Hub user"
            );
            let old = principal.config.read().await.exposure;
            let id = uuid::Uuid::new_v4().to_string();
            self.state
                .write()
                .await
                .pending_registrations
                .insert(principal.id.0.clone(), id.clone());
            let announce = async {
                manager
                    .prepare_hub_registration(name, exposure.into())
                    .await?;
                self.announce_registration(name, Some(id.clone())).await
            }
            .await;
            if let Err(error) = announce {
                self.state
                    .write()
                    .await
                    .pending_registrations
                    .remove(&principal.id.0);
                manager.update_config(name, |c| c.exposure = old).await?;
                anyhow::bail!("Hub registration failed: {error}");
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                loop {
                    if let Some(result) = self.state.write().await.registration_receipts.remove(&id)
                    {
                        break result;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await;
            let mut state = self.state.write().await;
            state.pending_registrations.remove(&principal.id.0);
            // A receipt already committing when the wait elapsed still has a definitive result.
            let result = match result {
                Err(elapsed) => state
                    .registration_receipts
                    .remove(&id)
                    .map(Ok)
                    .unwrap_or(Err(elapsed)),
                completed => completed,
            };
            drop(state);
            match result {
                Ok(Ok(_)) => {}
                other => {
                    manager.update_config(name, |c| c.exposure = old).await?;
                    anyhow::bail!("Hub registration failed: {:?}", other);
                }
            }
        }
        let port: std::sync::Arc<dyn peko_channel::ChannelPort> = self.host.tunnel_channel_port();
        manager.register_actor_identity(&principal, &port).await?;
        Ok(manager_hub_id(&self.host, &principal).await)
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

    /// Build and send an `ExposureUpdate` for the given Principal. Chat
    /// access stays the runtime's decision (ADR-081 D8); Hub learns only
    /// the exposure.
    async fn send_exposure_update(
        &self,
        principal_name: &str,
        exposure: InstanceExposure,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(principal_name);
        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };

        if let Some(handle) = handle {
            let payload = ExposureUpdatePayload {
                instance_id: instance_id.clone(),
                exposure: exposure.clone(),
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
        } else {
            state.instance_state.insert(
                payload.instance_id.clone(),
                InstanceState {
                    exposure: payload.exposure.clone(),
                    status: InstanceStatus::Online,
                },
            );
        }
        drop(state);

        // Re-announce the Principal instance to confirm the change.
        let principal_manager = self.host.principal_manager();
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
                    let principal_pop = self.principal_pop(&did.0).await;
                    let announce_payload = InstanceAnnouncePayload {
                        registration_id: None,
                        local_id: principal.id.0.clone(),
                        identity_key: self
                            .host
                            .tunnel_channel_port()
                            .actor_info(&ActorRef::from(&principal.id))
                            .await
                            .identity_key,
                        id: instance_id,
                        instance_type: InstanceType::Principal,
                        name: name.clone(),
                        agent_did: None,
                        principal_did: Some(did.0),
                        principal_pop,
                        owner: self.hub_owner(&principal).await,
                        bundle_ref: None,
                        runtime_display_name: Some(self.runtime_display_name.clone()),
                        status,
                        exposure: payload.exposure.clone(),
                        capabilities: None,
                        metadata: None,
                        avatar: principal.config.read().await.identity.avatar.clone(),
                        // B5: `runtime_direct_endpoint` field dropped from payload.
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

    /// Handle an inbound `TunnelChannelEvent` — a push-only channel
    /// fan-out from a peer runtime. peko-channel cross-runtime PR-A
    /// commit 3; ADR-058 D2/D3 v2 envelope.
    ///
    /// Pipeline:
    ///
    /// 1. Derive the verifying key from `source_runtime_id` (the
    ///    self-certifying `did:key` form). Invalid DIDs are dropped
    ///    with a warn — the hub's source-allowlist is the primary
    ///    gate, this is the receiver-side check.
    /// 2. Verify the runtime counter-signature — a compact JWS over
    ///    the v2 payload (`verify_channel_event` checks the version
    ///    tag and the `iat`/`exp` freshness window). The payload's
    ///    fields must equal the envelope's, and `event_b64u` must
    ///    decode to the re-serialized `event` bytes — any mismatch
    ///    means a mangled/malicious envelope even if the signature
    ///    holds.
    /// 3. ADR-058 D2 author gate: when `source_principal_did` is a
    ///    `did:key`, the envelope MUST carry a non-empty
    ///    `author_signature` that verifies against the key embedded
    ///    in that DID over the SAME payload segment, and the author
    ///    must be a registered remote member of the channel mirror
    ///    (`is_remote_member`). Non-`did:key` authors (e.g.
    ///    `user:<id>`, legacy bare ids) are accepted as
    ///    runtime-vouched with a debug log.
    /// 4. Emit the `received_inbound` audit line via
    ///    [`tunnel_channel_audit::emit_received_inbound`].
    /// 5. Append the event to the local `events.jsonl` mirror via
    ///    [`crate::tunnel::TunnelChannelPort::append_remote_event`].
    ///    The local mirror write trusts the upstream verification
    ///    (steps 1-4), so it skips the membership / parent / sender
    ///    checks that the local `post` path enforces.
    ///
    /// No response is sent back to the source runtime — channel
    /// events are push-only.
    #[allow(clippy::too_many_arguments)]
    async fn handle_inbound_tunnel_channel_event(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        source_principal_did: String,
        channel_id: String,
        event: peko_protocol::channel::ChannelEvent,
        signature: String,
        author_signature: String,
    ) -> anyhow::Result<()> {
        // 0. ADR-057 transport audit: reject envelopes not addressed to
        // us. `recipient_runtime_id` is signature-covered, so a
        // mis-routing (or compromised) hub cannot silently redirect
        // another runtime's envelope here — but nothing verified the
        // binding receiver-side before, and a valid-for-runtime-B
        // envelope must not append into this runtime's mirrors.
        let own_did = self.host.runtime_did();
        if recipient_runtime_id != own_did {
            warn!(
                "inbound TunnelChannelEvent: dropping envelope addressed to {recipient_runtime_id} (we are {own_did}, request_id={request_id})"
            );
            return Ok(());
        }

        // Replays: the signed `iat`/`exp` (ADR-058 D3) bound an
        // envelope's life, and the persistent applied-envelope log
        // (step 5) applies each `request_id` once while still
        // confirming a resend — the sender's outbox resends under the
        // same id until it gets a receipt.

        // 1. Derive the verifying key.
        let verifying_key = match did_key_to_verifying_key(&source_runtime_id) {
            Ok(k) => k,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelEvent: invalid source_runtime_id {source_runtime_id}: {e}"
                );
                return Ok(()); // drop, do not propagate — channel events have no response.
            }
        };

        // 2. Verify the runtime counter-signature (ADR-058 D3 compact
        // JWS; version tag + freshness window checked inside).
        let (payload, payload_segment) = match verify_channel_event(&verifying_key, &signature) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelEvent: signature verification failed for source_runtime_id={source_runtime_id}: {e}"
                );
                return Ok(());
            }
        };

        // 2b. The signed payload must agree with the envelope it
        // rides — a mismatch means a mangled/malicious envelope even
        // though the signature itself holds.
        if payload.request_id != request_id
            || payload.source_runtime_id != source_runtime_id
            || payload.recipient_runtime_id != recipient_runtime_id
            || payload.source_principal_did != source_principal_did
            || payload.channel_id != channel_id
        {
            warn!(
                "inbound TunnelChannelEvent: dropping envelope whose fields disagree with the signed payload (request_id={request_id})"
            );
            return Ok(());
        }
        // Re-serialize the event for the payload-bytes comparison and
        // the audit preview. We must use the same byte sequence the
        // source runtime signed; serde_json's stable
        // declaration-order emission is what guarantees this.
        let event_bytes = match serde_json::to_vec(&event) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelEvent: failed to re-serialize event for verification (channel_id={channel_id}): {e}"
                );
                return Ok(());
            }
        };
        let payload_event_bytes = match URL_SAFE_NO_PAD.decode(&payload.event_b64u) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelEvent: payload event_b64u is not valid base64url (request_id={request_id}): {e}"
                );
                return Ok(());
            }
        };
        if payload_event_bytes != event_bytes {
            warn!(
                "inbound TunnelChannelEvent: dropping envelope whose event bytes disagree with the signed payload (request_id={request_id})"
            );
            return Ok(());
        }

        // 2c. A delivery receipt: the (signature-verified) source runtime
        // applied envelopes we sent it. It confirms only envelopes
        // addressed to that runtime, and is never appended to a log.
        if let peko_protocol::channel::ChannelEvent::Received { request_ids, .. } = &event {
            self.host
                .tunnel_channel_port()
                .confirm_delivery(&source_runtime_id, request_ids)
                .await;
            return Ok(());
        }

        // A previously applied, runtime-signed delivery needs only its receipt.
        // This also confirms a resent leave after its author has left membership.
        let port = self.host.tunnel_channel_port();
        if port.already_applied(&request_id).await {
            port.send_receipt(
                &source_runtime_id,
                &peko_protocol::channel::ChannelId(channel_id.clone()),
                vec![request_id.clone()],
            )
            .await;
            return Ok(());
        }

        // 3. ADR-058 D2 author gate. A `did:key` author must prove
        // possession of the DID's key AND be a registered remote
        // member of the channel mirror; anything else is accepted as
        // runtime-vouched (legacy path).
        let channel_id_typed = peko_protocol::channel::ChannelId(channel_id.clone());
        if payload.source_principal_did.starts_with("did:key:") {
            if author_signature.is_empty() {
                warn!(
                    "inbound TunnelChannelEvent: dropping did:key-authored envelope with no author signature (request_id={request_id}, author={})",
                    payload.source_principal_did
                );
                return Ok(());
            }
            if let Err(e) = verify_author_signature(
                &author_signature,
                &payload_segment,
                &payload.source_principal_did,
            ) {
                warn!(
                    "inbound TunnelChannelEvent: author signature verification failed (request_id={request_id}, author={}): {e}",
                    payload.source_principal_did
                );
                return Ok(());
            }
            match self
                .host
                .tunnel_channel_port()
                .is_remote_member(
                    &channel_id_typed,
                    &source_runtime_id,
                    &payload.source_principal_did,
                )
                .await
            {
                Ok(true) => {}
                // ADR-081 D7: a member whose online link moved here keeps
                // its membership once the directory confirms the link.
                Ok(false)
                    if self
                        .host
                        .tunnel_channel_port()
                        .follow_link(
                            &channel_id_typed,
                            &source_runtime_id,
                            &payload.source_principal_did,
                        )
                        .await => {}
                Ok(false) => {
                    warn!(
                        "inbound TunnelChannelEvent: dropping envelope from did:key author who is not a remote member of the mirror (request_id={request_id}, channel_id={channel_id}, author={})",
                        payload.source_principal_did
                    );
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        "inbound TunnelChannelEvent: remote-membership check failed, dropping envelope (request_id={request_id}, channel_id={channel_id}): {e}"
                    );
                    return Ok(());
                }
            }
        } else {
            let human = payload.source_principal_did.parse::<ActorRef>();
            let Ok(human) = human else {
                return Ok(());
            };
            if !matches!(human, ActorRef::User(_) | ActorRef::Visitor(_)) || !human.is_hub_route() {
                warn!(
                    request_id,
                    "Channel author must be a registered principal or a Hub human actor"
                );
                return Ok(());
            }
            if !self
                .host
                .tunnel_channel_port()
                .is_remote_member(
                    &channel_id_typed,
                    &source_runtime_id,
                    &payload.source_principal_did,
                )
                .await?
            {
                return Ok(());
            }
        }
        let claimed_actor = match &event {
            peko_protocol::channel::ChannelEvent::Posted { author, .. } => Some(author),
            peko_protocol::channel::ChannelEvent::MemberLeft { member, .. } => Some(member),
            _ => None,
        };
        if let Some(author) = claimed_actor {
            let source_subject = payload
                .source_principal_did
                .parse::<ActorRef>()
                .unwrap_or_else(|_| {
                    ActorRef::Principal(payload.source_principal_did.clone().into())
                });
            let port = self.host.tunnel_channel_port();
            let source_actor = port.actor_info(&source_subject).await;
            let author_actor = match port.resolve_actor(author).await {
                Ok(actor) => actor,
                Err(_) => return Ok(()),
            };
            if source_actor.hub_id.is_none()
                || author_actor.identity_key != source_actor.identity_key
                || author_actor.kind != source_actor.kind
            {
                warn!(
                    request_id,
                    "Posted author does not match its authenticated actor"
                );
                return Ok(());
            }
        }

        // 4. Audit. Always emit a row, regardless of whether the
        // local-mirror append below succeeds; the row documents the
        // verify-and-(append-or-drop) decision.
        let event_kind = event.kind();
        let preview_json = String::from_utf8_lossy(&event_bytes).into_owned();
        tunnel_channel_audit::emit_received_inbound(
            &request_id,
            &source_runtime_id,
            &source_principal_did,
            &channel_id,
            event_kind,
            &super::tunnel_channel_audit::preview_event_payload(&preview_json),
        );

        // 4b. ADR-057 transport audit: the inner event's channel must
        // match the envelope's signed `channel_id` — the payload
        // covers the envelope field, so a mismatched inner value is a
        // mangled/malicious envelope even though the signature holds.
        let inner_channel = event.channel();
        if inner_channel.0 != channel_id {
            warn!(
                "inbound TunnelChannelEvent: dropping envelope whose inner event channel does not match the signed channel_id (request_id={request_id})"
            );
            return Ok(());
        }

        // 5. Local-mirror append. The dispatcher is the single source
        // of truth for envelope validity (steps 1-4); the
        // `append_remote_event` impl deliberately does not re-verify
        // or re-check membership, since layering a second check here
        // would either duplicate work or risk drift.
        // A resend of an envelope already applied: confirm it again (the
        // first receipt may have been lost), apply nothing.
        let port = self.host.tunnel_channel_port();
        if port.already_applied(&request_id).await {
            port.send_receipt(
                &source_runtime_id,
                &channel_id_typed,
                vec![request_id.clone()],
            )
            .await;
            return Ok(());
        }
        // ADR-081 D7: only the linked copy receives. Unanswered, the sender
        // re-resolves the link and redelivers to the online copy.
        if !port.has_online_local_member(&channel_id_typed).await {
            warn!(
                request_id,
                channel_id, "inbound TunnelChannelEvent: no local member of this channel is online here; leaving it unacknowledged"
            );
            return Ok(());
        }
        if let Err(e) = port.append_remote_event(&channel_id_typed, &event).await {
            warn!(
                "inbound TunnelChannelEvent: local-mirror append failed (request_id={request_id}, channel_id={channel_id}, kind={event_kind}): {e}"
            );
            // Drop the event without a receipt — the source resends it
            // (e.g. once the invite that creates the mirror lands) —
            // and do not error: a single bad append must not
            // backpressure the dispatcher. The audit row above
            // preserves the trail.
            return Ok(());
        }
        port.mark_applied(&request_id).await;
        port.send_receipt(
            &source_runtime_id,
            &channel_id_typed,
            vec![request_id.clone()],
        )
        .await;

        debug!(
            "inbound TunnelChannelEvent: verified + audited + appended to local mirror \
             (request_id={request_id}, channel_id={channel_id}, kind={event_kind})"
        );

        Ok(())
    }

    /// Handle an inbound `TunnelChannelInvite` — a one-shot bootstrap
    /// envelope from a peer runtime that created a cross-runtime
    /// channel. peko-channel cross-runtime PR-3a commit 3; ADR-058
    /// D2/D3 v2 envelope.
    ///
    /// Pipeline (mirrors `handle_inbound_tunnel_channel_event`):
    ///
    /// 1. Derive the verifying key from `source_runtime_id` (the
    ///    self-certifying `did:key` form). Invalid DIDs are dropped
    ///    with a warn.
    /// 2. Verify the runtime counter-signature (compact JWS over the
    ///    v2 invite payload — version tag + freshness checked
    ///    inside). The payload's fields must equal the envelope's and
    ///    `initial_members_b64u` must decode to the re-serialized
    ///    `initial_members` bytes.
    /// 3. ADR-058 D2 author gate: when `creator_did` is a `did:key`,
    ///    the envelope MUST carry a non-empty `author_signature` that
    ///    verifies against the creator DID's key over the same
    ///    payload segment, AND `creator_did` must equal
    ///    `source_principal_did` (the inviter speaks for itself).
    ///    Non-`did:key` creator DIDs are accepted as runtime-vouched
    ///    with a debug log.
    /// 4. Emit the `received_inbound` audit line via
    ///    [`tunnel_channel_audit::emit_received_inbound`] (kind =
    ///    `"channel_invite"`).
    /// 5. Bootstrap the local mirror via the host's
    ///    [`crate::tunnel::TunnelHost::dm_channel_mirror_bootstrap`]
    ///    (Phase 12a): the host resolves the invited local principal
    ///    from the invitee row, derives the receiver-local DM binding
    ///    for DM invites, calls `join_remote` (idempotent), and
    ///    ensures a live subscriber.
    ///
    /// Like the channel-event handler, no response is sent back to
    /// the source runtime — invites are push-only.
    #[allow(clippy::too_many_arguments)]
    async fn handle_inbound_tunnel_channel_invite(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        source_principal_did: String,
        channel_id: String,
        creator: String,
        creator_did: String,
        name: String,
        kind: peko_protocol::channel::ConversationKind,
        initial_members: Vec<peko_protocol::channel::InitialMember>,
        signature: String,
        author_signature: String,
    ) -> anyhow::Result<()> {
        // 0. ADR-057 transport audit: recipient binding, mirroring the
        // channel-event handler (replays: see step 5).
        let own_did = self.host.runtime_did();
        if recipient_runtime_id != own_did {
            warn!(
                "inbound TunnelChannelInvite: dropping envelope addressed to {recipient_runtime_id} (we are {own_did}, request_id={request_id})"
            );
            return Ok(());
        }

        // 1. Derive the verifying key from the claimed
        // `source_runtime_id`. Same defense-in-depth layer as the
        // channel-event handler.
        let verifying_key = match did_key_to_verifying_key(&source_runtime_id) {
            Ok(k) => k,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelInvite: invalid source_runtime_id {source_runtime_id}: {e}"
                );
                return Ok(());
            }
        };

        // 2. Verify the runtime counter-signature (ADR-058 D3 compact
        // JWS; version tag + freshness window checked inside).
        let (payload, payload_segment) = match verify_channel_invite(&verifying_key, &signature) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelInvite: signature verification failed for \
                     source_runtime_id={source_runtime_id}: {e}"
                );
                return Ok(());
            }
        };

        // 2b. The signed payload must agree with the envelope it
        // rides.
        if payload.request_id != request_id
            || payload.source_runtime_id != source_runtime_id
            || payload.recipient_runtime_id != recipient_runtime_id
            || payload.source_principal_did != source_principal_did
            || payload.channel_id != channel_id
            || payload.creator != creator
            || payload.creator_did != creator_did
            || payload.name != name
            || payload.kind != kind.as_str()
        {
            warn!(
                "inbound TunnelChannelInvite: dropping envelope whose fields disagree with the signed payload (request_id={request_id})"
            );
            return Ok(());
        }
        // Re-serialize `initial_members` for the payload-bytes
        // comparison and the audit preview. `serde_json` emits vec
        // items in declaration order; round-trip is stable.
        let initial_members_bytes = match serde_json::to_vec(&initial_members) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelInvite: failed to re-serialize initial_members \
                     for verification (channel_id={channel_id}): {e}"
                );
                return Ok(());
            }
        };
        let payload_members_bytes = match URL_SAFE_NO_PAD.decode(&payload.initial_members_b64u) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "inbound TunnelChannelInvite: payload initial_members_b64u is not valid base64url (request_id={request_id}): {e}"
                );
                return Ok(());
            }
        };
        if payload_members_bytes != initial_members_bytes {
            warn!(
                "inbound TunnelChannelInvite: dropping envelope whose initial_members disagree with the signed payload (request_id={request_id})"
            );
            return Ok(());
        }

        // 3. ADR-058 D2 author gate: a `did:key` creator must prove
        // possession of its key and be the principal the envelope
        // names as source. Anything else is runtime-vouched (legacy).
        if payload.creator_did.starts_with("did:key:") {
            if author_signature.is_empty() {
                warn!(
                    "inbound TunnelChannelInvite: dropping did:key-authored invite with no author signature (request_id={request_id}, creator_did={})",
                    payload.creator_did
                );
                return Ok(());
            }
            if let Err(e) =
                verify_author_signature(&author_signature, &payload_segment, &payload.creator_did)
            {
                warn!(
                    "inbound TunnelChannelInvite: author signature verification failed (request_id={request_id}, creator_did={}): {e}",
                    payload.creator_did
                );
                return Ok(());
            }
            if payload.creator_did != payload.source_principal_did {
                warn!(
                    "inbound TunnelChannelInvite: dropping invite whose did:key creator_did ({}) does not match source_principal_did ({}) (request_id={request_id})",
                    payload.creator_did, payload.source_principal_did
                );
                return Ok(());
            }
        } else {
            warn!(request_id, "Principal invites require a signing key proof");
            return Ok(());
        }

        // 4. Audit. Always emit, regardless of whether the
        // local-mirror bootstrap below succeeds.
        let preview_json = String::from_utf8_lossy(&initial_members_bytes).into_owned();
        tunnel_channel_audit::emit_received_inbound(
            &request_id,
            &source_runtime_id,
            &source_principal_did,
            &channel_id,
            "channel_invite",
            &super::tunnel_channel_audit::preview_event_payload(&preview_json),
        );

        // An invite already applied (a resend after a lost receipt):
        // confirm it again, bootstrap nothing.
        let port = self.host.tunnel_channel_port();
        let channel_typed = peko_protocol::channel::ChannelId(channel_id.clone());
        if port.already_applied(&request_id).await {
            port.send_receipt(&source_runtime_id, &channel_typed, vec![request_id.clone()])
                .await;
            return Ok(());
        }

        // 5. Bootstrap the local mirror through the host (Phase
        // 12a): principal resolution + receiver-local DM binding +
        // `join_remote` (idempotent on the meta.json-existence
        // check, so a duplicate envelope is a no-op) + the live
        // subscriber kickoff. We log + drop on error rather than
        // propagating — invites are push-only and there's no caller
        // to error against.
        if let Err(e) = self
            .host
            .dm_channel_mirror_bootstrap(crate::tunnel::DmChannelInviteBootstrap {
                channel_id: channel_id.clone(),
                creator: creator.clone(),
                creator_did,
                name: name.clone(),
                initial_members: initial_members.clone(),
                source_runtime_id: source_runtime_id.clone(),
                kind,
            })
            .await
        {
            warn!(
                "inbound TunnelChannelInvite: local-mirror bootstrap failed \
                 (request_id={request_id}, channel_id={channel_id}, creator={creator}): {e}"
            );
            return Ok(());
        }
        port.mark_applied(&request_id).await;
        port.send_receipt(&source_runtime_id, &channel_typed, vec![request_id.clone()])
            .await;

        debug!(
            "inbound TunnelChannelInvite: verified + audited + bootstrapped local mirror \
             (request_id={request_id}, channel_id={channel_id}, creator={creator}, \
              initial_members={})",
            initial_members.len()
        );

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
        caller: &peko_auth::ActorRef,
    ) -> anyhow::Result<()> {
        let principal = self
            .host
            .principal_manager()
            .get_by_name(agent_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Principal not loaded"))?;
        let actor = self
            .host
            .tunnel_channel_port()
            .actor_info(&ActorRef::from(&principal.id))
            .await;
        anyhow::ensure!(
            actor.hub_id.is_some() && principal.exposure().await != peko_auth::Exposure::LocalOnly,
            "Principal is local-only; register it before Hub access"
        );
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

        // PR #11: an invite token bypasses the exposure-based ACL
        // when a token is present and valid. The token MUST be
        // signed by THIS runtime's signing key (so a token minted
        // by runtime A cannot be used to chat with a principal on
        // runtime B), and its `principal_name` MUST match the
        // requested agent — even if the signature is valid, a
        // token for the wrong principal is rejected.
        if let Some(token) = bridge_payload
            .get("headers")
            .and_then(|h| h.get("x-pekohub-invite-token"))
            .and_then(|v| v.as_str())
        {
            match super::invite_token::verify_token(
                &self.host.runtime_verifying_key(),
                token,
                chrono::Utc::now(),
            ) {
                Ok(claims) => {
                    if claims.principal_name != agent_name {
                        warn!(
                            agent_name,
                            token_principal = claims.principal_name.as_str(),
                            "Invite token rejected: principal name mismatch"
                        );
                        // Fall through to the exposure-based ACL
                        // instead of returning early — the token
                        // may be invalid for THIS principal but the
                        // exposure ACL may still allow it (e.g. the
                        // instance is Public anyway).
                    } else if self
                        .host
                        .invite_revocation_set()
                        .is_revoked(&claims.jti)
                        .await
                    {
                        warn!(
                            agent_name,
                            jti = %claims.jti,
                            "Invite token rejected: revoked by principal owner"
                        );
                    } else {
                        debug!(
                            agent_name,
                            jti = %claims.jti,
                            "Invite token accepted; bypassing exposure ACL"
                        );
                        return Ok(());
                    }
                }
                Err(e) => {
                    warn!(agent_name, error = %e, "Invite token rejected; falling through to exposure ACL");
                }
            }
        }

        match instance_state.exposure {
            // Public + Unlisted are both world-chat-table via the public
            // URL; PekoHub filters discovery strictly to `exposure =
            // public`. PR #11 introduces invite tokens for scoped
            // access — until then, Unlisted == Public at the runtime
            // ACL level.
            InstanceExposure::Public => Ok(()),
            InstanceExposure::LocalOnly => {
                anyhow::bail!("Agent is not exposed")
            }
            InstanceExposure::Private => {
                // The caller identity was resolved before this ACL ran
                // (see `handle_proxied_request`) — a typed `ActorRef`
                // mapped from the validated bridge token's `(kind,
                // sub)` claims (ADR-058 D5). Membership is a
                // kind-aware equality check: a visitor subject only
                // matches a visitor allow-list entry, a user only a
                // user entry — no cross-kind aliasing.
                if !caller.is_session_peer() || caller.subject_id().is_empty() {
                    warn!(
                        agent_name,
                        "Private instance request denied: no resolved caller identity"
                    );
                    anyhow::bail!("Authentication required")
                }

                // The runtime's own grants decide (ADR-081 D8): the owner
                // and every grant are matched through the actor registry.
                let config = principal.config.read().await;
                let port = self.host.tunnel_channel_port();
                if peko_auth::ownership::check_permission(
                    &peko_auth::ownership::principal_resource(&*config),
                    Permission::Chat,
                    caller,
                    &peko_channel::PortKeys(port.as_ref()),
                )
                .is_ok()
                {
                    Ok(())
                } else {
                    warn!(
                        agent_name,
                        caller = %caller,
                        "Private instance request denied: caller has no chat grant"
                    );
                    anyhow::bail!("Forbidden")
                }
            }
        }
    }
}

/// The status and message a failed principal turn goes back with. A
/// caller without the permission gets a 403 that says so in words a
/// chat user can read; the detailed reason, with actor ids, stays in
/// the runtime's log.
fn execution_failure(
    principal_name: &str,
    e: &crate::principal::manager::PrincipalManagerError,
) -> (u16, String) {
    use crate::principal::manager::PrincipalManagerError as E;
    use crate::principal::router::RouterError;
    match e {
        E::PermissionDenied(_) | E::RouterError(RouterError::PermissionDenied(_)) => (
            403,
            format!("{principal_name} doesn't accept chats from you."),
        ),
        other => (500, format!("Execution failed: {other}")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::{AppState, DaemonConfigSnapshot};

    use crate::principal::config::{
        PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalRoutingConfig,
    };
    use crate::tunnel::protocol::InstanceExposure;
    use peko_auth::{ActorRef, Permission, PermissionGrant};
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    async fn create_test_app_state() -> AppState {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let config = DaemonConfigSnapshot {
            data_dir: data_dir.clone(),
            config_dir: data_dir.clone(),
            log_level: "info".to_string(),
            launch_mode: crate::daemon::LaunchMode::Headless,
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

    async fn seed_registered(dispatcher: &TunnelDispatcher, name: &str) {
        let manager = dispatcher.host.principal_manager();
        let principal = manager
            .create(test_principal_config(
                name,
                ActorRef::User("local".into()),
                vec![],
                peko_auth::Exposure::Public,
            ))
            .await
            .unwrap();
        let hub_id = uuid::Uuid::new_v4().to_string();
        let config = principal.config.read().await;
        dispatcher
            .host
            .tunnel_channel_port()
            .register_actor(
                &ActorRef::from(&principal.id),
                Some(&hub_id),
                config.did.as_ref().map(|d| d.0.as_str()),
                config.actor_key.as_deref(),
                Some(name),
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();
        drop(config);
    }
    fn mock_tunnel_handle() -> (TunnelHandle, mpsc::Receiver<TunnelMessage>) {
        let (tx, rx) = mpsc::channel(crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE);
        (TunnelHandle::new(tx), rx)
    }

    fn chat_grant(subject: ActorRef) -> PermissionGrant {
        PermissionGrant {
            subject,
            permission: Permission::Chat,
            granted_at: "2026-06-30T00:00:00Z".to_string(),
            granted_by: ActorRef::User("user:owner".into()),
        }
    }

    /// Grant `Chat` on `name` to each subject (the runtime's ACL).
    async fn grant_chat(dispatcher: &TunnelDispatcher, name: &str, subjects: &[ActorRef]) {
        let grants: Vec<_> = subjects.iter().cloned().map(chat_grant).collect();
        dispatcher
            .host
            .principal_manager()
            .update_config(name, |config| config.permissions.extend(grants))
            .await
            .unwrap();
    }

    /// Build a minimal `PrincipalConfig` for dispatcher tests.
    fn test_principal_config(
        name: &str,
        owner: ActorRef,
        permissions: Vec<PermissionGrant>,
        exposure: peko_auth::Exposure,
    ) -> PrincipalConfig {
        PrincipalConfig {
            actor_key: None,
            name: name.to_string(),
            id: None,
            did: None,
            owner,
            identity: PrincipalIdentityConfig {
                display_name: Some(name.to_string()),
                description: Some(format!("Test principal {name}")),
                avatar: None,
            },
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            exposure,
            status: None,
            boot_state: None,
            permissions,
            preferred_model_id: None,
            quota: None,
            children: Default::default(),
        }
    }

    /// Create a Principal inside the given test `AppState`, including a
    /// default agent prompt so `PrincipalManager::create` succeeds.
    async fn create_test_principal(
        app_state: &AppState,
        name: &str,
        owner: ActorRef,
        permissions: Vec<PermissionGrant>,
        exposure: peko_auth::Exposure,
    ) -> std::sync::Arc<crate::principal::Principal> {
        let workspace = app_state.config.data_dir.join("principals").join(name);
        let roles_dir = workspace.join("roles");
        tokio::fs::create_dir_all(&roles_dir).await.unwrap();
        tokio::fs::write(
            roles_dir.join("primary.md"),
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

    /// ADR-057: no bridge request carries identity anymore — a request
    /// without a signed bridge token is rejected with `NoCaller`,
    /// whether or not a validator happens to be configured.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_requests_without_token() {
        let payload = serde_json::json!({"body": {"message": "hi"}});
        assert_eq!(
            resolve_bridge_caller(&payload, None).await,
            Err(BridgeCallerError::NoCaller)
        );

        let (validator, _signing_key) = ed25519_validator();
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            Err(BridgeCallerError::NoCaller)
        );

        // The retired `x-pekohub-user-id` header confers nothing.
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": "user-42"},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, None).await,
            Err(BridgeCallerError::NoCaller)
        );
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            Err(BridgeCallerError::NoCaller)
        );
    }

    // ─── Issue #17: JWT wiring (signed identity) ───────────────────────────

    use ed25519_dalek::{Signer, SigningKey};

    /// Build a `(validator, signing_key)` pair whose `validator` accepts
    /// tokens signed by `signing_key` against the runtime DID
    /// `did:key:z6MkTestRuntime` and the issuer `pekohub`.
    fn ed25519_validator() -> (peko_auth::jwt::JwtValidator, SigningKey) {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill(&mut bytes);
        let signing_key = SigningKey::from_bytes(&bytes);
        let verifying_key = signing_key.verifying_key();

        let x = URL_SAFE_NO_PAD.encode(verifying_key.to_bytes());
        let jwks = peko_auth::jwt::JwksResponse {
            keys: vec![peko_auth::jwt::JwkEntry {
                kty: "OKP".to_string(),
                kid: Some("test-key".to_string()),
                n: None,
                e: None,
                x: Some(x),
                crv: Some("Ed25519".to_string()),
                extra: std::collections::HashMap::new(),
            }],
        };
        let validator = peko_auth::jwt::JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );
        (validator, signing_key)
    }

    /// Mint an EdDSA JWT for the given sub, with the audience/issuer
    /// expected by `ed25519_validator()`. ADR-058 D5: carries the
    /// typed `kind: "user"` claim (see [`mint_jwt_with_kind`]).
    fn mint_jwt(signing_key: &SigningKey, sub: &str) -> String {
        mint_jwt_with_kind(signing_key, Some("user"), sub)
    }

    /// Mint an EdDSA JWT with an explicit `kind` claim (`None` omits
    /// the claim entirely — the D5 fail-closed test case).
    fn mint_jwt_with_kind(signing_key: &SigningKey, kind: Option<&str>, sub: &str) -> String {
        let header = serde_json::json!({"alg": "EdDSA", "typ": "JWT", "kid": "test-key"});
        let mut claims = serde_json::json!({
            "iss": "pekohub",
            "sub": sub,
            "jti": format!("test-jti-{sub}"),
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });
        if let Some(kind) = kind {
            claims["kind"] = serde_json::Value::String(kind.to_string());
        }
        let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims.to_string());
        let message = format!("{header_b64}.{claims_b64}");
        let signature = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
        format!("{message}.{sig_b64}")
    }

    /// When PekoHub sends an `Authorization: Bearer <jwt>` and a
    /// `JwtValidator` is configured, the validated `(kind, sub)` claims
    /// map to a typed caller `ActorRef` (issue #17 + ADR-058 D5).
    #[tokio::test]
    async fn resolve_bridge_caller_uses_validated_jwt_sub() {
        let (validator, signing_key) = ed25519_validator();
        let jwt = mint_jwt(&signing_key, "user-jwt");

        let payload = serde_json::json!({
            "headers": {"Authorization": format!("Bearer {jwt}")},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator))
                .await
                .unwrap(),
            ActorRef::User("hub:user-jwt".into())
        );
    }

    /// ADR-058 D5: `kind: "visitor"` maps to a `ActorRef::Visitor` —
    /// a distinct namespace that can never alias a user or principal.
    #[tokio::test]
    async fn resolve_bridge_caller_maps_visitor_kind_to_visitor_subject() {
        let (validator, signing_key) = ed25519_validator();
        let jwt = mint_jwt_with_kind(&signing_key, Some("visitor"), "vis_abc123");

        let payload = serde_json::json!({
            "headers": {"Authorization": format!("Bearer {jwt}")},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator))
                .await
                .unwrap(),
            ActorRef::Visitor("hub:vis_abc123".into())
        );
    }

    /// ADR-058 D5 (fail-closed): a validated token with NO `kind`
    /// claim is rejected — the untyped stringly-`sub` era is over.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_missing_kind_claim() {
        let (validator, signing_key) = ed25519_validator();
        let jwt = mint_jwt_with_kind(&signing_key, None, "user-jwt");

        let payload = serde_json::json!({
            "headers": {"Authorization": format!("Bearer {jwt}")},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            Err(BridgeCallerError::InvalidKind)
        );
    }

    /// ADR-058 D5: an unknown `kind` (including `principal` — a bridge
    /// token can never again produce a principal subject) is rejected.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_unknown_kind_claim() {
        let (validator, signing_key) = ed25519_validator();
        for kind in ["principal", "admin", ""] {
            let jwt = mint_jwt_with_kind(&signing_key, Some(kind), "did:key:z6MkVictim");
            let payload = serde_json::json!({
                "headers": {"Authorization": format!("Bearer {jwt}")},
            });
            assert_eq!(
                resolve_bridge_caller(&payload, Some(&validator)).await,
                Err(BridgeCallerError::InvalidJwt),
                "kind {kind:?} must be rejected"
            );
        }
    }

    /// ADR-058 D5: namespace-aliasing subs are rejected at the bridge —
    /// a visitor id of `principal:did:key:z…`, `user:<id>`, or `local`
    /// was the D5 forgery vector.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_namespace_aliasing_subs() {
        let (validator, signing_key) = ed25519_validator();
        for (kind, sub) in [
            ("visitor", "principal:did:key:z6MkVictim"),
            ("visitor", "user:39"),
            ("visitor", "local"),
            ("user", "principal:did:key:z6MkVictim"),
        ] {
            let jwt = mint_jwt_with_kind(&signing_key, Some(kind), sub);
            let payload = serde_json::json!({
                "headers": {"Authorization": format!("Bearer {jwt}")},
            });
            assert_eq!(
                resolve_bridge_caller(&payload, Some(&validator)).await,
                Err(BridgeCallerError::InvalidJwt),
                "(kind={kind:?}, sub={sub:?}) must be rejected"
            );
        }
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
            "headers": {"Authorization": format!("Bearer {wrong_jwt}")},
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

    /// Header-only (no JWT) → rejected when a JWT validator is
    /// configured. The unverified `x-pekohub-user-id` header is never
    /// trusted once JWT validation is enabled — that would let any
    /// caller impersonate any user simply by omitting the
    /// `Authorization` header.
    #[tokio::test]
    async fn resolve_bridge_caller_rejects_header_only_when_validator_configured() {
        let (validator, _signing_key) = ed25519_validator();
        let payload = serde_json::json!({
            "headers": {"x-pekohub-user-id": "user-hub"},
        });
        assert_eq!(
            resolve_bridge_caller(&payload, Some(&validator)).await,
            Err(BridgeCallerError::NoCaller)
        );
    }

    /// No JWT → rejected (no anonymous callers).
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
            ActorRef::User("hub:user-jwt".into())
        );
    }

    /// ADR-058 D4: a vault-backed (`did:key`) principal's announce
    /// PoP is a compact JWS over {"runtimeId","principalDid","iat","exp"}
    /// (exp = iat + 300) that verifies against the key embedded in the
    /// principal's DID.
    #[tokio::test]
    async fn principal_pop_mints_verifiable_jws_for_vault_backed_principal() {
        let app_state = create_test_app_state().await;
        let principal = create_test_principal(
            &app_state,
            "pop-agent",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Public,
        )
        .await;
        let did = principal.did().await.0;
        assert!(
            did.starts_with("did:key:"),
            "D1: genesis must mint a did:key principal DID; got {did}"
        );

        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let pop = dispatcher
            .principal_pop(&did)
            .await
            .expect("vault-backed did:key principal must produce a principalPop");

        let key = crate::tunnel::did_key::did_key_to_verifying_key(&did).unwrap();
        let (_segment, payload) = crate::tunnel::tunnel_channel_signature::jws_verify(&key, &pop)
            .expect("principalPop must verify against the principal DID key");
        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload["principalDid"].as_str(), Some(did.as_str()));
        assert_eq!(
            payload["runtimeId"].as_str(),
            Some(dispatcher.host.runtime_did().as_str())
        );
        let iat = payload["iat"].as_i64().expect("iat present");
        let exp = payload["exp"].as_i64().expect("exp present");
        assert_eq!(
            exp - iat,
            300,
            "PoP lifetime is 300s (iat=now, exp=now+300)"
        );
    }

    /// ADR-058 D4: legacy non-`did:key` principals announce without a
    /// PoP (the hub treats them as unverified — its policy).
    #[tokio::test]
    async fn principal_pop_is_none_for_legacy_did() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        assert!(dispatcher
            .principal_pop("did:peko:public:legacy:abc")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn test_handle_message_stores_tunnel_handle_synchronously() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
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
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
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
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
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
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        let status = dispatcher.get_instance_status("unknown-agent").await;
        assert_eq!(status, InstanceStatus::Online);
    }

    #[tokio::test]
    async fn test_handle_exposure_update_updates_local_state() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
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
        };

        dispatcher.handle_exposure_update(payload).await.unwrap();

        // Verify local state was updated
        let state = dispatcher.state.read().await;
        let entry = state
            .instance_state
            .get(&instance_id)
            .expect("Instance state should exist");
        assert_eq!(entry.exposure, InstanceExposure::Public);
    }

    #[tokio::test]
    async fn test_set_instance_status_updates_local_state() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
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
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (handle, _rx) = mock_tunnel_handle();

        {
            let mut state = dispatcher.state.write().await;
            state.tunnel_handle = Some(handle);
        }

        dispatcher
            .set_instance_exposure("my-agent", InstanceExposure::LocalOnly)
            .await
            .unwrap();

        let instance_id = dispatcher.instance_id("my-agent");
        let state = dispatcher.state.read().await;
        let entry = state.instance_state.get(&instance_id).unwrap();
        assert_eq!(entry.exposure, InstanceExposure::LocalOnly);
    }

    #[tokio::test]
    async fn test_check_request_allowed_public_allows_any_request() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "public-agent").await;
        let instance_id = dispatcher.instance_id("public-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Public,
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "any-user"}});
        let result = dispatcher
            .check_request_allowed(
                "public-agent",
                &bridge_payload,
                &ActorRef::User("any-user".into()),
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_check_request_allowed_unexposed_denies_any_request() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "unexposed-agent").await;
        let instance_id = dispatcher.instance_id("unexposed-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::LocalOnly,
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-123"}});
        let result = dispatcher
            .check_request_allowed(
                "unexposed-agent",
                &bridge_payload,
                &ActorRef::User("user-123".into()),
            )
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Agent is not exposed"));
    }

    #[tokio::test]
    async fn test_check_request_allowed_private_allows_allowed_user() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "private-agent").await;
        grant_chat(
            &dispatcher,
            "private-agent",
            &[ActorRef::User("user-123".into())],
        )
        .await;
        let instance_id = dispatcher.instance_id("private-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-123"}});
        let result = dispatcher
            .check_request_allowed(
                "private-agent",
                &bridge_payload,
                &ActorRef::User("user-123".into()),
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_check_request_allowed_private_without_user_id_denies() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "private-agent").await;
        grant_chat(
            &dispatcher,
            "private-agent",
            &[ActorRef::User("user-123".into())],
        )
        .await;
        let instance_id = dispatcher.instance_id("private-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {}});
        let result = dispatcher
            .check_request_allowed("private-agent", &bridge_payload, &ActorRef::Public)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Authentication required"));
    }

    #[tokio::test]
    async fn test_check_request_allowed_private_with_non_allowed_user_id_denies() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "private-agent").await;
        grant_chat(
            &dispatcher,
            "private-agent",
            &[ActorRef::User("user-123".into())],
        )
        .await;
        let instance_id = dispatcher.instance_id("private-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-999"}});
        let result = dispatcher
            .check_request_allowed(
                "private-agent",
                &bridge_payload,
                &ActorRef::User("user-999".into()),
            )
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Forbidden"));
    }

    /// ADR-058 D5: the Private-exposure ACL is kind-aware — a visitor
    /// subject matches only a visitor allow-list entry, never a user
    /// entry with the same bare id (and vice versa).
    #[tokio::test]
    async fn test_check_request_allowed_private_is_kind_aware() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "private-agent").await;
        grant_chat(
            &dispatcher,
            "private-agent",
            &[
                ActorRef::User("peer-1".into()),
                ActorRef::Visitor("vis-2".into()),
            ],
        )
        .await;
        let instance_id = dispatcher.instance_id("private-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    status: InstanceStatus::Online,
                },
            );
        }

        let bridge_payload = serde_json::json!({"headers": {}});
        // Exact kind + id matches are admitted.
        assert!(dispatcher
            .check_request_allowed(
                "private-agent",
                &bridge_payload,
                &ActorRef::User("peer-1".into()),
            )
            .await
            .is_ok());
        assert!(dispatcher
            .check_request_allowed(
                "private-agent",
                &bridge_payload,
                &ActorRef::Visitor("vis-2".into()),
            )
            .await
            .is_ok());
        // Cross-kind same-id is denied (no aliasing).
        assert!(dispatcher
            .check_request_allowed(
                "private-agent",
                &bridge_payload,
                &ActorRef::Visitor("peer-1".into()),
            )
            .await
            .is_err());
        assert!(dispatcher
            .check_request_allowed(
                "private-agent",
                &bridge_payload,
                &ActorRef::User("vis-2".into()),
            )
            .await
            .is_err());
    }

    /// ADR-081 D8: the owner reaches a private instance without a grant,
    /// and a registered principal's Hub alias matches its local grant.
    #[tokio::test]
    async fn test_check_request_allowed_private_admits_owner_and_aliases() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "private-agent").await;
        let owner = ActorRef::User("hub:owner".into());
        dispatcher
            .host
            .principal_manager()
            .update_config("private-agent", |config| config.owner = owner.clone())
            .await
            .unwrap();
        // A second registered principal, granted by its local ID.
        seed_registered(&dispatcher, "peer-agent").await;
        let peer = dispatcher
            .host
            .principal_manager()
            .get_by_name("peer-agent")
            .await
            .unwrap();
        let peer_local = ActorRef::from(&peer.id);
        let peer_hub = ActorRef::Principal(
            format!(
                "hub:{}",
                dispatcher
                    .host
                    .tunnel_channel_port()
                    .actor_info(&peer_local)
                    .await
                    .hub_id
                    .unwrap()
            )
            .into(),
        );
        grant_chat(&dispatcher, "private-agent", &[peer_local]).await;
        let instance_id = dispatcher.instance_id("private-agent");
        dispatcher.state.write().await.instance_state.insert(
            instance_id,
            InstanceState {
                exposure: InstanceExposure::Private,
                status: InstanceStatus::Online,
            },
        );

        let bridge_payload = serde_json::json!({"headers": {}});
        for caller in [owner, peer_hub] {
            assert!(dispatcher
                .check_request_allowed("private-agent", &bridge_payload, &caller)
                .await
                .is_ok());
        }
        assert!(dispatcher
            .check_request_allowed(
                "private-agent",
                &bridge_payload,
                &ActorRef::User("hub:stranger".into())
            )
            .await
            .is_err());
    }

    /// ADR-081 D7: when Hub moves the online link elsewhere, this copy goes
    /// offline-only; a notice naming another Hub link is ignored, and no
    /// directory claim can give the copy its Hub route back.
    /// ADR-081 D4: an online transfer is Hub's to make. The runtime asks,
    /// and takes the new owner only from Hub's confirmation; a refusal
    /// leaves the owner as it was.
    #[tokio::test]
    async fn an_online_transfer_takes_the_owner_hub_confirms() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        seed_registered(&dispatcher, "sold").await;
        let (handle, mut rx) = mock_tunnel_handle();
        {
            let mut state = dispatcher.state.write().await;
            state.tunnel_handle = Some(handle.clone());
            state.ready = true;
        }
        let principal = dispatcher
            .host
            .principal_manager()
            .get_by_name("sold")
            .await
            .unwrap();
        let alice = uuid::Uuid::new_v4().to_string();

        for accept in [false, true] {
            let asking = {
                let dispatcher = dispatcher.clone();
                let alice = alice.clone();
                tokio::spawn(async move { dispatcher.transfer_online("sold", &alice).await })
            };
            let TunnelMessage::PrincipalTransfer {
                request_id,
                local_id,
                hub_id,
                owner_id,
            } = rx.recv().await.unwrap()
            else {
                panic!("expected a transfer request");
            };
            assert_eq!(owner_id, alice);
            assert_eq!(local_id, principal.id.0);
            let answer = if accept {
                TunnelMessage::PrincipalOwnerChanged {
                    request_id: Some(request_id),
                    local_id,
                    hub_id,
                    owner_id: alice.clone(),
                }
            } else {
                TunnelMessage::PrincipalTransferRejected {
                    request_id,
                    message: "not the owner".into(),
                }
            };
            dispatcher.dispatch(answer, handle.clone()).await.unwrap();
            let result = asking.await.unwrap();
            let owner = principal.config.read().await.owner.clone();
            if accept {
                result.unwrap();
                assert_eq!(
                    owner,
                    ActorRef::User(peko_subject::Route::hub(alice.clone()))
                );
            } else {
                assert!(result.unwrap_err().to_string().contains("not the owner"));
                assert_eq!(
                    owner,
                    ActorRef::User("local".into()),
                    "a refusal changes nothing"
                );
            }
        }
    }

    /// ADR-081 D4: Hub's record wins over a local edit. A notice for
    /// another Hub link is ignored.
    #[tokio::test]
    async fn the_local_owner_follows_the_hub_record() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        seed_registered(&dispatcher, "edited").await;
        let (handle, _rx) = mock_tunnel_handle();
        let manager = dispatcher.host.principal_manager();
        let principal = manager.get_by_name("edited").await.unwrap();
        let hub = manager_hub_id(&dispatcher.host, &principal).await.unwrap();
        let recorded = uuid::Uuid::new_v4().to_string();
        // Someone edited the local owner.
        manager
            .update_config("edited", |c| c.owner = ActorRef::User("mallory".into()))
            .await
            .unwrap();

        let notice = |hub_id: &str| TunnelMessage::PrincipalOwnerChanged {
            request_id: None,
            local_id: principal.id.0.clone(),
            hub_id: hub_id.into(),
            owner_id: recorded.clone(),
        };
        dispatcher
            .dispatch(notice("another-hub-id"), handle.clone())
            .await
            .unwrap();
        assert_eq!(
            principal.config.read().await.owner,
            ActorRef::User("mallory".into())
        );
        dispatcher.dispatch(notice(&hub), handle).await.unwrap();
        assert_eq!(
            principal.config.read().await.owner,
            ActorRef::User(peko_subject::Route::hub(recorded.clone()))
        );
    }

    #[tokio::test]
    async fn an_unlinked_copy_goes_offline_only() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        seed_registered(&dispatcher, "mover").await;
        let principal = dispatcher
            .host
            .principal_manager()
            .get_by_name("mover")
            .await
            .unwrap();
        let port = dispatcher.host.tunnel_channel_port();
        let me = ActorRef::from(&principal.id);
        let hub = port.actor_info(&me).await.hub_id.unwrap();

        dispatcher
            .unlink_principal(&principal.id.0, "some-other-hub-id")
            .await
            .unwrap();
        assert_eq!(
            port.actor_info(&me).await.hub_id.as_deref(),
            Some(hub.as_str())
        );

        dispatcher
            .unlink_principal(&principal.id.0, &hub)
            .await
            .unwrap();
        assert!(port.actor_info(&me).await.hub_id.is_none());
        let config = principal.config.read().await.clone();
        assert_eq!(config.exposure, peko_auth::Exposure::LocalOnly);

        // A directory answer for the moved link cannot re-attach it here.
        assert!(port
            .register_actor(
                &ActorRef::Principal(peko_subject::Route::hub(hub.clone())),
                Some(&hub),
                config.did.as_ref().map(|d| d.0.as_str()),
                config.actor_key.as_deref(),
                None,
                peko_channel::Registration::Public,
            )
            .await
            .is_err());
        assert!(port.actor_info(&me).await.hub_id.is_none());
    }

    #[tokio::test]
    async fn test_check_request_allowed_missing_state_denies() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        let bridge_payload = serde_json::json!({"headers": {"x-pekohub-user-id": "user-123"}});
        let result = dispatcher
            .check_request_allowed(
                "unknown-agent",
                &bridge_payload,
                &ActorRef::User("user-123".into()),
            )
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Principal not loaded"));
    }

    // PR #2: `Unlisted` is reachable via the public URL with no
    // invite-token check until PR #11 narrows it. Behavior at the
    // runtime ACL level matches `Public`.
    #[tokio::test]
    async fn test_check_request_allowed_unlisted_allows_any_request() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        seed_registered(&dispatcher, "unlisted-agent").await;
        let instance_id = dispatcher.instance_id("unlisted-agent");
        {
            let mut state = dispatcher.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Public,
                    status: InstanceStatus::Online,
                },
            );
        }

        // No invite header, no user id — same shape as anonymous
        // public chat reaching the runtime through the hub.
        let bridge_payload = serde_json::json!({"headers": {}});
        let result = dispatcher
            .check_request_allowed("unlisted-agent", &bridge_payload, &ActorRef::Public)
            .await;
        assert!(result.is_ok());
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

        let dispatcher = TunnelDispatcher::new(Arc::new(app_state.clone()));
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

    #[tokio::test]
    async fn local_principals_are_not_announced_and_receipts_require_matching_request() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state.clone()));
        let (handle, mut rx) = mock_tunnel_handle();
        let principal = create_test_principal(
            &app_state,
            "announce-me",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Public,
        )
        .await;
        dispatcher.announce_instances(&handle).await.unwrap();
        assert!(rx.try_recv().is_err());
        let hub = uuid::Uuid::new_v4().to_string();
        let receipt = TunnelMessage::PrincipalRegistered {
            registration_id: Some("request".into()),
            local_id: principal.id.0.clone(),
            hub_id: hub.clone(),
            signing_did: principal.did().await.0,
        };
        assert!(dispatcher
            .dispatch(receipt.clone(), handle.clone())
            .await
            .is_err());
        assert!(manager_hub_id(&dispatcher.host, &principal).await.is_none());
        dispatcher
            .state
            .write()
            .await
            .pending_registrations
            .insert(principal.id.0.clone(), "request".into());
        dispatcher.dispatch(receipt, handle.clone()).await.unwrap();
        let profile = app_state
            .tunnel_channel_port()
            .resolve_actor(&format!("principal:hub:{hub}"))
            .await
            .unwrap();
        assert_eq!(profile.local_id.as_deref(), Some(principal.id.0.as_str()));
        assert_eq!(
            dispatcher
                .state
                .write()
                .await
                .registration_receipts
                .remove("request"),
            Some(Ok(hub))
        );
        dispatcher.announce_instances(&handle).await.unwrap();
        let TunnelMessage::InstanceAnnounce { payload } = rx.try_recv().unwrap() else {
            panic!("expected registered announcement")
        };
        assert_eq!(payload.local_id, principal.id.0);
        assert_eq!(
            payload.identity_key,
            principal.config.read().await.actor_key.clone().unwrap()
        );
    }

    /// The Hub marks an instance offline after 90 s without a heartbeat,
    /// and public chat to it answers 503. The heartbeat task was never
    /// started; `TunnelReady` now starts it and `Disconnect` stops it.
    #[tokio::test]
    async fn tunnel_ready_starts_instance_heartbeats_and_disconnect_stops_them() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state.clone()));
        let (handle, mut rx) = mock_tunnel_handle();
        create_test_principal(
            &app_state,
            "beats",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Public,
        )
        .await;

        dispatcher
            .dispatch(
                TunnelMessage::TunnelReady {
                    heartbeat_interval_secs: 30,
                },
                handle.clone(),
            )
            .await
            .unwrap();
        let beat = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(TunnelMessage::InstanceHeartbeat { payload }) = rx.recv().await {
                    break payload;
                }
            }
        })
        .await
        .expect("a heartbeat follows TunnelReady");
        assert_eq!(beat.id, dispatcher.instance_id("beats"));
        assert!(dispatcher.state.read().await.heartbeat_task.is_some());

        dispatcher
            .dispatch(
                TunnelMessage::Disconnect {
                    reason: "test".into(),
                },
                handle,
            )
            .await
            .unwrap();
        assert!(dispatcher.state.read().await.heartbeat_task.is_none());
    }

    /// A request without a valid bridge token is refused with a status
    /// response (403), which PekoHub turns into an error for the chat
    /// client. Before, the refusal went out as a reply chunk, and the
    /// client showed "Forbidden: …" as if the peko had said it.
    #[tokio::test]
    async fn an_unauthenticated_proxied_request_is_refused_with_a_status() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state.clone()));
        let (handle, mut rx) = mock_tunnel_handle();

        create_test_principal(
            &app_state,
            "web-bound",
            ActorRef::User("user:test-user".into()),
            vec![],
            peko_auth::Exposure::Public,
        )
        .await;

        // The retired `x-pekohub-user-id` header confers nothing.
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

        let TunnelMessage::ProxiedResponse {
            request_id,
            payload,
        } = rx.recv().await.expect("a response")
        else {
            panic!("a refusal is a status response, not a stream chunk");
        };
        assert_eq!(request_id, "req-web");
        let response: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(response["status"], 403);
        assert!(response["body"]
            .as_str()
            .unwrap()
            .starts_with("Forbidden: "));
    }

    #[test]
    fn a_denied_turn_is_a_403_in_plain_words() {
        use crate::principal::manager::PrincipalManagerError as E;
        let reason = "Permission denied: visitor:hub:x cannot perform Chat on principal:remy";
        assert_eq!(
            execution_failure("remy", &E::PermissionDenied(reason.into())),
            (403, "remy doesn't accept chats from you.".to_string())
        );
        assert_eq!(
            execution_failure("remy", &E::Config("bad".into())),
            (500, "Execution failed: config error: bad".to_string())
        );
    }

    /// Once reply text has streamed, a failure follows as text and the
    /// stream ends: a status response mid-stream would leave an older
    /// hub's stream open.
    #[tokio::test]
    async fn a_failure_after_streaming_ends_the_stream_as_text() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (handle, mut rx) = mock_tunnel_handle();
        dispatcher
            .send_error_response(&handle, "req-late", 500, "Execution failed: boom", true)
            .await
            .unwrap();
        assert!(matches!(
            rx.recv().await,
            Some(TunnelMessage::StreamChunk { ref request_id, .. }) if request_id == "req-late"
        ));
        assert!(matches!(
            rx.recv().await,
            Some(TunnelMessage::StreamEnd { ref request_id }) if request_id == "req-late"
        ));
    }

    // -----------------------------------------------------------------
    // peko-channel cross-runtime PR-A commit 3:
    // `TunnelChannelEvent` inbound handler tests.
    // -----------------------------------------------------------------

    /// Build a fresh signed `TunnelChannelEvent` envelope for tests.
    /// Mirrors the production outbound path (ADR-058 D3): serialize
    /// the event to JSON, build the v2 payload, sign a compact JWS
    /// with the source runtime's key. Tests use non-`did:key` authors
    /// (`prin_alice`, …), so no author signature is produced — the
    /// receiver accepts those as runtime-vouched.
    fn build_signed_tunnel_channel_event(
        kp: &peko_identity::keys::KeyPair,
        request_id: &str,
        source_runtime_id: &str,
        recipient_runtime_id: &str,
        source_principal_did: &str,
        channel_id: &str,
        event: &peko_protocol::channel::ChannelEvent,
    ) -> (String, peko_protocol::channel::ChannelEvent) {
        let event_bytes = serde_json::to_vec(event).expect("event must serialize");
        let signed = crate::tunnel::ChannelSignedFields {
            request_id,
            source_runtime_id,
            recipient_runtime_id,
            source_principal_did,
            channel_id,
            event_bytes: &event_bytes,
        };
        let iat = chrono::Utc::now().timestamp();
        let (sig, author_sig) =
            crate::tunnel::sign_channel_event(&kp.signing_key, None, signed, iat, iat + 300);
        assert!(author_sig.is_empty(), "no author key → no author JWS");
        (sig, event.clone())
    }

    /// The inbound `handle_inbound_tunnel_channel_event` accepts a
    /// valid signed envelope and returns Ok. The audit line is
    /// emitted (we don't capture tracing output here — the `Ok`
    /// contract is what the dispatcher relies on; the audit row
    /// emits unconditionally inside the handler).
    ///
    /// PR-A commit 3 stops short of the local-mirror append (that
    /// lands in PR-B), so this test only proves "routing +
    /// verification + audit emit do not error."
    #[tokio::test]
    async fn dispatcher_routes_valid_tunnel_channel_event_to_inbound_handler() {
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (_handle, _rx) = mock_tunnel_handle();

        let kp = peko_identity::keys::KeyPair::generate();
        let source_runtime_id = crate::tunnel::verifying_key_to_did_key(&kp.verifying_key);
        let event = peko_protocol::channel::ChannelEvent::Posted {
            channel: peko_protocol::channel::ChannelId("chan_abcdefgh".to_string()),
            author: "prin_alice".to_string(),
            parent: None,
            text: "hello from A".to_string(),
            at: "2026-08-06T12:00:00Z".to_string(),
            via: None,
            msg_id: None,
            reply_to: None,
            attachments: Vec::new(),
        };
        let (signature, event) = build_signed_tunnel_channel_event(
            &kp,
            "chan-evt-1",
            &source_runtime_id,
            &local_runtime_id,
            "prin_alice",
            "chan_abcdefgh",
            &event,
        );

        // Direct call to the inbound handler.
        dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-1".to_string(),
                source_runtime_id,
                local_runtime_id,
                "prin_alice".to_string(),
                "chan_abcdefgh".to_string(),
                event,
                signature,
                String::new(),
            )
            .await
            .expect("valid signed envelope must not error");
    }

    /// A `TunnelChannelEvent` whose signature does not verify (the
    /// envelope is intact but signed with a key that does not match
    /// the claimed `source_runtime_id`) is dropped with a warn. The
    /// handler returns `Ok(())` so the dispatcher does not propagate
    /// the rejection up the call stack — channel events have no
    /// response, so there's nothing to error against.
    #[tokio::test]
    async fn dispatcher_drops_tunnel_channel_event_with_invalid_signature() {
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (_handle, _rx) = mock_tunnel_handle();

        let kp_signer = peko_identity::keys::KeyPair::generate();
        let kp_claimed = peko_identity::keys::KeyPair::generate();
        // Claimed source_runtime_id is derived from a DIFFERENT key
        // than the one that signed the envelope — signature
        // verification MUST fail.
        let claimed_source_runtime_id =
            crate::tunnel::verifying_key_to_did_key(&kp_claimed.verifying_key);
        let event = peko_protocol::channel::ChannelEvent::Posted {
            channel: peko_protocol::channel::ChannelId("chan_abcdefgh".to_string()),
            author: "prin_alice".to_string(),
            parent: None,
            text: "tampered".to_string(),
            at: "2026-08-06T12:00:00Z".to_string(),
            via: None,
            msg_id: None,
            reply_to: None,
            attachments: Vec::new(),
        };
        let (signature, event) = build_signed_tunnel_channel_event(
            &kp_signer,
            "chan-evt-tampered",
            &claimed_source_runtime_id,
            &local_runtime_id,
            "prin_alice",
            "chan_abcdefgh",
            &event,
        );

        let result = dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-tampered".to_string(),
                claimed_source_runtime_id,
                local_runtime_id,
                "prin_alice".to_string(),
                "chan_abcdefgh".to_string(),
                event,
                signature,
                String::new(),
            )
            .await;
        // Returns Ok — the rejection is logged and dropped, not
        // propagated. Same shape as the DM path's signature-failure
        // branch (which DOES propagate because there's a hub error
        // envelope to send back; channels have no such envelope).
        assert!(
            result.is_ok(),
            "invalid-signature rejection must not propagate; got: {result:?}"
        );
    }

    /// An invalid `source_runtime_id` (not a valid `did:key:z...`)
    /// is dropped without panicking. Mirrors the same defense-in-
    /// depth branch in the DM path.
    #[tokio::test]
    async fn dispatcher_drops_tunnel_channel_event_with_invalid_runtime_did() {
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (_handle, _rx) = mock_tunnel_handle();

        let event = peko_protocol::channel::ChannelEvent::Posted {
            channel: peko_protocol::channel::ChannelId("chan_abcdefgh".to_string()),
            author: "prin_alice".to_string(),
            parent: None,
            text: "hello".to_string(),
            at: "2026-08-06T12:00:00Z".to_string(),
            via: None,
            msg_id: None,
            reply_to: None,
            attachments: Vec::new(),
        };

        let result = dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-bad-did".to_string(),
                "did:peko:not-a-key".to_string(), // wrong scheme entirely
                local_runtime_id,
                "prin_alice".to_string(),
                "chan_abcdefgh".to_string(),
                event,
                "any-base64url".to_string(),
                String::new(),
            )
            .await;
        assert!(
            result.is_ok(),
            "invalid DID rejection must not propagate; got: {result:?}"
        );
    }

    /// peko-channel cross-runtime PR-B commit 2: the inbound
    /// `TunnelChannelEvent` appends the event to the local mirror's
    /// `events.jsonl`. PR-A commit 3 verified-and-audited-and-dropped;
    /// commit 2 wires `self.host.tunnel_channel_port().append_remote_event(...)`
    /// so the receiver's local `peek` shows the event.
    ///
    /// The test creates the channel locally first (via
    /// `app_state.channel_port.create(...)`) so the mirror directory
    /// exists; the dispatcher then appends a Posted event from
    /// runtime B into runtime A's mirror and the assertion reads back
    /// via `peek`.
    #[tokio::test]
    async fn inbound_tunnel_channel_event_appends_to_local_mirror() {
        use peko_channel::ChannelPort;
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let creator = peko_subject::PrincipalId("prin_alice".into());
        let channel = app_state
            .channel_port()
            .create(&creator, peko_channel::CreateOpts::runtime("team"))
            .await
            .expect("local channel create must succeed");
        let channel_id_str = channel.as_str().to_string();

        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (_handle, _rx) = mock_tunnel_handle();

        let kp = peko_identity::keys::KeyPair::generate();
        let source_runtime_id = crate::tunnel::verifying_key_to_did_key(&kp.verifying_key);
        let event = peko_protocol::channel::ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_bob@runtime-B".into(),
            parent: None,
            text: "hello from B".into(),
            at: "2026-08-06T12:00:00Z".into(),
            via: None,
            msg_id: None,
            reply_to: None,
            attachments: Vec::new(),
        };
        let (signature, event) = build_signed_tunnel_channel_event(
            &kp,
            "chan-evt-mirror",
            &source_runtime_id,
            &local_runtime_id,
            "prin_bob@runtime-B",
            &channel_id_str,
            &event,
        );

        dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-mirror".to_string(),
                source_runtime_id,
                local_runtime_id,
                "prin_bob@runtime-B".to_string(),
                channel_id_str.clone(),
                event,
                signature,
                String::new(),
            )
            .await
            .expect("verified envelope must not error");

        // The local mirror's `peek` should now show the remote event.
        let events = dispatcher
            .host
            .tunnel_channel_port()
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .expect("local peek must succeed");
        let posted: Vec<_> = events
            .iter()
            .filter(|ev| matches!(ev, peko_protocol::channel::ChannelEvent::Posted { .. }))
            .collect();
        assert!(
            posted.is_empty(),
            "Retired asserted principal identities must not be accepted"
        );
    }

    /// Delivery receipts: an inbound envelope is applied once and
    /// confirmed every time it arrives (a resend after a lost receipt is
    /// confirmed again, not appended twice); an inbound receipt confirms
    /// only the outbox entries addressed to its sender, and is never
    /// appended to a log.
    #[tokio::test]
    async fn inbound_envelopes_are_applied_once_and_confirmed_every_time() {
        use peko_channel::ChannelPort;
        use peko_protocol::channel::ChannelEvent;
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let creator = peko_subject::PrincipalId("prin_alice".into());
        let channel = app_state
            .channel_port()
            .create(&creator, peko_channel::CreateOpts::runtime("team"))
            .await
            .unwrap();
        let channel_id = channel.as_str().to_string();
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let port = dispatcher.host.tunnel_channel_port();

        let author_key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        port.register_actor(
            &ActorRef::from(&creator),
            Some("hub-creator"),
            Some(&crate::tunnel::verifying_key_to_did_key(
                &author_key.verifying_key(),
            )),
            None,
            None,
            peko_channel::Registration::Public,
        )
        .await
        .unwrap();
        // A live (captured) tunnel so receipts and posts leave the runtime.
        let (tx, mut outbound) = tokio::sync::mpsc::channel::<TunnelMessage>(16);
        port.set_ctx(Arc::new(crate::tunnel::CrossRuntimeChannelCtx {
            directory: Arc::new(crate::tunnel::hub_directory::FakeAgentDirectory::default()),
            signing_key: Arc::new(ed25519_dalek::SigningKey::from_bytes(&[3u8; 32])),
            caller_runtime_id: local_runtime_id.clone(),
            tunnel: Arc::new(tokio::sync::RwLock::new(Some(TunnelHandle::new(tx)))),
            principal_keys: Arc::new(crate::tunnel::cross_runtime_channel::TestPrincipalKeys(
                vec![(
                    creator.clone(),
                    Arc::new(ed25519_dalek::SigningKey::from_bytes(&[8; 32])),
                )],
            )),
        }))
        .await;

        let kp = peko_identity::keys::KeyPair::generate();
        let source = crate::tunnel::verifying_key_to_did_key(&kp.verifying_key);
        port.register_actor(
            &ActorRef::User("hub:bob".into()),
            Some("bob"),
            None,
            None,
            None,
            peko_channel::Registration::Registered,
        )
        .await
        .unwrap();
        port.add_remote_member(&channel, &source, "user:hub:bob")
            .await
            .unwrap();
        let deliver = |request_id: &'static str, event: ChannelEvent| {
            let (signature, event) = build_signed_tunnel_channel_event(
                &kp,
                request_id,
                &source,
                &local_runtime_id,
                "user:hub:bob",
                &channel_id,
                &event,
            );
            dispatcher.handle_inbound_tunnel_channel_event(
                request_id.to_string(),
                source.clone(),
                local_runtime_id.clone(),
                "user:hub:bob".to_string(),
                channel_id.clone(),
                event,
                signature,
                String::new(),
            )
        };
        let receipt_for = |message: TunnelMessage| match message {
            TunnelMessage::TunnelChannelEvent {
                recipient_runtime_id,
                event: ChannelEvent::Received { request_ids, .. },
                ..
            } => (recipient_runtime_id, request_ids),
            other => panic!("expected a receipt, got {other:?}"),
        };

        let post = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "user:hub:bob".into(),
            parent: None,
            text: "hello".into(),
            at: "2026-10-12T00:00:00Z".into(),
            via: None,
            msg_id: Some("msg_hello".into()),
            reply_to: None,
            attachments: Vec::new(),
        };
        deliver("req-1", post.clone()).await.unwrap();
        deliver("req-1", post).await.unwrap();
        let posted = port
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|ev| matches!(ev, ChannelEvent::Posted { .. }))
            .count();
        assert_eq!(posted, 1, "applied once");
        for _ in 0..2 {
            let (to, ids) = receipt_for(outbound.try_recv().expect("a receipt per arrival"));
            assert_eq!(to, source);
            assert_eq!(ids, ["req-1"]);
        }

        // Our own post to the source runtime waits in the outbox until the
        // source confirms it; the receipt itself is not logged.
        port.add_remote_member(&channel, &source, "user:hub:bob")
            .await
            .unwrap();
        port.post(
            &channel,
            &peko_subject::ActorRef::from(&creator),
            peko_channel::PostMsg::root("ours").with_msg_id("msg_ours"),
        )
        .await
        .unwrap();
        let sent = match outbound.try_recv().unwrap() {
            TunnelMessage::TunnelChannelEvent { request_id, .. } => request_id,
            other => panic!("expected our post, got {other:?}"),
        };
        assert!(
            !port
                .wait_delivered("msg_ours", std::time::Duration::from_millis(10))
                .await
        );
        let before = port
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap()
            .len();
        let receipt = ChannelEvent::Received {
            channel: channel.clone(),
            request_ids: vec![sent],
            at: "2026-10-12T00:00:01Z".into(),
        };
        deliver("req-receipt", receipt).await.unwrap();
        assert!(
            port.wait_delivered("msg_ours", std::time::Duration::from_millis(10))
                .await
        );
        let after = port
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap()
            .len();
        assert_eq!(after, before, "a receipt is never logged");
        assert!(
            outbound.try_recv().is_err(),
            "a receipt is not itself confirmed"
        );
    }

    /// peko-channel cross-runtime PR-B commit 2: the inbound handler
    /// surfaces `Ok(())` when the channel id is unknown (defense in
    /// depth — the hub's source-allowlist + signature check is the
    /// primary gate; a bogus channel id on a verified envelope
    /// shouldn't backpressure the dispatcher).
    #[tokio::test]
    async fn inbound_tunnel_channel_event_unknown_channel_silently_drops() {
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (_handle, _rx) = mock_tunnel_handle();

        let kp = peko_identity::keys::KeyPair::generate();
        let source_runtime_id = crate::tunnel::verifying_key_to_did_key(&kp.verifying_key);
        let bogus = peko_protocol::channel::ChannelId("chan_unknown01".into());
        let event = peko_protocol::channel::ChannelEvent::Posted {
            channel: bogus.clone(),
            author: "prin_bob".into(),
            parent: None,
            text: "should not land".into(),
            at: "2026-08-06T12:00:00Z".into(),
            via: None,
            msg_id: None,
            reply_to: None,
            attachments: Vec::new(),
        };
        let (signature, event) = build_signed_tunnel_channel_event(
            &kp,
            "chan-evt-unknown",
            &source_runtime_id,
            &local_runtime_id,
            "prin_bob",
            bogus.as_str(),
            &event,
        );

        let result = dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-unknown".to_string(),
                source_runtime_id,
                local_runtime_id,
                "prin_bob".to_string(),
                bogus.as_str().to_string(),
                event,
                signature,
                String::new(),
            )
            .await;
        assert!(
            result.is_ok(),
            "unknown channel must not propagate as an error (channels are push-only); got: {result:?}"
        );
    }

    /// ADR-058 D2: a `did:key`-authored event is accepted only when
    /// the author signature verifies over the same payload segment
    /// AND the author is a registered remote member of the mirror. A
    /// did:key author with no membership row — or with an empty
    /// author signature — is dropped even when every other check
    /// passes.
    #[tokio::test]
    async fn inbound_did_key_author_requires_author_signature_and_membership() {
        use peko_channel::ChannelPort;
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let creator = peko_subject::PrincipalId("prin_alice".into());
        let channel = app_state
            .channel_port()
            .create(&creator, peko_channel::CreateOpts::runtime("team"))
            .await
            .expect("local channel create must succeed");
        let channel_id_str = channel.as_str().to_string();

        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));
        let (_handle, _rx) = mock_tunnel_handle();
        // The local member is online (ADR-081 D7: only the linked copy receives).
        dispatcher
            .host
            .tunnel_channel_port()
            .register_actor(
                &ActorRef::from(&creator),
                Some("hub-creator"),
                None,
                None,
                None,
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();

        let kp = peko_identity::keys::KeyPair::generate();
        let source_runtime_id = crate::tunnel::verifying_key_to_did_key(&kp.verifying_key);
        let author = peko_identity::keys::KeyPair::generate();
        let author_did = crate::tunnel::verifying_key_to_did_key(&author.verifying_key);

        // Build a dual-signed (runtime + author) envelope for the
        // did:key author, mirroring the production outbound path.
        dispatcher
            .host
            .tunnel_channel_port()
            .register_actor(
                &ActorRef::Principal("hub:author-hub".into()),
                Some("author-hub"),
                Some(&author_did),
                Some("author-key"),
                Some("Author"),
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();
        let build = |request_id: &str| {
            let event = peko_protocol::channel::ChannelEvent::Posted {
                channel: channel.clone(),
                author: "principal:hub:author-hub".into(),
                parent: None,
                text: "hello from the did:key author".to_string(),
                at: "2026-09-16T00:00:00Z".to_string(),
                via: None,
                msg_id: None,
                reply_to: None,
                attachments: Vec::new(),
            };
            let event_bytes = serde_json::to_vec(&event).expect("event must serialize");
            let signed = crate::tunnel::ChannelSignedFields {
                request_id,
                source_runtime_id: &source_runtime_id,
                recipient_runtime_id: &local_runtime_id,
                source_principal_did: &author_did,
                channel_id: &channel_id_str,
                event_bytes: &event_bytes,
            };
            let iat = chrono::Utc::now().timestamp();
            let (sig, author_sig) = crate::tunnel::sign_channel_event(
                &kp.signing_key,
                Some(&author.signing_key),
                signed,
                iat,
                iat + 300,
            );
            (sig, author_sig, event)
        };

        let peek_len = || async {
            dispatcher
                .host
                .tunnel_channel_port()
                .peek(&channel, &peko_channel::Checkpoint::default())
                .await
                .expect("local peek must succeed")
                .len()
        };

        // 1. Valid author signature but NO membership row → dropped.
        let (sig, author_sig, event) = build("chan-evt-did-nomember");
        dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-did-nomember".to_string(),
                source_runtime_id.clone(),
                local_runtime_id.clone(),
                author_did.clone(),
                channel_id_str.clone(),
                event,
                sig,
                author_sig,
            )
            .await
            .expect("drop must not error");
        assert_eq!(
            peek_len().await,
            1,
            "only Created; a did:key event without remote membership must be dropped"
        );

        // 2. did:key author with an EMPTY author signature → dropped.
        let (sig, _author_sig, event) = build("chan-evt-did-nosig");
        dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-did-nosig".to_string(),
                source_runtime_id.clone(),
                local_runtime_id.clone(),
                author_did.clone(),
                channel_id_str.clone(),
                event,
                sig,
                String::new(),
            )
            .await
            .expect("drop must not error");
        assert_eq!(
            peek_len().await,
            1,
            "a did:key event with no author signature must be dropped"
        );

        // 3. Register the author as a remote member of the mirror →
        //    the dual-signed envelope is accepted and appended.
        dispatcher
            .host
            .tunnel_channel_port()
            .add_remote_member(&channel, &source_runtime_id, &author_did)
            .await
            .expect("add_remote_member must succeed");
        let (sig, author_sig, event) = build("chan-evt-did-member");
        dispatcher
            .handle_inbound_tunnel_channel_event(
                "chan-evt-did-member".to_string(),
                source_runtime_id,
                local_runtime_id,
                author_did,
                channel_id_str,
                event,
                sig,
                author_sig,
            )
            .await
            .expect("verified envelope must not error");
        assert_eq!(
            peek_len().await,
            2,
            "Created + the accepted did:key-authored Posted"
        );
    }

    // -----------------------------------------------------------------
    // peko-channel cross-runtime PR-3a commit 3:
    // `TunnelChannelInvite` inbound handler tests.
    // -----------------------------------------------------------------

    /// Build a fresh signed `TunnelChannelInvite` envelope for tests.
    /// Mirrors the production outbound path (ADR-058 D3):
    /// `fanout_dm_invite` → serialize initial_members → build the v2
    /// payload → sign a compact JWS with the source runtime's key.
    /// Tests use non-`did:key` creators, so no author signature.
    #[allow(clippy::too_many_arguments)]
    fn build_signed_tunnel_channel_invite(
        kp: &peko_identity::keys::KeyPair,
        request_id: &str,
        source_runtime_id: &str,
        recipient_runtime_id: &str,
        source_principal_did: &str,
        channel_id: &str,
        creator: &str,
        creator_did: &str,
        name: &str,
        kind: peko_channel::ConversationKind,
        initial_members: &[peko_protocol::channel::InitialMember],
    ) -> (String, Vec<peko_protocol::channel::InitialMember>) {
        let initial_members_bytes =
            serde_json::to_vec(initial_members).expect("initial_members must serialize");
        let signed = crate::tunnel::ChannelInviteSignedFields {
            request_id,
            source_runtime_id,
            recipient_runtime_id,
            source_principal_did,
            channel_id,
            creator,
            creator_did,
            name,
            kind: kind.as_str(),
            initial_members_bytes: &initial_members_bytes,
        };
        let iat = chrono::Utc::now().timestamp();
        let (sig, author_sig) =
            crate::tunnel::sign_channel_invite(&kp.signing_key, None, signed, iat, iat + 300);
        assert!(author_sig.is_empty(), "no author key → no author JWS");
        (sig, initial_members.to_vec())
    }

    /// Inbound `TunnelChannelInvite` with a valid signature: the
    /// handler returns `Ok(())` and the local mirror gets
    /// bootstrapped (meta.json + members.json + a synthetic Created
    /// event lands at line 0). Phase 12a: the invited LOCAL principal
    /// is resolved from the invitee row's DID, the receiver's own id
    /// becomes the only local member, and the source's rows are filed
    /// as remote — so the receiver can post to its own mirror and its
    /// posts fan back out. The synthetic Created event keeps PR-2b's
    /// `peko-stream` desktop listener firing.
    async fn registered_invite_roundtrip(dm: bool) {
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_channel::{ChannelEvent, Checkpoint};
        let app = create_test_app_state().await;
        let principal = create_test_principal(
            &app,
            "invitee",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Public,
        )
        .await;
        let target_hub = uuid::Uuid::new_v4().to_string();
        let source_hub = uuid::Uuid::new_v4().to_string();
        let config = principal.config.read().await;
        app.tunnel_channel_port()
            .register_actor(
                &ActorRef::from(&principal.id),
                Some(&target_hub),
                config.did.as_ref().map(|d| d.0.as_str()),
                config.actor_key.as_deref(),
                Some("invitee"),
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();
        let target_key = config.actor_key.clone().unwrap();
        drop(config);
        let runtime = peko_identity::keys::KeyPair::generate();
        let signer = peko_identity::keys::KeyPair::generate();
        let source_runtime = crate::tunnel::verifying_key_to_did_key(&runtime.verifying_key);
        let source_did = crate::tunnel::verifying_key_to_did_key(&signer.verifying_key);
        let source_key = "immutable-source-key";
        let directory = Arc::new(FakeAgentDirectory::new());
        directory.register_hub_id(
            &source_hub,
            AgentResolution {
                name: None,
                hub_id: source_hub.clone(),
                local_id: "prin_source".into(),
                identity_key: source_key.into(),
                runtime_id: source_runtime.clone(),
                instance_id: "source-instance".into(),
                agent_did: source_did.clone(),
                owner_principal: ActorRef::Public,
                exposure: ResolvedExposure::Public,
            },
        );
        app.tool_runtime
            .tooling()
            .services()
            .set_cross_runtime_a2a_ctx(Arc::new(crate::tunnel::CrossRuntimeA2aCtx {
                directory,
                caller_runtime_id: app.runtime_did(),
                principal_manager: app.principal_manager().clone(),
                channel_port: app.tunnel_channel_port(),
            }));
        let channel = if dm {
            peko_channel::dm_channel_id(&target_key, source_key)
        } else {
            peko_channel::ChannelId::generate()
        };
        let creator = format!("principal:hub:{source_hub}");
        let members = vec![
            peko_protocol::channel::InitialMember {
                principal_did: source_did.clone(),
                runtime_id: Some(source_runtime.clone()),
                hub_id: Some(source_hub.clone()),
                identity_key: Some(source_key.into()),
                name: Some("source".into()),
            },
            peko_protocol::channel::InitialMember {
                principal_did: principal.did().await.0,
                runtime_id: None,
                hub_id: Some(target_hub),
                identity_key: Some(target_key),
                name: Some("invitee".into()),
            },
        ];
        let bytes = serde_json::to_vec(&members).unwrap();
        let kind = if dm {
            peko_channel::ConversationKind::Dm
        } else {
            peko_channel::ConversationKind::Group
        };
        let now = chrono::Utc::now().timestamp();
        let fields = crate::tunnel::ChannelInviteSignedFields {
            request_id: "registered-invite",
            source_runtime_id: &source_runtime,
            recipient_runtime_id: &app.runtime_did(),
            source_principal_did: &source_did,
            channel_id: channel.as_str(),
            creator: &creator,
            creator_did: &source_did,
            name: "registered conversation",
            kind: kind.as_str(),
            initial_members_bytes: &bytes,
        };
        let (signature, author_signature) = crate::tunnel::sign_channel_invite(
            &runtime.signing_key,
            Some(&signer.signing_key),
            fields,
            now,
            now + 300,
        );
        let dispatcher = TunnelDispatcher::new(Arc::new(app.clone()));
        dispatcher
            .handle_inbound_tunnel_channel_invite(
                "registered-invite".into(),
                source_runtime.clone(),
                app.runtime_did(),
                source_did.clone(),
                channel.0.clone(),
                creator.clone(),
                source_did.clone(),
                "registered conversation".into(),
                kind,
                members,
                signature,
                author_signature,
            )
            .await
            .unwrap();
        let port = app.tunnel_channel_port();
        assert_eq!(
            port.peek(&channel, &Checkpoint::default())
                .await
                .unwrap()
                .len(),
            1
        );
        let profile = port.resolve_actor(&creator).await.unwrap();
        assert_eq!(profile.local_id, None);
        assert_eq!(profile.address, creator);
        assert_eq!(port.participants(&channel).await.unwrap().len(), 2);
        if dm {
            assert!(app
                .channel_binding_supervisor()
                .has_subscriber(&principal.id, &channel));
            let receiver_binding = port
                .binding_for(&channel, &principal.id)
                .await
                .unwrap()
                .unwrap();
            assert!(receiver_binding.session.starts_with('/'));
            assert_eq!(receiver_binding.wake, peko_channel::WakePolicy::Human);
            let metas = peko_session::manager::SessionManager::new()
                .with_sessions_dir_internal(principal.memory.sessions_dir())
                .list_all_sessions(false)
                .await
                .unwrap();
            let meta = metas
                .iter()
                .find(|m| m.channel_id.as_deref() == Some(channel.as_str()))
                .unwrap();
            assert!(meta.peer_id.is_none() && meta.peer_type.is_none());
        }
        for (request, author) in [
            ("authentic-post", creator),
            (
                "spoofed-post",
                format!("principal:hub:{}", uuid::Uuid::new_v4()),
            ),
        ] {
            let event = ChannelEvent::Posted {
                channel: channel.clone(),
                author,
                parent: None,
                text: request.into(),
                at: chrono::Utc::now().to_rfc3339(),
                via: None,
                msg_id: Some(request.into()),
                reply_to: None,
                attachments: Vec::new(),
            };
            let bytes = serde_json::to_vec(&event).unwrap();
            let fields = crate::tunnel::ChannelSignedFields {
                request_id: request,
                source_runtime_id: &source_runtime,
                recipient_runtime_id: &app.runtime_did(),
                source_principal_did: &source_did,
                channel_id: channel.as_str(),
                event_bytes: &bytes,
            };
            let (signature, author_signature) = crate::tunnel::sign_channel_event(
                &runtime.signing_key,
                Some(&signer.signing_key),
                fields,
                now,
                now + 300,
            );
            dispatcher
                .handle_inbound_tunnel_channel_event(
                    request.into(),
                    source_runtime.clone(),
                    app.runtime_did(),
                    source_did.clone(),
                    channel.0.clone(),
                    event,
                    signature,
                    author_signature,
                )
                .await
                .unwrap();
        }
        let events = port.peek(&channel, &Checkpoint::default()).await.unwrap();
        assert_eq!(
            events.len(),
            2,
            "only the authenticated author's post is accepted"
        );
        assert!(matches!(&events[1], ChannelEvent::Posted {author,..} if author==&profile.address));
    }

    #[tokio::test]
    async fn inbound_registered_group_invite_and_message_verify_both_ids() {
        registered_invite_roundtrip(false).await;
    }
    #[tokio::test]
    async fn inbound_registered_dm_invite_binds_only_the_channel() {
        registered_invite_roundtrip(true).await;
    }

    /// An invite whose invitee row matches NO loaded principal is
    /// logged and skipped (no mirror, no error) — invites are
    /// push-only and the receiver may simply not host the addressee.
    #[tokio::test]
    async fn inbound_invite_for_unknown_principal_is_skipped() {
        use peko_channel::ChannelPort;
        let app_state = create_test_app_state().await;
        let local_runtime_id = app_state.runtime_did();
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state));

        let kp = peko_identity::keys::KeyPair::generate();
        let source_runtime_id = crate::tunnel::verifying_key_to_did_key(&kp.verifying_key);
        let channel_id = "chan_unknown01";
        let initial_members = vec![peko_protocol::channel::InitialMember {
            hub_id: None,
            identity_key: None,
            name: None,
            principal_did: "did:peko:principal:nobody".to_string(),
            runtime_id: None,
        }];
        let (signature, initial_members) = build_signed_tunnel_channel_invite(
            &kp,
            "chan-invite-unknown",
            &source_runtime_id,
            &local_runtime_id,
            "prin_alice",
            channel_id,
            "prin_alice",
            "did:peko:principal:alice",
            "team-chat",
            peko_channel::ConversationKind::Group,
            &initial_members,
        );

        dispatcher
            .handle_inbound_tunnel_channel_invite(
                "chan-invite-unknown".to_string(),
                source_runtime_id,
                local_runtime_id,
                "prin_alice".to_string(),
                channel_id.to_string(),
                "prin_alice".to_string(),
                "did:peko:principal:alice".to_string(),
                "team-chat".to_string(),
                peko_channel::ConversationKind::Group,
                initial_members,
                signature,
                String::new(),
            )
            .await
            .expect("unknown-invitee invite must not error");

        let port = dispatcher.host.tunnel_channel_port();
        let local_store: &peko_channel::ChannelStore = port.local();
        let result = local_store
            .peek(
                &peko_protocol::channel::ChannelId(channel_id.to_string()),
                &peko_channel::Checkpoint::default(),
            )
            .await;
        assert!(
            matches!(result, Err(peko_channel::ChannelError::NotFound(_))),
            "no mirror may be bootstrapped for an unknown invitee; got: {result:?}"
        );
    }
}
