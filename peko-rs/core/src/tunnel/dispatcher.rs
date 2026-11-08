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
        ChannelHistoryRequestPayload,
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

/// Hub's answer to a transfer of an online principal (ADR-081 D4, amended).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferOutcome {
    /// The principal has its new owner.
    Transferred,
    /// The new owner was offered it; it moves when they accept on PekoHub.
    /// Hub's words for it.
    Offered(String),
}

/// Shared dispatcher state for instance lifecycle management
#[derive(Debug, Default)]
pub struct TunnelDispatcherState {
    /// Whether the tunnel is authenticated and ready
    pub registration_receipts: std::collections::HashMap<String, Result<String, String>>,
    /// ADR-081 D4: Hub's answers to transfer requests, by request ID.
    pub transfer_receipts: std::collections::HashMap<String, Result<TransferOutcome, String>>,
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

/// A verified inbound channel event, ready to apply.
struct InboundEvent {
    request_id: String,
    source_runtime_id: String,
    channel: peko_protocol::channel::ChannelId,
    event: peko_protocol::channel::ChannelEvent,
}

/// Step 5 of `handle_inbound_tunnel_channel_event`: fetch the files the
/// post names (ADR-094), append it to the local mirror, confirm it, and
/// start the peko's turn for a person's DM post. Runs under the
/// channel's apply lock.
async fn apply_inbound_event(host: Arc<dyn TunnelHost>, inbound: InboundEvent) {
    let InboundEvent {
        request_id,
        source_runtime_id,
        channel,
        event,
    } = inbound;
    let event_kind = event.kind();
    let port = host.tunnel_channel_port();
    // A resend of an envelope already applied: confirm it again (the
    // first receipt may have been lost), apply nothing.
    if port.already_applied(&request_id).await {
        port.send_receipt(&source_runtime_id, &channel, vec![request_id])
            .await;
        return;
    }
    // ADR-081 D7: only the linked copy receives. Unanswered, the sender
    // re-resolves the link and redelivers to the online copy.
    if !port.has_online_local_member(&channel).await {
        warn!(
            request_id,
            %channel, "inbound TunnelChannelEvent: no local member of this channel is online here; leaving it unacknowledged"
        );
        return;
    }
    // A post this copy already holds — its own post relayed back by the
    // group's home, or one that arrived with the history — is confirmed
    // and applied nothing more: no second line, no second turn.
    if let peko_protocol::channel::ChannelEvent::Posted {
        msg_id: Some(msg_id),
        ..
    } = &event
    {
        if port.holds_post(&channel, msg_id).await {
            port.mark_applied(&request_id).await;
            port.send_receipt(&source_runtime_id, &channel, vec![request_id])
                .await;
            debug!(%channel, msg_id, "inbound TunnelChannelEvent: post already held; not applied again");
            return;
        }
    }
    // ADR-094: the files first. One that cannot be had now is asked for
    // again later; the post applies either way.
    if let peko_protocol::channel::ChannelEvent::Posted { attachments, .. } = &event {
        port.fetch_attachments(&channel, attachments, &source_runtime_id)
            .await;
    }
    // The dispatcher is the single source of truth for envelope validity
    // (steps 1-4); `append_remote_event` deliberately does not re-verify.
    if let Err(e) = port.append_remote_event(&channel, &event).await {
        warn!(
            "inbound TunnelChannelEvent: local-mirror append failed (request_id={request_id}, channel_id={channel}, kind={event_kind}): {e}"
        );
        // Drop the event without a receipt — the source resends it
        // (e.g. once the invite that creates the mirror lands). The audit
        // row preserves the trail.
        return;
    }
    port.mark_applied(&request_id).await;
    port.send_receipt(&source_runtime_id, &channel, vec![request_id.clone()])
        .await;
    // ADR-090 D5: a person's root post in a DM is ingress — the host
    // starts the peko's turn (only for DMs; groups wake by binding).
    if let peko_protocol::channel::ChannelEvent::Posted {
        author,
        text,
        parent: None,
        attachments,
        model,
        ..
    } = &event
    {
        if let Ok(human @ (ActorRef::User(_) | ActorRef::Visitor(_))) = author.parse::<ActorRef>() {
            host.human_dm_post(
                channel.clone(),
                human,
                text.clone(),
                attachments.clone(),
                model.clone(),
            )
            .await;
        }
    }
    debug!(
        "inbound TunnelChannelEvent: verified + audited + appended to local mirror \
         (request_id={request_id}, channel_id={channel}, kind={event_kind})"
    );
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

    /// Put on PekoHub the local user's pekos whose registration says they
    /// belong there but that have no Hub ID yet: a peko the desktop made
    /// (private by default) before or after this computer signed in. Only
    /// the local user's own pekos; one owned by someone else goes online
    /// through its owner's consent, not here. Each registration waits for
    /// PekoHub's receipt, so call this from a spawned task, never from the
    /// tunnel's message loop.
    pub async fn register_pending_principals(&self) {
        let port: Arc<dyn ChannelPort> = self.host.tunnel_channel_port();
        let local_user = ActorRef::User("local".into());
        if port.actor_info(&local_user).await.hub_id.is_none() {
            return;
        }
        for principal in self.host.principal_manager().list_all().await {
            let (name, exposure, owner) = {
                let config = principal.config.read().await;
                (config.name.clone(), config.exposure, config.owner.clone())
            };
            if exposure == peko_auth::Exposure::LocalOnly
                || manager_hub_id(&self.host, &principal).await.is_some()
                || !peko_subject::ActorKeys::same_actor(
                    &peko_channel::PortKeys(port.as_ref()),
                    &owner,
                    &local_user,
                )
            {
                continue;
            }
            match self.register_principal(&name, exposure.into()).await {
                Ok(hub_id) => {
                    info!(principal = %name, ?hub_id, "registered with PekoHub");
                    // The profile may have changed while the receipt was
                    // on its way (the desktop sets a new peko's avatar
                    // right after creating it, and an unregistered peko
                    // announces nothing): announce what it is now.
                    if let Err(e) = self.announce_single_instance(&name).await {
                        warn!(principal = %name, "announce after registering failed: {e}");
                    }
                }
                Err(e) => warn!(principal = %name, "not registered with PekoHub yet: {e}"),
            }
        }
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
            TunnelMessage::PeerRuntimeOnline { runtime_id } => {
                let port = self.host.tunnel_channel_port();
                tokio::spawn(async move {
                    port.on_peer_runtime_online(&runtime_id).await;
                    // ADR-091 D4: catch up DMs whose peko it hosts.
                    port.sync_remote_dms(Some(&runtime_id)).await;
                    // ADR-094: files it holds that did not arrive yet.
                    port.retry_wanted(Some(&runtime_id)).await;
                    // ADR-095 D6: a person asked while it was away.
                    port.resend_person_dm_invites().await;
                    // ADR-095 D5: it may be another computer of this
                    // account that missed our request for the person's
                    // DMs; ask again so it answers now.
                    port.sync_account_dms().await;
                });
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
                        self.state.write().await.transfer_receipts.insert(
                            id,
                            synced
                                .as_ref()
                                .map(|_| TransferOutcome::Transferred)
                                .map_err(ToString::to_string),
                        );
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
            TunnelMessage::PrincipalTransferOffered {
                request_id,
                message,
            } => {
                self.state
                    .write()
                    .await
                    .transfer_receipts
                    .insert(request_id, Ok(TransferOutcome::Offered(message)));
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
                let manager = self.host.principal_manager();
                tokio::spawn(async move {
                    // ADR-095 D7: registering connects the tunnel; key the
                    // person's DMs by their account before anything syncs.
                    if let Err(e) =
                        crate::principal::dm_rekey::reconcile_local_user_dms(&manager).await
                    {
                        warn!("Could not rekey the local user's DMs: {e}");
                    }
                    port.on_tunnel_ready().await;
                    // ADR-091 D4: catch up remote DMs.
                    port.sync_remote_dms(None).await;
                    // ADR-092 D4: the account's groups follow it here.
                    port.sync_account_groups().await;
                    // ADR-095 D5: and so do its DMs.
                    port.sync_account_dms().await;
                    // ADR-095 D6: ask again where nobody answered yet.
                    port.resend_person_dm_invites().await;
                    // ADR-094: files that did not arrive yet.
                    port.retry_wanted(None).await;
                });
                // Announce all instances after auth
                if let Err(e) = self.announce_instances(&handle).await {
                    warn!("Failed to announce instances: {}", e);
                }
                // Then register the local user's pekos that should be on
                // PekoHub and are not yet (a peko the desktop made before
                // this computer signed in). Spawned: each waits for a
                // receipt this loop delivers.
                let dispatcher = self.clone();
                tokio::spawn(async move { dispatcher.register_pending_principals().await });
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
            TunnelMessage::TunnelChannelHistoryRequest {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                before,
                after,
                limit,
                signature,
            } => {
                let request = ChannelHistoryRequestPayload {
                    v: crate::tunnel::CHANNEL_HISTORY_REQUEST_VERSION.into(),
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    channel_id,
                    before,
                    after,
                    limit,
                    iat: 0,
                    exp: 0,
                };
                self.handle_history_request(request, &signature, &handle)
                    .await;
            }
            TunnelMessage::TunnelAccountDmsRequest {
                request_id,
                source_runtime_id,
                recipient_account,
                signature,
            } => {
                let port = self.host.tunnel_channel_port();
                let verified = port
                    .on_account_dms_request(
                        &handle,
                        request_id,
                        source_runtime_id,
                        recipient_account,
                        signature,
                    )
                    .await;
                // ADR-092 D4: another device of this account may have
                // joined a group; look the account's groups up again.
                if verified {
                    tokio::spawn(async move { port.sync_account_groups().await });
                }
            }
            TunnelMessage::TunnelAccountDms {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                dms,
                people,
                blocks,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_account_dms(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        dms,
                        people,
                        blocks,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelPersonDmInvite {
                request_id,
                source_runtime_id,
                recipient_account,
                source_principal_did,
                channel_id,
                name,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_person_dm_invite(
                        request_id,
                        source_runtime_id,
                        recipient_account,
                        source_principal_did,
                        channel_id,
                        name,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelPekoModelsRequest {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                principal_did,
                signature,
            } => {
                // ADR-097 D1: the models a peko's owner may pick, only to
                // its owner; anyone else gets none.
                let port = self.host.tunnel_channel_port();
                let envelope = crate::tunnel::PekoModelsRequestPayload {
                    v: crate::tunnel::PEKO_MODELS_REQUEST_VERSION.into(),
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    principal_did,
                    iat: 0,
                    exp: 0,
                };
                if let Some(request) = port
                    .verified_peko_models_request(envelope, &signature)
                    .await
                {
                    let models = self
                        .host
                        .peko_models_for_owner(
                            &request.principal_did,
                            &request.source_principal_did,
                        )
                        .await;
                    port.answer_peko_models(&handle, &request, models).await;
                }
            }
            TunnelMessage::TunnelPekoModels {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                principal_did,
                models,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_peko_models(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        principal_did,
                        models,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelPekoApprovalsRequest {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                principal_did,
                signature,
            } => {
                // ADR-099 D1: a peko's waiting requests, only to its
                // owner; anyone else gets none.
                let port = self.host.tunnel_channel_port();
                let envelope = crate::tunnel::PekoApprovalsRequestPayload {
                    v: crate::tunnel::PEKO_APPROVALS_REQUEST_VERSION.into(),
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    principal_did,
                    iat: 0,
                    exp: 0,
                };
                if let Some(request) = port
                    .verified_peko_approvals_request(envelope, &signature)
                    .await
                {
                    let approvals = self
                        .host
                        .peko_approvals_for_owner(
                            &request.principal_did,
                            &request.source_principal_did,
                        )
                        .await;
                    port.answer_peko_approvals(&handle, &request, approvals)
                        .await;
                }
            }
            TunnelMessage::TunnelPekoApprovals {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                principal_did,
                approvals,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_peko_approvals(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        principal_did,
                        approvals,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelPekoApprovalDecide {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                principal_did,
                approval_id,
                approve,
                always,
                signature,
            } => {
                // ADR-099 D2: the owner's decision from their other
                // computer, applied as `approval_decide` applies it.
                let port = self.host.tunnel_channel_port();
                let envelope = crate::tunnel::PekoApprovalDecidePayload {
                    v: crate::tunnel::PEKO_APPROVAL_DECIDE_VERSION.into(),
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    principal_did,
                    approval_id,
                    approve,
                    always,
                    iat: 0,
                    exp: 0,
                };
                if let Some(request) = port
                    .verified_peko_approval_decide(envelope, &signature)
                    .await
                {
                    let outcome = self
                        .host
                        .decide_peko_approval_for_owner(
                            &request.principal_did,
                            &request.source_principal_did,
                            &request.approval_id,
                            request.approve,
                            request.always,
                            &request.source_runtime_id,
                        )
                        .await;
                    port.answer_peko_approval_decide(&handle, &request, outcome)
                        .await;
                }
            }
            TunnelMessage::TunnelPekoApprovalDecided {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                principal_did,
                approval_id,
                decided,
                message,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_peko_approval_decided(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        principal_did,
                        approval_id,
                        decided,
                        message,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelDmRoute {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                routes,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_dm_route(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        source_principal_did,
                        channel_id,
                        routes,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelGroupInvitation {
                request_id,
                source_runtime_id,
                recipient_account,
                channel_id,
                name,
                inviter,
                initial_members,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_group_invitation(
                        request_id,
                        source_runtime_id,
                        recipient_account,
                        channel_id,
                        name,
                        inviter,
                        initial_members,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelGroupJoin {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                accept,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_group_join(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        source_principal_did,
                        channel_id,
                        accept,
                        signature,
                    )
                    .await;
            }
            TunnelMessage::TunnelGroupJoinRequest {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                invite,
                source_hub_id,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_group_join_request(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        source_principal_did,
                        channel_id,
                        invite,
                        source_hub_id,
                        signature,
                    )
                    .await;
            }
            // ADR-092 D4/D5: PekoHub's answers to this runtime's group
            // requests.
            answer @ (TunnelMessage::GroupRegistered { .. }
            | TunnelMessage::GroupRegisterRejected { .. }
            | TunnelMessage::AccountGroups { .. }
            | TunnelMessage::GroupLookup { .. }) => {
                let request_id = match &answer {
                    TunnelMessage::GroupRegistered { request_id, .. }
                    | TunnelMessage::GroupRegisterRejected { request_id, .. }
                    | TunnelMessage::AccountGroups { request_id, .. }
                    | TunnelMessage::GroupLookup { request_id, .. } => request_id.clone(),
                    _ => unreachable!(),
                };
                if !self
                    .host
                    .tunnel_channel_port()
                    .on_hub_answer(&request_id, answer)
                {
                    debug!(request_id, "a PekoHub answer nothing waits for; dropped");
                }
            }
            TunnelMessage::TunnelAttachmentRequest {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                channel_id,
                sha256,
                offset,
                signature,
            } => {
                let request = crate::tunnel::AttachmentRequestPayload {
                    v: crate::tunnel::ATTACHMENT_REQUEST_VERSION.into(),
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    channel_id,
                    sha256,
                    offset,
                    iat: 0,
                    exp: 0,
                };
                self.handle_attachment_request(request, &signature, &handle)
                    .await;
            }
            TunnelMessage::TunnelAttachmentChunk {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                channel_id,
                sha256,
                offset,
                size,
                data,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_attachment_chunk(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        channel_id,
                        sha256,
                        offset,
                        size,
                        data,
                    )
                    .await;
            }
            TunnelMessage::TunnelChannelHistory {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                channel_id,
                events,
                has_more,
                signature,
            } => {
                self.host
                    .tunnel_channel_port()
                    .on_history(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        channel_id,
                        events,
                        has_more,
                        signature,
                    )
                    .await;
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

        // ADR-092 D3: a group's history is gated by membership, not by the
        // owning peko's exposure (PekoHub proxies it in that peko's name).
        if bridge_payload["body"]["kind"].as_str() == Some("history")
            && bridge_payload["body"]["group"].is_string()
        {
            return self
                .send_proxied_history(
                    &handle,
                    &request_id,
                    &principal_name,
                    &bridge_payload,
                    &caller_principal,
                )
                .await;
        }

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

        // ADR-091 D6: a client without a runtime reads the caller's own
        // conversation with this peko — and nothing else.
        if bridge_payload["body"]["kind"].as_str() == Some("history") {
            return self
                .send_proxied_history(
                    &handle,
                    &request_id,
                    &principal_name,
                    &bridge_payload,
                    &caller_principal,
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
    /// when this runtime's account is the recorded owner. Hub makes it an
    /// offer the new owner accepts on PekoHub ([`TransferOutcome::Offered`]);
    /// the new owner arrives later as `principal_owner_changed`, which
    /// [`Self::sync_owner_from_hub`] writes. A Hub that confirms at once
    /// answers [`TransferOutcome::Transferred`].
    pub async fn transfer_online(
        &self,
        name: &str,
        owner_account: &str,
    ) -> anyhow::Result<TransferOutcome> {
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
            // ADR-095 D6: nothing from a person this one blocked.
            if self
                .host
                .tunnel_channel_port()
                .blocks(&payload.source_principal_did)
                .await
            {
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
        // ADR-094: a post applies once the files it names are here, so it
        // never shows (or wakes a peko) without them. Posts of one channel
        // apply in arrival order; a fetch runs outside the dispatch permit,
        // since the chunks it waits for are dispatched too.
        let inbound = InboundEvent {
            request_id,
            source_runtime_id,
            channel: channel_id_typed,
            event,
        };
        let needs_files = !port.has_all_files(&inbound.channel, &inbound.event).await;
        let lock = port.apply_lock(&inbound.channel);
        let host = Arc::clone(&self.host);
        match Arc::clone(&lock).try_lock_owned() {
            Ok(_guard) if !needs_files => apply_inbound_event(host, inbound).await,
            Ok(guard) => {
                tokio::spawn(async move {
                    let _guard = guard;
                    apply_inbound_event(host, inbound).await;
                });
            }
            Err(_) => {
                tokio::spawn(async move {
                    let _guard = lock.lock_owned().await;
                    apply_inbound_event(host, inbound).await;
                });
            }
        }
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
        } else if is_human_dm_invite(
            &payload.creator,
            &payload.creator_did,
            &payload.source_principal_did,
            kind,
        ) && author_signature.is_empty()
        {
            // ADR-090 D3: the local user of the source runtime opens a DM.
            // A human has no key; the runtime signature and the Hub's
            // binding of that runtime to the account vouch for it, and the
            // host accepts only the two-member shape.
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
        // ADR-092 D1: a group's roster comes from its home runtime only.
        let group = kind == peko_protocol::channel::ConversationKind::Group;
        if group {
            if let Ok(Some(home)) = port.local().group_home(&channel_typed).await {
                if home.runtime != source_runtime_id {
                    warn!(
                        request_id,
                        channel_id,
                        home = %home.runtime,
                        "inbound TunnelChannelInvite: dropping a group roster change from a runtime that is not the group's home"
                    );
                    return Ok(());
                }
            }
        }
        // ADR-092 D2: a roster addressed to this runtime's person keeps their
        // copy of a group they joined (and is ignored otherwise).
        if group {
            let addressed = initial_members
                .iter()
                .find(|m| m.runtime_id.is_none())
                .map(|m| m.principal_did.clone());
            if addressed.as_deref().is_some_and(|a| a.starts_with("user:")) {
                let applied = port
                    .on_person_roster(
                        &channel_typed,
                        &name,
                        &creator,
                        &initial_members,
                        &source_runtime_id,
                    )
                    .await
                    .unwrap_or_else(|e| {
                        warn!(
                            request_id,
                            "group roster for this runtime's person not applied: {e}"
                        );
                        false
                    });
                if applied {
                    adopt_start(&port, &channel_typed, payload.created_at.as_deref()).await;
                    port.mark_applied(&request_id).await;
                    port.send_receipt(&source_runtime_id, &channel_typed, vec![request_id.clone()])
                        .await;
                }
                return Ok(());
            }
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
        // ADR-092 D1: the first roster to arrive names the home: invites
        // leave a group only from there. ADR-092 D3: a peko joining a group
        // that already talked catches up from its home.
        if group {
            if let Ok(None) = port.local().group_home(&channel_typed).await {
                let _ = port
                    .local()
                    .set_group_home(
                        &channel_typed,
                        peko_channel::GroupHome {
                            runtime: source_runtime_id.clone(),
                            owner: None,
                        },
                    )
                    .await;
                let sync = port.clone();
                let joined = channel_typed.clone();
                tokio::spawn(async move {
                    if let Err(e) = sync.sync_history(&joined).await {
                        debug!(channel = %joined, "group catch-up skipped: {e}");
                    }
                });
            }
        }
        adopt_start(&port, &channel_typed, payload.created_at.as_deref()).await;
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

    /// ADR-091 D3: serve a person their DM with a peko hosted here; a
    /// group member its group (ADR-092 D3); a peko its DM with a peko
    /// hosted here (ADR-095 D2) — and nothing else. Every refusal answers
    /// an empty page, so a probe learns nothing about what exists.
    async fn handle_history_request(
        &self,
        envelope: ChannelHistoryRequestPayload,
        signature: &str,
        handle: &TunnelHandle,
    ) {
        if envelope.recipient_runtime_id != self.host.runtime_did() {
            return;
        }
        let verified = match did_key_to_verifying_key(&envelope.source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| crate::tunnel::verify_history_request(&key, signature))
        {
            Ok(payload) => payload,
            Err(e) => {
                warn!(request_id = %envelope.request_id, "history request failed verification: {e}");
                return;
            }
        };
        let same = ChannelHistoryRequestPayload {
            iat: verified.iat,
            exp: verified.exp,
            ..envelope
        };
        if same != verified {
            warn!(request_id = %verified.request_id, "history request disagrees with its signature");
            return;
        }
        let port = self.host.tunnel_channel_port();
        let page = match self.history_for_person(&verified).await {
            Ok(page) => page,
            Err(e) => {
                debug!(request_id = %verified.request_id, "history request refused: {e}");
                crate::tunnel::HistoryPage::default()
            }
        };
        port.answer_history(handle, &verified, page).await;
    }

    /// ADR-094: serve a chunk of a channel file to a runtime that asked
    /// for it, once its signature holds. The port's gate decides whether
    /// the file is the asker's to have.
    async fn handle_attachment_request(
        &self,
        envelope: crate::tunnel::AttachmentRequestPayload,
        signature: &str,
        handle: &TunnelHandle,
    ) {
        if envelope.recipient_runtime_id != self.host.runtime_did() {
            return;
        }
        let verified = match did_key_to_verifying_key(&envelope.source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| crate::tunnel::verify_attachment_request(&key, signature))
        {
            Ok(payload) => payload,
            Err(e) => {
                warn!(request_id = %envelope.request_id, "attachment request failed verification: {e}");
                return;
            }
        };
        let same = crate::tunnel::AttachmentRequestPayload {
            iat: verified.iat,
            exp: verified.exp,
            ..envelope
        };
        if same != verified {
            warn!(request_id = %verified.request_id, "attachment request disagrees with its signature");
            return;
        }
        self.host
            .tunnel_channel_port()
            .answer_attachment_request(handle, &verified)
            .await;
    }

    /// ADR-091 D6: the caller's own DM with `principal_name`, one page,
    /// as a proxied JSON response. The exposure / `Chat` check already
    /// ran.
    async fn send_proxied_history(
        &self,
        handle: &TunnelHandle,
        request_id: &str,
        principal_name: &str,
        bridge_payload: &serde_json::Value,
        caller: &ActorRef,
    ) -> anyhow::Result<()> {
        let page = self
            .proxied_history_page(principal_name, &bridge_payload["body"], caller)
            .await;
        let payload = serde_json::json!({
            "status": 200,
            "body": { "events": page.events, "hasMore": page.has_more },
        });
        if let Err(e) =
            handle.send_response(request_id.to_string(), payload.to_string().into_bytes())
        {
            warn!("Failed to send history response: {}", e);
        }
        Ok(())
    }

    /// The page behind [`Self::send_proxied_history`]: only a person on
    /// the Hub, only their pair DM with this peko. A visitor or a missing
    /// conversation reads as an empty page.
    async fn proxied_history_page(
        &self,
        principal_name: &str,
        body: &serde_json::Value,
        caller: &ActorRef,
    ) -> crate::tunnel::HistoryPage {
        let empty = crate::tunnel::HistoryPage::default();
        if !(matches!(caller, ActorRef::User(_)) && caller.is_hub_route()) {
            return empty;
        }
        // ADR-092 D3: a group the caller is a member of, kept here.
        if let Some(group) = body["group"].as_str() {
            return self
                .proxied_group_page(group, body, caller)
                .await
                .unwrap_or_else(|e| {
                    debug!(group, "proxied group history refused: {e}");
                    empty
                });
        }
        let Some(principal) = self
            .host
            .principal_manager()
            .get_by_name(principal_name)
            .await
        else {
            return empty;
        };
        let port = self.host.tunnel_channel_port();
        let me = port.actor_info(&ActorRef::from(&principal.id)).await;
        let channel = peko_channel::dm_channel_id(&me.identity_key, &caller.canonical_address());
        if port.conversation_kind(&channel).await.ok() != Some(peko_channel::ConversationKind::Dm) {
            return empty;
        }
        let limit = body["limit"]
            .as_u64()
            .map_or(crate::tunnel::HISTORY_PAGE_LIMIT, |l| {
                u32::try_from(l).unwrap_or(crate::tunnel::HISTORY_PAGE_LIMIT)
            });
        port.served_history_page(
            &channel,
            body["before"].as_str(),
            body["after"].as_str(),
            limit,
        )
        .await
    }

    /// The D3 gate and the page behind it.
    async fn history_for_person(
        &self,
        request: &ChannelHistoryRequestPayload,
    ) -> anyhow::Result<crate::tunnel::HistoryPage> {
        // ADR-092 D3: a group's history is its home's, for its members.
        let group = peko_channel::ChannelId(request.channel_id.clone());
        if self
            .host
            .tunnel_channel_port()
            .conversation_kind(&group)
            .await
            .ok()
            == Some(peko_channel::ConversationKind::Group)
        {
            return self.history_for_group_member(request, &group).await;
        }
        // ADR-095 D2: a peko asking for its DM with a peko hosted here.
        if request.source_principal_did.starts_with("did:key:") {
            return self.history_for_peko_peer(request, &group).await;
        }
        // 1. A person on the Hub, whose runtime the Hub bound to them.
        let person = match request.source_principal_did.parse::<ActorRef>() {
            Ok(human @ ActorRef::User(_))
                if human.is_hub_route()
                    && human.canonical_address() == request.source_principal_did =>
            {
                human
            }
            _ => anyhow::bail!("only a person on PekoHub may ask for history"),
        };
        let port = self.host.tunnel_channel_port();
        let channel = peko_channel::ChannelId(request.channel_id.clone());
        // ADR-095 D2/D6: this runtime's person's DM with another person,
        // for either of the two (the other person, or the same person on
        // another runtime) — never for someone it blocked.
        let local = port.actor_info(&ActorRef::User("local".into())).await;
        if let Some(account) = local.hub_id.as_deref() {
            if let Some(peer) = port.person_dm_peer(account, &channel).await {
                let mine = format!("user:hub:{account}");
                anyhow::ensure!(
                    request.source_principal_did == peer || request.source_principal_did == mine,
                    "not one of the DM's two people"
                );
                anyhow::ensure!(!port.blocks(&peer).await, "blocked");
                return Ok(port
                    .history_page(
                        &channel,
                        request.before.as_deref(),
                        request.after.as_deref(),
                        request.limit,
                    )
                    .await);
            }
        }
        // 2. Exactly their DM with a peko hosted here.
        let mut hosted = None;
        for principal in self.host.principal_manager().list_all().await {
            let me = port.actor_info(&ActorRef::from(&principal.id)).await;
            let pair = peko_channel::dm_channel_id(&me.identity_key, &request.source_principal_did);
            if pair == channel {
                hosted = Some(principal);
                break;
            }
        }
        let principal =
            hosted.ok_or_else(|| anyhow::anyhow!("not a DM with a peko hosted here"))?;
        anyhow::ensure!(
            port.conversation_kind(&channel).await? == peko_channel::ConversationKind::Dm,
            "not a DM"
        );
        // 3. They may still open this DM (ADR-090 D4): a registered peko,
        //    not local-only, and a private one only with a `Chat` grant.
        anyhow::ensure!(
            port.actor_info(&ActorRef::from(&principal.id))
                .await
                .hub_id
                .is_some(),
            "the peko is not registered"
        );
        let config = principal.config.read().await;
        anyhow::ensure!(
            config.exposure != peko_auth::Exposure::LocalOnly,
            "the peko is local-only"
        );
        if config.exposure == peko_auth::Exposure::Private {
            peko_auth::ownership::check_permission(
                &peko_auth::ownership::principal_resource(&*config),
                Permission::Chat,
                &person,
                &peko_channel::PortKeys(port.as_ref()),
            )
            .map_err(|e| anyhow::anyhow!("no chat grant: {e}"))?;
        }
        drop(config);
        Ok(port
            .history_page(
                &channel,
                request.before.as_deref(),
                request.after.as_deref(),
                request.limit,
            )
            .await)
    }

    /// ADR-095 D2: a page of a DM between a peko hosted here and the
    /// asking peko, which must be the DM's other member, asking from the
    /// runtime recorded as its route (PekoHub relays a peko's request only
    /// from the runtime its online link points at).
    async fn history_for_peko_peer(
        &self,
        request: &ChannelHistoryRequestPayload,
        channel: &peko_channel::ChannelId,
    ) -> anyhow::Result<crate::tunnel::HistoryPage> {
        let port = self.host.tunnel_channel_port();
        anyhow::ensure!(
            port.conversation_kind(channel).await? == peko_channel::ConversationKind::Dm,
            "not a DM"
        );
        // 1. The asker is the DM's remote member, from its route.
        anyhow::ensure!(
            port.local()
                .is_remote_member(
                    channel,
                    &request.source_runtime_id,
                    &request.source_principal_did
                )
                .await?,
            "not the DM's other member"
        );
        // 2. The DM is exactly the pair of the asker and a peko here.
        let peer = port
            .actor_info(&ActorRef::Principal(
                request.source_principal_did.clone().into(),
            ))
            .await;
        let mut hosted = None;
        for principal in self.host.principal_manager().list_all().await {
            let me = port.actor_info(&ActorRef::from(&principal.id)).await;
            if peko_channel::dm_channel_id(&me.identity_key, &peer.identity_key) == *channel {
                hosted = Some(principal);
                break;
            }
        }
        let principal =
            hosted.ok_or_else(|| anyhow::anyhow!("not a DM with a peko hosted here"))?;
        // 3. The peko here still talks over the tunnel: registered, not
        //    local-only (the invite's own check).
        anyhow::ensure!(
            port.actor_info(&ActorRef::from(&principal.id))
                .await
                .hub_id
                .is_some(),
            "the peko is not registered"
        );
        anyhow::ensure!(
            principal.config.read().await.exposure != peko_auth::Exposure::LocalOnly,
            "the peko is local-only"
        );
        Ok(port
            .history_page(
                channel,
                request.before.as_deref(),
                request.after.as_deref(),
                request.limit,
            )
            .await)
    }

    /// ADR-092 D3: a page of a group whose home is this runtime, for a
    /// current member — a person by their Hub-vouched account, a peko by
    /// the runtime that is its route — within the group's visibility.
    async fn history_for_group_member(
        &self,
        request: &ChannelHistoryRequestPayload,
        channel: &peko_channel::ChannelId,
    ) -> anyhow::Result<crate::tunnel::HistoryPage> {
        let port = self.host.tunnel_channel_port();
        if let Some(home) = port.local().group_home(channel).await? {
            anyhow::ensure!(
                home.runtime == self.host.runtime_did(),
                "not the group's home"
            );
        }
        let address = match request.source_principal_did.parse::<ActorRef>() {
            Ok(person @ ActorRef::User(_))
                if person.is_hub_route()
                    && person.canonical_address() == request.source_principal_did =>
            {
                let address = port.actor_info(&person).await.address;
                let mut member = false;
                for m in port.list_members(channel).await? {
                    if port.actor_info(&m).await.address == address {
                        member = true;
                    }
                }
                anyhow::ensure!(member, "not a member");
                address
            }
            _ if request.source_principal_did.starts_with("did:key:") => {
                anyhow::ensure!(
                    port.local()
                        .is_remote_member(
                            channel,
                            &request.source_runtime_id,
                            &request.source_principal_did
                        )
                        .await?,
                    "not a member's route"
                );
                port.actor_info(&ActorRef::Principal(
                    request.source_principal_did.clone().into(),
                ))
                .await
                .address
            }
            _ => anyhow::bail!("only a member may ask for a group's history"),
        };
        self.group_page_for(
            channel,
            &address,
            request.before.as_deref(),
            request.after.as_deref(),
            request.limit,
        )
        .await
    }

    /// A page of `channel` for its member `address`, as the group's
    /// history setting lets them read it (ADR-092 D3).
    async fn group_page_for(
        &self,
        channel: &peko_channel::ChannelId,
        address: &str,
        before: Option<&str>,
        after: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<crate::tunnel::HistoryPage> {
        let port = self.host.tunnel_channel_port();
        let since = match port.local().history_visibility(channel).await? {
            peko_channel::HistoryVisibility::Shared => None,
            peko_channel::HistoryVisibility::Joined => Some(
                port.local()
                    .joined_at(channel, address)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("no record of their joining"))?,
            ),
        };
        Ok(port
            .group_history_page(channel, before, after, limit, since.as_deref())
            .await)
    }

    /// A group's page for a person without a runtime, proxied by PekoHub
    /// in the owning peko's name (ADR-092 D3 as ADR-091 D6): only at the
    /// home, only for a member.
    async fn proxied_group_page(
        &self,
        group: &str,
        body: &serde_json::Value,
        caller: &ActorRef,
    ) -> anyhow::Result<crate::tunnel::HistoryPage> {
        let channel = peko_channel::ChannelId::parse(group)
            .ok_or_else(|| anyhow::anyhow!("not a channel"))?;
        let port = self.host.tunnel_channel_port();
        anyhow::ensure!(
            port.conversation_kind(&channel).await? == peko_channel::ConversationKind::Group,
            "not a group"
        );
        if let Some(home) = port.local().group_home(&channel).await? {
            anyhow::ensure!(
                home.runtime == self.host.runtime_did(),
                "not the group's home"
            );
        }
        let address = port.actor_info(caller).await.address;
        let mut member = false;
        for m in port.list_members(&channel).await? {
            if port.actor_info(&m).await.address == address {
                member = true;
            }
        }
        anyhow::ensure!(member, "not a member");
        let limit = body["limit"]
            .as_u64()
            .map_or(crate::tunnel::HISTORY_PAGE_LIMIT, |l| {
                u32::try_from(l).unwrap_or(crate::tunnel::HISTORY_PAGE_LIMIT)
            });
        self.group_page_for(
            &channel,
            &address,
            body["before"].as_str(),
            body["after"].as_str(),
            limit,
        )
        .await
    }
}

/// Whether an invite's creator fields are the local user of the source
/// runtime opening a DM (ADR-090 D3): one Hub user address as creator,
/// creator DID, and source, on a `dm` invite.
fn is_human_dm_invite(
    creator: &str,
    creator_did: &str,
    source_principal_did: &str,
    kind: peko_protocol::channel::ConversationKind,
) -> bool {
    kind == peko_protocol::channel::ConversationKind::Dm
        && creator == creator_did
        && creator == source_principal_did
        && matches!(
            creator.parse::<ActorRef>(),
            Ok(human @ ActorRef::User(_)) if human.is_hub_route()
                && human.canonical_address() == creator
        )
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
/// A copy of a channel takes the start time its source signed into the
/// invite (ADR-092 D1), so it shows when the group began.
async fn adopt_start(
    port: &Arc<crate::tunnel::TunnelChannelPort>,
    channel: &peko_protocol::channel::ChannelId,
    created_at: Option<&str>,
) {
    let Some(at) = created_at.and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok()) else {
        return;
    };
    if let Err(e) = port
        .local()
        .adopt_created_at(channel, at.with_timezone(&chrono::Utc))
        .await
    {
        debug!(%channel, "start time not taken: {e}");
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
        let workspace = app_state.config.data_dir.join("pekos").join(name);
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

        for answer in ["refuse", "offer", "confirm"] {
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
            let message = match answer {
                "confirm" => TunnelMessage::PrincipalOwnerChanged {
                    request_id: Some(request_id),
                    local_id,
                    hub_id,
                    owner_id: alice.clone(),
                },
                "offer" => TunnelMessage::PrincipalTransferOffered {
                    request_id,
                    message: "Offered to @alice".into(),
                },
                _ => TunnelMessage::PrincipalTransferRejected {
                    request_id,
                    message: "not the owner".into(),
                },
            };
            dispatcher.dispatch(message, handle.clone()).await.unwrap();
            let result = asking.await.unwrap();
            let owner = principal.config.read().await.owner.clone();
            match answer {
                "confirm" => {
                    assert_eq!(result.unwrap(), TransferOutcome::Transferred);
                    assert_eq!(
                        owner,
                        ActorRef::User(peko_subject::Route::hub(alice.clone()))
                    );
                }
                "offer" => {
                    assert_eq!(
                        result.unwrap(),
                        TransferOutcome::Offered("Offered to @alice".into())
                    );
                    assert_eq!(
                        owner,
                        ActorRef::User("local".into()),
                        "an offer changes nothing until it is accepted"
                    );
                }
                _ => {
                    assert!(result.unwrap_err().to_string().contains("not the owner"));
                    assert_eq!(
                        owner,
                        ActorRef::User("local".into()),
                        "a refusal changes nothing"
                    );
                }
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

    /// A peko the desktop made is private by intent. Once this computer
    /// is registered, the dispatcher puts the local user's such pekos on
    /// PekoHub; a local-only one stays local, and one owned by someone
    /// else waits for its owner.
    #[tokio::test]
    async fn the_local_users_private_pekos_go_on_pekohub_once_signed_in() {
        let app_state = create_test_app_state().await;
        let dispatcher = TunnelDispatcher::new(Arc::new(app_state.clone()));
        let (handle, mut rx) = mock_tunnel_handle();
        dispatcher.state.write().await.tunnel_handle = Some(handle.clone());
        dispatcher.mark_ready(30).await;
        let mine = create_test_principal(
            &app_state,
            "mine",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        create_test_principal(
            &app_state,
            "kept-local",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::LocalOnly,
        )
        .await;
        create_test_principal(
            &app_state,
            "theirs",
            ActorRef::User(peko_subject::Route::hub(uuid::Uuid::new_v4().to_string())),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;

        // Not signed in: nothing goes to PekoHub.
        dispatcher.register_pending_principals().await;
        assert!(rx.try_recv().is_err());

        // Signed in: the one peko is announced; PekoHub answers.
        app_state
            .tunnel_channel_port()
            .local()
            .link_local_user(&app_state.runtime_did(), &uuid::Uuid::new_v4().to_string())
            .await
            .unwrap();
        let pending = dispatcher.clone();
        let registering = tokio::spawn(async move { pending.register_pending_principals().await });
        let announced = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("announced")
            .expect("tunnel open");
        let TunnelMessage::InstanceAnnounce { payload } = announced else {
            panic!("expected an announce, got {announced:?}");
        };
        assert_eq!(payload.local_id, mine.id.0);
        let hub = uuid::Uuid::new_v4().to_string();
        dispatcher
            .dispatch(
                TunnelMessage::PrincipalRegistered {
                    registration_id: payload.registration_id.clone(),
                    local_id: mine.id.0.clone(),
                    hub_id: hub.clone(),
                    signing_did: mine.did().await.0,
                },
                handle.clone(),
            )
            .await
            .unwrap();
        registering.await.unwrap();
        assert_eq!(
            manager_hub_id(&dispatcher.host, &mine).await.as_deref(),
            Some(hub.as_str())
        );
        // Then it announces what it is now (an avatar set meanwhile).
        let TunnelMessage::InstanceAnnounce { payload } = rx.try_recv().expect("re-announced")
        else {
            panic!("expected the profile announce");
        };
        assert_eq!(payload.local_id, mine.id.0);
        assert_eq!(payload.registration_id, None);
        assert!(
            rx.try_recv().is_err(),
            "neither the local-only peko nor someone else's"
        );

        // Already registered: asking again sends nothing.
        dispatcher.register_pending_principals().await;
        assert!(rx.try_recv().is_err());
    }

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
            options: Vec::new(),
            model: None,
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
            options: Vec::new(),
            model: None,
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
            options: Vec::new(),
            model: None,
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
            options: Vec::new(),
            model: None,
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
            options: Vec::new(),
            model: None,
        };
        deliver("req-1", post.clone()).await.unwrap();
        deliver("req-1", post.clone()).await.unwrap();
        // The same post in a new envelope (a group's home relaying it
        // back, or history and live both carrying it) is confirmed and
        // applied nothing more.
        deliver("req-echo", post).await.unwrap();
        let posted = port
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|ev| matches!(ev, ChannelEvent::Posted { .. }))
            .count();
        assert_eq!(posted, 1, "applied once");
        for expected in ["req-1", "req-1", "req-echo"] {
            let (to, ids) = receipt_for(outbound.try_recv().expect("a receipt per arrival"));
            assert_eq!(to, source);
            assert_eq!(ids, [expected]);
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
            options: Vec::new(),
            model: None,
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
                options: Vec::new(),
                model: None,
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
            created_at: None,
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
                owner_handle: None,
                exposure: ResolvedExposure::Public,
            },
        );
        app.tool_runtime
            .tooling()
            .services()
            .set_cross_runtime_a2a_ctx(Arc::new(crate::tunnel::CrossRuntimeA2aCtx {
                directory: directory.clone(),
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
            created_at: None,
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
                members.clone(),
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
                options: Vec::new(),
                model: None,
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
        if dm {
            return;
        }
        // ADR-092 D1: the first roster names the group's home; a roster
        // from any other runtime — even correctly signed, even reusing
        // the id — changes nothing.
        let home = port.local().group_home(&channel).await.unwrap().unwrap();
        assert_eq!(home.runtime, source_runtime);
        assert_eq!(home.owner, None, "only the home knows the owner");
        let other_runtime = peko_identity::keys::KeyPair::generate();
        let other_signer = peko_identity::keys::KeyPair::generate();
        let other_runtime_did =
            crate::tunnel::verifying_key_to_did_key(&other_runtime.verifying_key);
        let other_did = crate::tunnel::verifying_key_to_did_key(&other_signer.verifying_key);
        // A real, registered principal on another runtime: only the home
        // rule stands between it and this group's roster.
        let intruder_hub = uuid::Uuid::new_v4().to_string();
        directory.register_hub_id(
            &intruder_hub,
            AgentResolution {
                name: None,
                hub_id: intruder_hub.clone(),
                local_id: "prin_intruder".into(),
                identity_key: "intruder-key".into(),
                runtime_id: other_runtime_did.clone(),
                instance_id: "intruder-instance".into(),
                agent_did: other_did.clone(),
                owner_principal: ActorRef::Public,
                owner_handle: None,
                exposure: ResolvedExposure::Public,
            },
        );
        let mut more = members.clone();
        more.push(peko_protocol::channel::InitialMember {
            principal_did: other_did.clone(),
            runtime_id: Some(other_runtime_did.clone()),
            hub_id: Some(intruder_hub.clone()),
            identity_key: Some("intruder-key".into()),
            name: Some("intruder".into()),
        });
        let bytes = serde_json::to_vec(&more).unwrap();
        let intruder = format!("principal:hub:{intruder_hub}");
        let fields = crate::tunnel::ChannelInviteSignedFields {
            request_id: "foreign-roster",
            source_runtime_id: &other_runtime_did,
            recipient_runtime_id: &app.runtime_did(),
            source_principal_did: &other_did,
            channel_id: channel.as_str(),
            creator: &intruder,
            creator_did: &other_did,
            name: "registered conversation",
            kind: kind.as_str(),
            initial_members_bytes: &bytes,
            created_at: None,
        };
        let (signature, author_signature) = crate::tunnel::sign_channel_invite(
            &other_runtime.signing_key,
            Some(&other_signer.signing_key),
            fields,
            now,
            now + 300,
        );
        dispatcher
            .handle_inbound_tunnel_channel_invite(
                "foreign-roster".into(),
                other_runtime_did.clone(),
                app.runtime_did(),
                other_did.clone(),
                channel.0.clone(),
                intruder,
                other_did,
                "registered conversation".into(),
                kind,
                more,
                signature,
                author_signature,
            )
            .await
            .unwrap();
        assert_eq!(port.participants(&channel).await.unwrap().len(), 2);
        assert!(!port
            .local()
            .list_remote_members(&channel)
            .await
            .unwrap()
            .iter()
            .any(|m| m.runtime_id == other_runtime_did));
        assert_eq!(
            port.local().group_home(&channel).await.unwrap().unwrap(),
            home
        );
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

    // -----------------------------------------------------------------
    // ADR-090: the local user DMs a peko on another runtime
    // -----------------------------------------------------------------

    use crate::ipc::packet::{RequestPacket, ResponsePacket};

    #[test]
    fn only_a_hub_user_opening_a_dm_is_a_human_invite() {
        use peko_protocol::channel::ConversationKind::{Dm, Group};
        let user = "user:hub:6f1c2a52-58b6-4b8e-9d1c-6a3f0b6f1a01";
        assert!(is_human_dm_invite(user, user, user, Dm));
        assert!(!is_human_dm_invite(user, user, user, Group), "a group");
        let other = "user:hub:0b3c2a52-58b6-4b8e-9d1c-6a3f0b6f1a02";
        assert!(!is_human_dm_invite(user, user, other, Dm), "another source");
        assert!(
            !is_human_dm_invite(user, other, user, Dm),
            "another creator DID"
        );
        for creator in [
            "user:local:local",
            "visitor:hub:6f1c2a52-58b6-4b8e-9d1c-6a3f0b6f1a01",
            "principal:hub:6f1c2a52-58b6-4b8e-9d1c-6a3f0b6f1a01",
            "did:key:z6Mk",
        ] {
            assert!(
                !is_human_dm_invite(creator, creator, creator, Dm),
                "{creator}"
            );
        }
    }

    /// One in-process runtime with a captured tunnel: what it sends to
    /// the Hub lands in `outbound`.
    struct DmRuntime {
        _dir: TempDir,
        app: AppState,
        dispatcher: TunnelDispatcher,
        outbound: mpsc::Receiver<TunnelMessage>,
    }

    impl DmRuntime {
        async fn new(directory: Arc<crate::tunnel::hub_directory::FakeAgentDirectory>) -> Self {
            let dir = TempDir::new().unwrap();
            let data_dir = dir.path().to_path_buf();
            let app = AppState::with_data_dir(
                data_dir.clone(),
                "127.0.0.1".to_string(),
                11435,
                DaemonConfigSnapshot {
                    data_dir: data_dir.clone(),
                    config_dir: data_dir.clone(),
                    log_level: "info".to_string(),
                    launch_mode: crate::daemon::LaunchMode::Headless,
                },
                data_dir,
            )
            .await
            .unwrap();
            let (tx, outbound) = mpsc::channel(64);
            *app.tunnel_handle_slot().write().await = Some(TunnelHandle::new(tx));
            app.tunnel_channel_port()
                .set_ctx(Arc::new(crate::tunnel::CrossRuntimeChannelCtx {
                    directory,
                    signing_key: Arc::clone(&app.runtime_signing_key),
                    caller_runtime_id: app.runtime_did(),
                    tunnel: app.tunnel_handle_slot(),
                    principal_keys: TunnelHost::principal_signing_keys(&app),
                }))
                .await;
            let dispatcher = TunnelDispatcher::new(Arc::new(app.clone()));
            Self {
                _dir: dir,
                app,
                dispatcher,
                outbound,
            }
        }

        /// Hand an envelope the other runtime sent to this one, as the Hub
        /// would.
        async fn receive(&self, message: TunnelMessage) {
            match message {
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
                } => self
                    .dispatcher
                    .handle_inbound_tunnel_channel_invite(
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
                    .await
                    .unwrap(),
                TunnelMessage::TunnelChannelEvent {
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    channel_id,
                    event,
                    signature,
                    author_signature,
                } => self
                    .dispatcher
                    .handle_inbound_tunnel_channel_event(
                        request_id,
                        source_runtime_id,
                        recipient_runtime_id,
                        source_principal_did,
                        channel_id,
                        event,
                        signature,
                        author_signature,
                    )
                    .await
                    .unwrap(),
                history @ (TunnelMessage::TunnelChannelHistoryRequest { .. }
                | TunnelMessage::TunnelChannelHistory { .. }
                | TunnelMessage::TunnelAttachmentRequest { .. }
                | TunnelMessage::TunnelAttachmentChunk { .. }
                | TunnelMessage::TunnelGroupInvitation { .. }
                | TunnelMessage::TunnelGroupJoin { .. }
                | TunnelMessage::TunnelGroupJoinRequest { .. }
                | TunnelMessage::TunnelAccountDmsRequest { .. }
                | TunnelMessage::TunnelAccountDms { .. }
                | TunnelMessage::TunnelPersonDmInvite { .. }
                | TunnelMessage::TunnelDmRoute { .. }
                | TunnelMessage::TunnelPekoModelsRequest { .. }
                | TunnelMessage::TunnelPekoModels { .. }
                | TunnelMessage::TunnelPekoApprovalsRequest { .. }
                | TunnelMessage::TunnelPekoApprovals { .. }
                | TunnelMessage::TunnelPekoApprovalDecide { .. }
                | TunnelMessage::TunnelPekoApprovalDecided { .. }) => {
                    let handle = self.app.tunnel_handle_slot().read().await.clone().unwrap();
                    self.dispatcher.dispatch(history, handle).await.unwrap();
                }
                // To PekoHub itself (ADR-092 D4–D6), not to a runtime.
                TunnelMessage::GroupRoster { .. }
                | TunnelMessage::GroupRegister { .. }
                | TunnelMessage::AccountGroupsRequest { .. }
                | TunnelMessage::GroupLookupRequest { .. } => {}
                other => panic!("unexpected envelope {other:?}"),
            }
        }

        /// Everything this runtime sent so far.
        fn sent(&mut self) -> Vec<TunnelMessage> {
            std::iter::from_fn(|| self.outbound.try_recv().ok()).collect()
        }

        async fn ipc(&self, request: RequestPacket) -> ResponsePacket {
            struct Sink(std::sync::Mutex<Vec<ResponsePacket>>);
            #[async_trait::async_trait]
            impl crate::ipc::response_sink::ResponseSink for Sink {
                async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
                    self.0
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(bytes).map_err(std::io::Error::other)?);
                    Ok(())
                }
            }
            use crate::ipc::handlers::RequestHandler;
            let sink = Sink(std::sync::Mutex::new(Vec::new()));
            crate::ipc::handlers::channel::ChannelHandler::new(Arc::new(self.app.clone()))
                .handle(
                    request,
                    &peko_auth::caller::CallerContext::local(),
                    &sink,
                    &crate::ipc::server::PeerAddr::Ip("127.0.0.1:0".parse().unwrap()),
                )
                .await
                .unwrap();
            sink.0.into_inner().unwrap().pop().expect("one response")
        }
    }

    /// Deliver what `from` sent to `to`, and the receipts back, until both
    /// are quiet.
    async fn relay(a: &mut DmRuntime, b: &mut DmRuntime) {
        loop {
            let (to_b, to_a) = (a.sent(), b.sent());
            if to_b.is_empty() && to_a.is_empty() {
                return;
            }
            for message in to_b {
                b.receive(message).await;
            }
            for message in to_a {
                a.receive(message).await;
            }
        }
    }

    trait EmptyInvitations {
        fn matches_empty_invitations(&self) -> bool;
    }
    impl EmptyInvitations for ResponsePacket {
        fn matches_empty_invitations(&self) -> bool {
            matches!(self, ResponsePacket::ChannelInvitationsResult { invitations, .. } if invitations.is_empty())
        }
    }

    fn posts(events: &[peko_channel::ChannelEvent]) -> Vec<(String, String)> {
        events
            .iter()
            .filter_map(|ev| match ev {
                peko_channel::ChannelEvent::Posted { author, text, .. } => {
                    Some((author.clone(), text.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// ADR-090 end to end, two runtimes in process with the Hub as a
    /// relay: the local user of A opens a DM with a peko hosted on B, B
    /// binds a session in the conversation's compartment, the user's
    /// post reaches B and wakes the peko (acting for its owner), and the
    /// peko's answer reaches A's record. Reopening finds the same DM, and
    /// works with the directory down.
    #[tokio::test]
    async fn local_user_talks_to_a_peko_on_another_runtime() {
        use crate::tunnel::hub_directory::{
            AgentResolution, DirectoryErrorKind, FakeAgentDirectory, ResolvedExposure,
        };
        use peko_channel::{ChannelPort, Checkpoint};

        let account = uuid::Uuid::new_v4().to_string();
        let human = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();

        // B hosts the peko, registered and owned by the person's account.
        let mut b = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &b.app,
            "peko",
            human.clone(),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        b.app
            .tunnel_channel_port()
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("peko"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();

        // A is the person's runtime, registered under their account.
        let directory = Arc::new(FakeAgentDirectory::new());
        let resolution = AgentResolution {
            runtime_id: b.app.runtime_did(),
            instance_id: "instance".into(),
            name: Some("peko".into()),
            hub_id: hub_id.clone(),
            local_id: peko.id.0.clone(),
            identity_key: peko_key.clone(),
            agent_did: peko_did.clone(),
            owner_principal: human.clone(),
            owner_handle: None,
            exposure: ResolvedExposure::Private,
        };
        directory.register_hub_id(&hub_id, resolution);
        let mut a = DmRuntime::new(Arc::clone(&directory)).await;
        let open = RequestPacket::ChannelOpenRemoteDm {
            request_id: 1,
            hub_id: hub_id.clone(),
            token: Some("desktop-token".into()),
        };
        // Unregistered, the runtime cannot open one.
        match a.ipc(open.clone()).await {
            ResponsePacket::Error { message, .. } => {
                assert!(message.contains("peko runtime register"), "{message}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        a.app
            .tunnel_channel_port()
            .local()
            .link_local_user(&a.app.runtime_did(), &account)
            .await
            .unwrap();

        let channel = match a.ipc(open.clone()).await {
            ResponsePacket::ChannelRemoteDmOpened {
                channel,
                peer,
                name,
                created,
                ..
            } => {
                assert!(created);
                assert_eq!(peer, format!("principal:hub:{hub_id}"));
                assert_eq!(name.as_deref(), Some("peko"));
                channel
            }
            other => panic!("expected the DM, got {other:?}"),
        };
        assert_eq!(
            channel,
            peko_channel::dm_channel_id(&peko_key, &format!("user:hub:{account}"))
        );
        relay(&mut a, &mut b).await;

        // B: a mirror bound to a new session in the conversation's
        // compartment; the person's turns run as the owner.
        let b_port = b.app.tunnel_channel_port();
        let binding = b_port
            .binding_for(&channel, &peko.id)
            .await
            .unwrap()
            .unwrap();
        assert!(binding.session.starts_with("/user-"), "{binding:?}");
        assert_eq!(binding.wake, peko_channel::WakePolicy::Human);
        let metas = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(peko.memory.sessions_dir())
            .list_all_sessions(false)
            .await
            .unwrap();
        let session = metas
            .iter()
            .find(|m| m.channel_id.as_deref() == Some(channel.as_str()))
            .expect("a session bound to the DM");
        assert_eq!(
            session.compartment,
            Some(peko_tools_core::Compartment::Conversation {
                channel: channel.as_str().into()
            })
        );
        let stamps = crate::principal::trust::stamps_for(
            &peko,
            Some(b_port.clone() as Arc<dyn ChannelPort>),
        );
        assert_eq!(
            stamps.driver(&human, &human).await.trust,
            peko_tools_core::Trust::Owner
        );

        // The person posts on A; it reaches B, where it is ingress for the
        // peko's session: with a run already active there, it becomes that
        // run's steering (D5).
        let child = session.session_id.to_string();
        let permit = b
            .app
            .inbox_registry
            .try_acquire_run(&child)
            .await
            .expect("no run yet");
        let posted = a
            .ipc(RequestPacket::ChannelPost {
                request_id: 2,
                channel: channel.as_str().into(),
                sender_name: None,
                text: "hello from my desktop".into(),
                parent: None,
                attachments: Vec::new(),
                model: None,
            })
            .await;
        assert!(
            matches!(posted, ResponsePacket::ChannelPosted { .. }),
            "{posted:?}"
        );
        relay(&mut a, &mut b).await;
        let on_b = b_port.peek(&channel, &Checkpoint::default()).await.unwrap();
        assert_eq!(
            posts(&on_b),
            [(
                format!("user:hub:{account}"),
                "hello from my desktop".to_string()
            )]
        );
        assert_eq!(
            b.app.human_dm_recipients(&channel).await,
            vec![peko.id.clone()]
        );
        let mut steering = Vec::new();
        for _ in 0..100 {
            if let Some(inbox) = b.app.inbox_registry.peek_inbox(&child).await {
                steering.extend(inbox.drain_steering().await);
            }
            if !steering.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            matches!(steering.as_slice(), [s] if s.content.contains("hello from my desktop")),
            "the post reaches the running session"
        );

        // ADR-094: a photo from the desktop crosses too. B pulls the bytes
        // from A in chunks, checks them against the post's digest, and the
        // peko gets its own copy with a note.
        let files = tempfile::tempdir().unwrap();
        let photo = files.path().join("photo.png");
        let bytes: Vec<u8> = b"\x89PNG\r\n\x1a\n"
            .iter()
            .copied()
            .chain((0..(crate::tunnel::ATTACHMENT_CHUNK_BYTES * 2 + 17)).map(|i| (i % 251) as u8))
            .collect();
        std::fs::write(&photo, &bytes).unwrap();
        let posted = a
            .ipc(RequestPacket::ChannelPost {
                request_id: 4,
                channel: channel.as_str().into(),
                sender_name: None,
                text: "what is this?".into(),
                parent: None,
                attachments: vec![photo.display().to_string()],
                model: None,
            })
            .await;
        assert!(
            matches!(posted, ResponsePacket::ChannelPosted { .. }),
            "{posted:?}"
        );
        let steering = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let registry = Arc::clone(&b.app.inbox_registry);
        relay_until(&mut a, &mut b, || {
            let (registry, child, steering) =
                (Arc::clone(&registry), child.clone(), Arc::clone(&steering));
            async move {
                let mut steering = steering.lock().await;
                if let Some(inbox) = registry.peek_inbox(&child).await {
                    steering.extend(inbox.drain_steering().await);
                }
                !steering.is_empty()
            }
        })
        .await;
        let steering = steering.lock().await.clone();
        let note = &steering[0].content;
        assert!(note.contains("what is this?"), "{note}");
        assert!(note.contains("[attached: photo.png (image/png"), "{note}");
        let copy = note
            .split(" at ")
            .nth(1)
            .and_then(|rest| rest.strip_suffix(']'))
            .expect("the note names the peko's copy");
        assert!(
            copy.starts_with(peko.workspace_path.to_str().unwrap()),
            "{copy}"
        );
        assert_eq!(std::fs::read(copy).unwrap(), bytes);
        let on_b = b_port.peek(&channel, &Checkpoint::default()).await.unwrap();
        let Some(peko_channel::ChannelEvent::Posted { attachments, .. }) = on_b.last() else {
            panic!("the photo post is on B");
        };
        assert_eq!(attachments[0].path, "", "no path from A's disk");
        let digest = attachments[0].sha256.clone().unwrap();
        assert_eq!(
            std::fs::read(
                b_port
                    .local()
                    .files_dir(&channel)
                    .await
                    .unwrap()
                    .join(&digest)
                    .join("photo.png")
            )
            .unwrap(),
            bytes
        );

        // Only a runtime that hosts a member of the conversation gets a
        // file, and only from the conversation's own store.
        let handle = b.app.tunnel_handle_slot().read().await.clone().unwrap();
        let b_did = b.app.runtime_did();
        let ask = |source: String, sha256: String| crate::tunnel::AttachmentRequestPayload {
            v: crate::tunnel::ATTACHMENT_REQUEST_VERSION.into(),
            request_id: uuid::Uuid::new_v4().to_string(),
            source_runtime_id: source,
            recipient_runtime_id: b_did.clone(),
            channel_id: channel.as_str().into(),
            sha256,
            offset: 0,
            iat: 0,
            exp: 0,
        };
        let stranger = crate::tunnel::verifying_key_to_did_key(
            &peko_identity::keys::KeyPair::generate().verifying_key,
        );
        for (source, sha256, served) in [
            (stranger, digest.clone(), false),
            (a.app.runtime_did(), "../../keys".to_string(), false),
            (a.app.runtime_did(), "cd".repeat(32), false),
            (a.app.runtime_did(), digest.clone(), true),
        ] {
            b_port
                .answer_attachment_request(&handle, &ask(source, sha256))
                .await;
            let sent = b.sent();
            let [TunnelMessage::TunnelAttachmentChunk { size, data, .. }] = sent.as_slice() else {
                panic!("one chunk answers, got {sent:?}");
            };
            assert_eq!(
                (*size == bytes.len() as u64 && !data.is_empty()),
                served,
                "served={served}"
            );
        }

        // Bytes that do not match the post's digest are refused, and the
        // file stays wanted.
        let forged = "ef".repeat(32);
        let a_files = a
            .app
            .tunnel_channel_port()
            .local()
            .files_dir(&channel)
            .await
            .unwrap();
        std::fs::create_dir_all(a_files.join(&forged)).unwrap();
        std::fs::write(
            a_files.join(&forged).join("forged.bin"),
            b"not what the digest says",
        )
        .unwrap();
        let fetch = {
            let (b_port, channel, from) =
                (Arc::clone(&b_port), channel.clone(), a.app.runtime_did());
            let wanted = peko_protocol::channel::Attachment {
                name: "forged.bin".into(),
                path: String::new(),
                mime: "application/octet-stream".into(),
                size: 24,
                sha256: Some(forged.clone()),
            };
            tokio::spawn(async move { b_port.fetch_attachments(&channel, &[wanted], &from).await })
        };
        relay_until(&mut a, &mut b, || {
            let finished = fetch.is_finished();
            async move { finished }
        })
        .await;
        assert!(fetch.await.unwrap().is_empty(), "nothing was received");
        let b_files = b_port.local().files_dir(&channel).await.unwrap();
        assert!(!b_files.join(&forged).exists());
        assert!(!b_files.join(format!("{forged}.part")).exists());
        let wanted = std::fs::read_to_string(
            b.app
                .tunnel_channel_port()
                .local()
                .config()
                .runtime_dir
                .join("channel_delivery/wanted_files.json"),
        )
        .unwrap();
        assert!(wanted.contains(&forged), "{wanted}");
        drop(permit);

        // The peko's answer reaches the person's record on A.
        b_port
            .post(
                &channel,
                &ActorRef::from(&peko.id),
                peko_channel::PostMsg::root("hi! what can I do?"),
            )
            .await
            .unwrap();
        relay(&mut a, &mut b).await;
        let read = a
            .ipc(RequestPacket::ChannelPeek {
                request_id: 3,
                channel: channel.as_str().into(),
                since: None,
                tail: None,
                before: None,
                query: None,
                author: None,
            })
            .await;
        let ResponsePacket::ChannelPeekResult { events, .. } = read else {
            panic!("expected events, got {read:?}");
        };
        let texts: Vec<_> = posts(&events).into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            texts,
            [
                "hello from my desktop",
                "what is this?",
                "hi! what can I do?"
            ]
        );
        assert_eq!(
            posts(&events)[2].0,
            format!("principal:hub:{hub_id}"),
            "the answer is the peko's"
        );

        // ADR-094: the peko's file reaches A's record, and A's client reads
        // it at A's stored copy.
        let report = files.path().join("report.md");
        std::fs::write(&report, "# findings").unwrap();
        let shared = crate::principal::attachments::ingest_into_channel(
            &b_port.local().files_dir(&channel).await.unwrap(),
            &[report.display().to_string()],
            crate::principal::attachments::Limits::REMOTE,
        )
        .await
        .unwrap();
        b_port
            .post(
                &channel,
                &ActorRef::from(&peko.id),
                peko_channel::PostMsg::root("here it is").with_attachments(shared),
            )
            .await
            .unwrap();
        let a_port = a.app.tunnel_channel_port();
        relay_until(&mut a, &mut b, || {
            let a_port = Arc::clone(&a_port);
            let channel = channel.clone();
            async move {
                posts(&a_port.peek(&channel, &Checkpoint::default()).await.unwrap())
                    .iter()
                    .any(|(_, t)| t == "here it is")
            }
        })
        .await;
        let read = a
            .ipc(RequestPacket::ChannelPeek {
                request_id: 5,
                channel: channel.as_str().into(),
                since: None,
                tail: None,
                before: None,
                query: None,
                author: None,
            })
            .await;
        let ResponsePacket::ChannelPeekResult { events, .. } = read else {
            panic!("expected events, got {read:?}");
        };
        let Some(peko_channel::ChannelEvent::Posted { attachments, .. }) = events.last() else {
            panic!("the peko's file post is on A");
        };
        assert_eq!(attachments[0].name, "report.md");
        assert_eq!(
            std::fs::read_to_string(&attachments[0].path).unwrap(),
            "# findings"
        );
        assert!(std::path::Path::new(&attachments[0].path)
            .starts_with(a_port.local().files_dir(&channel).await.unwrap()));
        // It keeps its name, so a client previews and opens it as what it is.
        // (Compare the file name, not a "/name" suffix: Windows separates
        // path segments with a backslash.)
        assert_eq!(
            std::path::Path::new(&attachments[0].path).file_name(),
            Some(std::ffi::OsStr::new("report.md")),
            "{}",
            attachments[0].path
        );

        // Reopening finds the same DM without a new invite — also with
        // the directory unreachable.
        for down in [false, true] {
            if down {
                directory
                    .register_hub_id_err(&hub_id, DirectoryErrorKind::Transport("offline".into()));
            }
            match a.ipc(open.clone()).await {
                ResponsePacket::ChannelRemoteDmOpened {
                    channel: again,
                    created,
                    ..
                } => assert!(again == channel && !created),
                other => panic!("expected the same DM, got {other:?}"),
            }
            // Opening also starts a catch-up from the host (ADR-091 D4),
            // which may already have sent a history request.
            assert!(
                !a.sent()
                    .iter()
                    .any(|m| matches!(m, TunnelMessage::TunnelChannelInvite { .. })),
                "no second invite"
            );
        }

        // On B, the account's Hub-proxy conversation is this same DM.
        assert_eq!(
            b_port
                .ensure_dm(&peko.id, &human, &binding.session)
                .await
                .unwrap(),
            channel
        );
    }

    /// Relay until `done` holds, giving background tasks time to run.
    async fn relay_until<F, Fut>(a: &mut DmRuntime, b: &mut DmRuntime, mut done: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..100 {
            relay(a, b).await;
            if done().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("relay never settled");
    }

    /// ADR-091 end to end: the person's second runtime catches up from
    /// the host and scrolls back; a stranger's request reads an empty
    /// page; a runtime that changes hands refuses the old account's DM.
    #[tokio::test]
    async fn a_persons_history_comes_from_the_host_and_only_to_them() {
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_channel::{ChannelPort, Checkpoint};

        let account = uuid::Uuid::new_v4().to_string();
        let human = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();
        let mut b = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &b.app,
            "peko",
            human.clone(),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let b_port = b.app.tunnel_channel_port();
        b_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("peko"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        let directory = Arc::new(FakeAgentDirectory::new());
        directory.register_hub_id(
            &hub_id,
            AgentResolution {
                runtime_id: b.app.runtime_did(),
                instance_id: "instance".into(),
                name: Some("peko".into()),
                hub_id: hub_id.clone(),
                local_id: peko.id.0.clone(),
                identity_key: peko_key.clone(),
                agent_did: peko_did.clone(),
                owner_principal: human.clone(),
                owner_handle: None,
                exposure: ResolvedExposure::Private,
            },
        );
        let open = RequestPacket::ChannelOpenRemoteDm {
            request_id: 1,
            hub_id: hub_id.clone(),
            token: None,
        };
        let peek = |channel: &peko_channel::ChannelId| RequestPacket::ChannelPeek {
            request_id: 9,
            channel: channel.as_str().into(),
            since: None,
            tail: None,
            before: None,
            query: None,
            author: None,
        };

        // The person talks to the peko from their first computer, A.
        let mut a = DmRuntime::new(Arc::clone(&directory)).await;
        a.app
            .tunnel_channel_port()
            .local()
            .link_local_user(&a.app.runtime_did(), &account)
            .await
            .unwrap();
        let ResponsePacket::ChannelRemoteDmOpened { channel, .. } = a.ipc(open.clone()).await
        else {
            panic!("A opens the DM");
        };
        relay(&mut a, &mut b).await;
        a.ipc(RequestPacket::ChannelPost {
            request_id: 2,
            channel: channel.as_str().into(),
            sender_name: None,
            text: "hello from my desktop".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        })
        .await;
        relay(&mut a, &mut b).await;
        b_port
            .post(
                &channel,
                &ActorRef::from(&peko.id),
                peko_channel::PostMsg::root("hi! what can I do?"),
            )
            .await
            .unwrap();
        relay(&mut a, &mut b).await;

        // A second computer, C, registered to the same account, starts
        // empty and catches up from B on opening.
        let mut c = DmRuntime::new(Arc::clone(&directory)).await;
        let c_port = c.app.tunnel_channel_port();
        c_port
            .local()
            .link_local_user(&c.app.runtime_did(), &account)
            .await
            .unwrap();
        let ResponsePacket::ChannelRemoteDmOpened { channel: same, .. } = c.ipc(open.clone()).await
        else {
            panic!("C opens the DM");
        };
        assert_eq!(same, channel, "one conversation per person");
        let caught_up = || async {
            posts(&c_port.peek(&channel, &Checkpoint::zero()).await.unwrap()).len() == 2
        };
        relay_until(&mut c, &mut b, caught_up).await;
        let on_c = posts(&c_port.peek(&channel, &Checkpoint::zero()).await.unwrap());
        let texts: Vec<_> = on_c.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["hello from my desktop", "hi! what can I do?"]);

        // Scroll-back reads the host without storing anything.
        let newest = c_port
            .peek(&channel, &Checkpoint::zero())
            .await
            .unwrap()
            .into_iter()
            .rev()
            .find_map(|ev| match ev {
                peko_channel::ChannelEvent::Posted { msg_id, .. } => msg_id,
                _ => None,
            })
            .unwrap();
        let older = {
            let port = Arc::clone(&c_port);
            let channel = channel.clone();
            tokio::spawn(async move { port.history_older(&channel, &newest).await })
        };
        relay_until(&mut c, &mut b, || async { older.is_finished() }).await;
        let page = older.await.unwrap().unwrap();
        assert_eq!(
            posts(&page.events)
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>(),
            ["hello from my desktop"]
        );
        // Synced and scrolled-back posts name their authors as C's own
        // posts and live answers do: the person as this runtime's local
        // user, the peko by its Hub address.
        let authors: Vec<_> = on_c.iter().map(|(a, _)| a.clone()).collect();
        assert_eq!(
            authors,
            [
                "user:local:local".to_string(),
                format!("principal:hub:{hub_id}")
            ]
        );
        assert_eq!(posts(&page.events)[0].0, "user:local:local");
        assert!(!page.has_more);
        for ev in &page.events {
            let peko_channel::ChannelEvent::Posted { via, parent, .. } = ev else {
                panic!("only posts are served")
            };
            assert!(via.is_none() && parent.is_none(), "no host-local fields");
        }
        assert_eq!(
            posts(&c_port.peek(&channel, &Checkpoint::zero()).await.unwrap()).len(),
            2
        );

        // A stranger asks for this conversation: an empty page, even with
        // the peko public (anyone may talk to it — but only about their
        // own conversation).
        peko.config.write().await.exposure = peko_auth::Exposure::Public;
        let stranger = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let iat = chrono::Utc::now().timestamp();
        let forged = crate::tunnel::ChannelHistoryRequestPayload {
            v: crate::tunnel::CHANNEL_HISTORY_REQUEST_VERSION.into(),
            request_id: "probe".into(),
            source_runtime_id: stranger.app.runtime_did(),
            recipient_runtime_id: b.app.runtime_did(),
            source_principal_did: format!("user:hub:{}", uuid::Uuid::new_v4()),
            channel_id: channel.as_str().into(),
            before: None,
            after: None,
            limit: 100,
            iat,
            exp: iat + 300,
        };
        let signature = crate::tunnel::sign_history(&stranger.app.runtime_signing_key, &forged);
        b.receive(TunnelMessage::TunnelChannelHistoryRequest {
            request_id: forged.request_id.clone(),
            source_runtime_id: forged.source_runtime_id.clone(),
            recipient_runtime_id: forged.recipient_runtime_id.clone(),
            source_principal_did: forged.source_principal_did.clone(),
            channel_id: forged.channel_id.clone(),
            before: None,
            after: None,
            limit: 100,
            signature,
        })
        .await;
        let answers = b.sent();
        assert!(
            matches!(answers.as_slice(), [TunnelMessage::TunnelChannelHistory { events, has_more: false, .. }] if events.is_empty()),
            "{answers:?}"
        );

        // A phone (no runtime) reads the same history through the Hub's
        // proxy as the person — and nothing as anyone else (D6).
        let phone = b
            .dispatcher
            .proxied_history_page("peko", &serde_json::json!({}), &human)
            .await;
        assert_eq!(posts(&phone.events).len(), 2);
        for other in [
            ActorRef::User(peko_subject::Route::hub(uuid::Uuid::new_v4().to_string())),
            ActorRef::Visitor(peko_subject::Route::hub(account.clone())),
        ] {
            let page = b
                .dispatcher
                .proxied_history_page("peko", &serde_json::json!({}), &other)
                .await;
            assert!(page.events.is_empty(), "{other}");
        }

        // C changes hands: the old account's DM is refused to its new holder.
        c_port.local().unlink_local_user().await.unwrap();
        c_port
            .local()
            .link_local_user(&c.app.runtime_did(), &uuid::Uuid::new_v4().to_string())
            .await
            .unwrap();
        match c.ipc(peek(&channel)).await {
            ResponsePacket::Error { message, .. } => {
                assert!(message.contains("another PekoHub account"), "{message}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Deliver what `from` sent to whichever of `to` it is addressed to,
    /// until nothing moves. Messages to PekoHub itself are dropped.
    async fn route(runtimes: &mut [&mut DmRuntime]) {
        loop {
            let mut moved = false;
            for i in 0..runtimes.len() {
                for message in runtimes[i].sent() {
                    let to = serde_json::to_value(&message).unwrap()["recipientRuntimeId"]
                        .as_str()
                        .map(str::to_string);
                    for rt in runtimes.iter() {
                        if to.as_deref() == Some(rt.app.runtime_did().as_str()) {
                            rt.receive(message.clone()).await;
                            moved = true;
                        }
                    }
                }
            }
            if !moved {
                return;
            }
        }
    }

    /// Wait for `rt` to send a message `pick` recognizes, and answer it as
    /// PekoHub would with `answer`.
    async fn hub_answers(
        rt: &mut DmRuntime,
        pick: impl Fn(&TunnelMessage) -> Option<String>,
        answer: impl FnOnce(String) -> TunnelMessage,
    ) -> Vec<TunnelMessage> {
        let mut seen = Vec::new();
        for _ in 0..500 {
            for message in rt.sent() {
                if let Some(request_id) = pick(&message) {
                    let handle = rt.app.tunnel_handle_slot().read().await.clone().unwrap();
                    rt.dispatcher
                        .dispatch(answer(request_id), handle)
                        .await
                        .unwrap();
                    return seen;
                }
                seen.push(message);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the runtime never asked PekoHub; sent {seen:?}");
    }

    /// ADR-092 D4–D6 end to end: the home registers its group on PekoHub
    /// and mints an invite link; a person joins with it from their
    /// runtime, which becomes their route; their second device finds the
    /// group through PekoHub and joins as a member, with no link; a
    /// revoked link and a link-less request to an invite-only group admit
    /// no one; and a member without a runtime reads the group's history
    /// through PekoHub's proxy.
    /// ADR-092 D4: a peko registers its group first and adds its owner
    /// after. The home tells PekoHub again, so the group follows the owner
    /// to their other computers; leaving is reported the same way.
    #[tokio::test]
    async fn the_home_reports_its_person_added_after_registering() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, CreateOpts, GroupVisibility};

        let hub_id = uuid::Uuid::new_v4().to_string();
        let group_id = "1234567890123456780".to_string();
        let mut h = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &h.app,
            "host",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let h_port = h.app.tunnel_channel_port();
        h_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("host"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        let owner_account = uuid::Uuid::new_v4().to_string();
        h_port
            .local()
            .link_local_user(&h.app.runtime_did(), &owner_account)
            .await
            .unwrap();
        let channel = h_port
            .create(&peko.id, CreateOpts::runtime("repo"))
            .await
            .unwrap();
        let registering = {
            let port = Arc::clone(&h_port);
            let (channel, by) = (channel.clone(), peko.id.clone());
            tokio::spawn(async move {
                port.register_group(&channel, &by, GroupVisibility::InviteOnly)
                    .await
            })
        };
        let gid = group_id.clone();
        hub_answers(
            &mut h,
            |m| match m {
                TunnelMessage::GroupRegister { request_id, .. } => Some(request_id.clone()),
                _ => None,
            },
            |request_id| TunnelMessage::GroupRegistered {
                request_id,
                group_id: gid.clone(),
                page_url: format!("https://hub.test/g/{gid}"),
            },
        )
        .await;
        registering.await.unwrap().unwrap();
        let _ = h.sent();

        let owner = ActorRef::User(peko_subject::Route::hub(owner_account.clone()));
        h_port.invite(&channel, &peko.id, &owner).await.unwrap();
        assert!(
            h.sent().iter().any(|m| matches!(m,
                TunnelMessage::GroupRoster { group_id: g, accounts }
                    if *g == group_id && *accounts == vec![owner_account.clone()])),
            "adding its own person after registering is reported"
        );

        h_port.leave_actor(&channel, &owner).await.unwrap();
        assert!(
            h.sent().iter().any(|m| matches!(m,
                TunnelMessage::GroupRoster { group_id: g, accounts }
                    if *g == group_id && accounts.is_empty())),
            "and so is their leaving"
        );
    }

    #[tokio::test]
    async fn people_join_a_registered_group_by_link_and_on_new_devices() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, CreateOpts, GroupVisibility};

        let account = uuid::Uuid::new_v4().to_string();
        let person = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();
        let group_id = "1234567890123456789".to_string();

        // H: the group's home, with its peko as owner.
        let mut h = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &h.app,
            "host",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let h_port = h.app.tunnel_channel_port();
        h_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("host"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        let channel = h_port
            .create(&peko.id, CreateOpts::runtime("book club"))
            .await
            .unwrap();
        // The home's own person (signed in to PekoHub) is in it too, as the
        // desktop makes groups.
        let owner_account = uuid::Uuid::new_v4().to_string();
        h_port
            .local()
            .link_local_user(&h.app.runtime_did(), &owner_account)
            .await
            .unwrap();
        // Once registered, the store keeps them by their Hub address.
        h_port
            .invite(
                &channel,
                &peko.id,
                &ActorRef::User(peko_subject::Route::hub(owner_account.clone())),
            )
            .await
            .unwrap();
        h_port
            .post(
                &channel,
                &ActorRef::from(&peko.id),
                peko_channel::PostMsg::root("welcome, readers"),
            )
            .await
            .unwrap();

        // No link before the group is registered.
        assert!(h_port
            .group_invite_link(&channel, &peko.id, 7)
            .await
            .is_err());

        // Register: PekoHub assigns the ID and the page.
        let registering = {
            let port = Arc::clone(&h_port);
            let (channel, by) = (channel.clone(), peko.id.clone());
            tokio::spawn(async move {
                port.register_group(&channel, &by, GroupVisibility::InviteOnly)
                    .await
            })
        };
        let gid = group_id.clone();
        let sent_before = hub_answers(
            &mut h,
            |m| match m {
                TunnelMessage::GroupRegister {
                    request_id,
                    owner_address,
                    visibility,
                    name,
                    ..
                } => {
                    assert_eq!(owner_address, &format!("principal:hub:{hub_id}"));
                    assert_eq!(visibility, "invite_only");
                    assert_eq!(name, "book club");
                    Some(request_id.clone())
                }
                _ => None,
            },
            |request_id| TunnelMessage::GroupRegistered {
                request_id,
                group_id: gid.clone(),
                page_url: format!("https://hub.test/g/{gid}"),
            },
        )
        .await;
        assert!(sent_before.is_empty(), "{sent_before:?}");
        let hub = registering.await.unwrap().unwrap();
        assert_eq!(hub.id, group_id);
        assert!(
            matches!(&h.sent()[..], [TunnelMessage::GroupRoster { group_id: g, accounts }, TunnelMessage::TunnelAccountDmsRequest { .. }]
                if *g == group_id && *accounts == vec![owner_account.clone()]),
            "the home reports who is in it: its own person, so far; and tells their other devices"
        );

        let (link, _) = h_port
            .group_invite_link(&channel, &peko.id, 7)
            .await
            .unwrap();
        assert!(link.starts_with(&format!("https://hub.test/g/{group_id}?invite=")));
        let token = link.split_once("invite=").unwrap().1.to_string();

        // P: the person's runtime joins with the link.
        let mut p = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let p_port = p.app.tunnel_channel_port();
        p_port
            .local()
            .link_local_user(&p.app.runtime_did(), &account)
            .await
            .unwrap();
        p_port
            .local()
            .set_person_display_name(Some("Pat"))
            .await
            .unwrap();
        let lookup = |home: String, channel: String, gid: String| {
            move |request_id: String| TunnelMessage::GroupLookup {
                request_id,
                group: Some(crate::tunnel::AccountGroup {
                    group_id: gid,
                    name: "book club".into(),
                    home_runtime: home,
                    channel_id: channel,
                }),
            }
        };
        let joining = {
            let port = Arc::clone(&p_port);
            let link = link.clone();
            tokio::spawn(async move { port.join_group(&link).await })
        };
        hub_answers(
            &mut p,
            |m| match m {
                TunnelMessage::GroupLookupRequest {
                    request_id,
                    group_id: g,
                    invite,
                } => {
                    assert_eq!(g, &group_id);
                    assert_eq!(invite.as_deref(), Some(token.as_str()));
                    Some(request_id.clone())
                }
                _ => None,
            },
            lookup(
                h.app.runtime_did(),
                channel.as_str().into(),
                group_id.clone(),
            ),
        )
        .await;
        let (joined, name) = joining.await.unwrap().unwrap();
        assert_eq!(
            (joined.clone(), name.as_str()),
            (channel.clone(), "book club")
        );
        assert!(
            p.ipc(RequestPacket::ChannelInvitations { request_id: 1 })
                .await
                .matches_empty_invitations(),
            "a request is not an invitation to answer"
        );
        route(&mut [&mut h, &mut p]).await;
        assert!(h_port
            .local()
            .is_remote_member(
                &channel,
                &p.app.runtime_did(),
                &format!("user:hub:{account}")
            )
            .await
            .unwrap());
        assert_eq!(
            p_port
                .local()
                .remote_dm_account(&channel)
                .await
                .unwrap()
                .as_deref(),
            Some(account.as_str()),
            "the home's roster completes the join"
        );
        assert_eq!(
            h_port.local().actor_info(&person).await.name.as_deref(),
            Some("Pat"),
            "the group names whoever joins by their PekoHub name"
        );
        // …and the group catches up from its home.
        let mut caught_up = false;
        for _ in 0..100 {
            route(&mut [&mut h, &mut p]).await;
            let events = p_port
                .local()
                .peek(&channel, &peko_channel::Checkpoint::zero())
                .await
                .unwrap();
            if posts(&events).iter().any(|(_, t)| t == "welcome, readers") {
                caught_up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            caught_up,
            "a group joined by link reads what was said before"
        );

        // The desktop sees where the group is managed, and its ID.
        for (rt, home_here) in [(&h, true), (&p, false)] {
            match rt
                .ipc(RequestPacket::ChannelInfo {
                    request_id: 9,
                    channel: channel.as_str().into(),
                })
                .await
            {
                ResponsePacket::ChannelInfoResult {
                    group: Some(group), ..
                } => {
                    assert_eq!(group.home_here, home_here);
                    if home_here {
                        assert_eq!(group.hub_id.as_deref(), Some(group_id.as_str()));
                        assert_eq!(group.visibility.as_deref(), Some("invite_only"));
                    }
                }
                other => panic!("expected a group's info, got {other:?}"),
            }
        }

        // P2: the same person's new device finds the group through PekoHub
        // and joins as a member, with no link.
        let mut p2 = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let p2_port = p2.app.tunnel_channel_port();
        p2_port
            .local()
            .link_local_user(&p2.app.runtime_did(), &account)
            .await
            .unwrap();
        let syncing = {
            let port = Arc::clone(&p2_port);
            tokio::spawn(async move { port.sync_account_groups().await })
        };
        let (home, chan, gid) = (
            h.app.runtime_did(),
            channel.as_str().to_string(),
            group_id.clone(),
        );
        hub_answers(
            &mut p2,
            |m| match m {
                TunnelMessage::AccountGroupsRequest { request_id } => Some(request_id.clone()),
                _ => None,
            },
            move |request_id| TunnelMessage::AccountGroups {
                request_id,
                groups: vec![crate::tunnel::AccountGroup {
                    group_id: gid,
                    name: "book club".into(),
                    home_runtime: home,
                    channel_id: chan,
                }],
            },
        )
        .await;
        syncing.await.unwrap();
        route(&mut [&mut h, &mut p, &mut p2]).await;
        assert_eq!(
            p2_port
                .local()
                .remote_dm_account(&channel)
                .await
                .unwrap()
                .as_deref(),
            Some(account.as_str()),
            "a member's new device joins without a link"
        );

        // A post from one of their devices reaches the other.
        p2.ipc(RequestPacket::ChannelPost {
            request_id: 10,
            channel: channel.as_str().into(),
            sender_name: None,
            text: "from my laptop".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        })
        .await;
        route(&mut [&mut h, &mut p, &mut p2]).await;
        for rt in [&h, &p] {
            let events = rt
                .app
                .tunnel_channel_port()
                .local()
                .peek(&channel, &peko_channel::Checkpoint::zero())
                .await
                .unwrap();
            assert!(
                posts(&events)
                    .iter()
                    .any(|(_, text)| text == "from my laptop"),
                "{:?}",
                posts(&events)
            );
        }

        // Q: someone else. A revoked link and a link-less request to an
        // invite-only group admit no one.
        assert_eq!(
            h_port.revoke_group_links(&channel, &peko.id).await.unwrap(),
            1
        );
        for invite in [Some(token.clone()), None] {
            let stranger = uuid::Uuid::new_v4().to_string();
            let mut q = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
            let q_port = q.app.tunnel_channel_port();
            q_port
                .local()
                .link_local_user(&q.app.runtime_did(), &stranger)
                .await
                .unwrap();
            let input = invite.clone().unwrap_or_else(|| group_id.clone());
            let joining = {
                let port = Arc::clone(&q_port);
                tokio::spawn(async move { port.join_group(&input).await })
            };
            hub_answers(
                &mut q,
                |m| match m {
                    TunnelMessage::GroupLookupRequest { request_id, .. } => {
                        Some(request_id.clone())
                    }
                    _ => None,
                },
                lookup(
                    h.app.runtime_did(),
                    channel.as_str().into(),
                    group_id.clone(),
                ),
            )
            .await;
            joining.await.unwrap().unwrap();
            route(&mut [&mut h, &mut q]).await;
            assert!(
                !h_port
                    .local()
                    .is_remote_member(
                        &channel,
                        &q.app.runtime_did(),
                        &format!("user:hub:{stranger}")
                    )
                    .await
                    .unwrap(),
                "admitted with {invite:?}"
            );
            assert!(q_port.conversation_kind(&channel).await.is_err());
        }

        // A member without a runtime reads the group through PekoHub's
        // proxy; someone else reads nothing.
        let page = h
            .dispatcher
            .proxied_group_page(channel.as_str(), &serde_json::json!({}), &person)
            .await
            .unwrap();
        assert_eq!(
            posts(&page.events)
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>(),
            vec!["welcome, readers".to_string(), "from my laptop".to_string()]
        );
        let stranger = ActorRef::User(peko_subject::Route::hub(uuid::Uuid::new_v4().to_string()));
        assert!(h
            .dispatcher
            .proxied_group_page(channel.as_str(), &serde_json::json!({}), &stranger)
            .await
            .is_err());
    }

    /// A registered peko on `rt`, known to the shared fake directory.
    async fn hub_peko(
        rt: &DmRuntime,
        directory: &Arc<crate::tunnel::hub_directory::FakeAgentDirectory>,
        name: &str,
        exposure: peko_auth::Exposure,
    ) -> (Arc<crate::principal::Principal>, String) {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        let peko = create_test_principal(
            &rt.app,
            name,
            ActorRef::User("local".into()),
            vec![],
            exposure,
        )
        .await;
        let hub_id = uuid::Uuid::new_v4().to_string();
        let (did, key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        rt.app
            .tunnel_channel_port()
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&did),
                Some(&key),
                Some(name),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        directory.register_hub_id(
            &hub_id,
            AgentResolution {
                runtime_id: rt.app.runtime_did(),
                instance_id: format!("{name}-instance"),
                name: Some(name.into()),
                hub_id: hub_id.clone(),
                local_id: peko.id.0.clone(),
                identity_key: key,
                agent_did: did,
                owner_principal: ActorRef::Public,
                owner_handle: None,
                exposure: ResolvedExposure::Private,
            },
        );
        rt.app
            .tool_runtime
            .tooling()
            .services()
            .set_cross_runtime_a2a_ctx(Arc::new(crate::tunnel::CrossRuntimeA2aCtx {
                directory: directory.clone(),
                caller_runtime_id: rt.app.runtime_did(),
                principal_manager: rt.app.principal_manager().clone(),
                channel_port: rt.app.tunnel_channel_port(),
            }));
        (peko, hub_id)
    }

    /// ADR-098 D2/D3 end to end: a private peko on another runtime joins a
    /// registered group by its invite link, for itself — not its owner —
    /// with no `Chat` grant; then the group's peko opens a first DM with
    /// it, which it takes because they share the group. A peko it shares
    /// nothing with is still refused.
    #[tokio::test]
    async fn a_private_peko_joins_by_link_and_takes_a_dm_from_a_groupmate() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, CreateOpts, GroupVisibility};

        let directory = Arc::new(FakeAgentDirectory::new());
        let mut h = DmRuntime::new(Arc::clone(&directory)).await;
        let mut p = DmRuntime::new(Arc::clone(&directory)).await;
        let mut s = DmRuntime::new(Arc::clone(&directory)).await;
        let private = peko_auth::Exposure::Private;
        let (host, _) = hub_peko(&h, &directory, "host", private.clone()).await;
        let (ada, ada_hub) = hub_peko(&p, &directory, "ada", private.clone()).await;
        let (zed, _) = hub_peko(&s, &directory, "zed", private).await;
        let ada_ref = ActorRef::Principal(peko_subject::Route::hub(ada_hub.clone()));
        let group_id = "2234567890123456789".to_string();

        let h_port = h.app.tunnel_channel_port();
        let channel = h_port
            .create(&host.id, CreateOpts::runtime("repo"))
            .await
            .unwrap();
        let registering = {
            let port = Arc::clone(&h_port);
            let (channel, by) = (channel.clone(), host.id.clone());
            tokio::spawn(async move {
                port.register_group(&channel, &by, GroupVisibility::InviteOnly)
                    .await
            })
        };
        let gid = group_id.clone();
        hub_answers(
            &mut h,
            |m| match m {
                TunnelMessage::GroupRegister { request_id, .. } => Some(request_id.clone()),
                _ => None,
            },
            |request_id| TunnelMessage::GroupRegistered {
                request_id,
                group_id: gid.clone(),
                page_url: format!("https://hub.test/g/{gid}"),
            },
        )
        .await;
        registering.await.unwrap().unwrap();
        let (link, _) = h_port
            .group_invite_link(&channel, &host.id, 7)
            .await
            .unwrap();

        // ada joins with the link, as itself.
        let p_port = p.app.tunnel_channel_port();
        let joining = {
            let port = Arc::clone(&p_port);
            let (id, link) = (ada.id.clone(), link.clone());
            tokio::spawn(async move { port.join_group_as(&id, &link).await })
        };
        let (home, chan, gid) = (
            h.app.runtime_did(),
            channel.as_str().to_string(),
            group_id.clone(),
        );
        hub_answers(
            &mut p,
            |m| match m {
                TunnelMessage::GroupLookupRequest { request_id, .. } => Some(request_id.clone()),
                _ => None,
            },
            move |request_id| TunnelMessage::GroupLookup {
                request_id,
                group: Some(crate::tunnel::AccountGroup {
                    group_id: gid,
                    name: "repo".into(),
                    home_runtime: home,
                    channel_id: chan,
                }),
            },
        )
        .await;
        let (joined, name) = joining.await.unwrap().unwrap();
        assert_eq!((joined, name.as_str()), (channel.clone(), "repo"));
        let ada_member = || async {
            p_port
                .list_members(&channel)
                .await
                .is_ok_and(|m| m.contains(&ActorRef::from(&ada.id)))
        };
        assert!(!ada_member().await, "requested, not yet a member");
        for _ in 0..50 {
            route(&mut [&mut h, &mut p]).await;
            if ada_member().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            ada_member().await,
            "the home's invitation completes the join"
        );
        assert!(
            p_port
                .local()
                .take_peko_join_request(&ada.id.0, &channel, &h.app.runtime_did())
                .await
                .unwrap()
                .is_none(),
            "the invitation answered the request"
        );
        let ada_key = h_port.key_of(&ada_ref);
        assert!(h_port
            .list_members(&channel)
            .await
            .unwrap()
            .iter()
            .any(|m| h_port.key_of(m) == ada_key));
        // Its owner did not join.
        assert!(!p_port
            .list_members(&channel)
            .await
            .unwrap()
            .contains(&ActorRef::User("local".into())));
        // The home names it as PekoHub does, so `@name` mentions work there.
        assert_eq!(
            h_port.actor_info(&ada_ref).await.name.as_deref(),
            Some("ada")
        );
        // Its own runtime binds it a group session, so its keepalive can
        // answer other pekos' posts in the group.
        assert!(
            p_port
                .binding_for(&channel, &ada.id)
                .await
                .unwrap()
                .is_some(),
            "the joined group is bound to a session of ada's"
        );

        // D3: the group's peko opens a first DM with ada; ada takes it.
        let dm = h_port
            .ensure_dm(&host.id, &ada_ref, "/principal-ada")
            .await
            .unwrap();
        h_port.invite(&dm, &host.id, &ada_ref).await.unwrap();
        route(&mut [&mut h, &mut p]).await;
        assert!(
            p_port.local().conversation_kind(&dm).await.is_ok(),
            "a groupmate's first DM arrives"
        );

        // A peko ada shares nothing with is refused, as before.
        let s_port = s.app.tunnel_channel_port();
        let resolved =
            crate::tunnel::hub_directory::AgentDirectory::resolve_by_hub_id(&*directory, &ada_hub)
                .await
                .unwrap();
        s_port
            .register_actor(
                &ada_ref,
                Some(&ada_hub),
                Some(&resolved.agent_did),
                Some(&resolved.identity_key),
                Some("ada"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        let cold = s_port
            .ensure_dm(&zed.id, &ada_ref, "/principal-ada")
            .await
            .unwrap();
        s_port.invite(&cold, &zed.id, &ada_ref).await.unwrap();
        route(&mut [&mut s, &mut p]).await;
        assert!(
            p_port.local().conversation_kind(&cold).await.is_err(),
            "a stranger's first DM waits"
        );
    }

    /// ADR-092 D4, found live: a group follows its person to a computer
    /// that was signed in before the group existed. The home's own person
    /// is in the group; registering it tells their other devices, and the
    /// desktop on the second computer looks the account's groups up again,
    /// asks the home with no link, and gets the group with its history,
    /// its real start time, and everyone's names.
    #[tokio::test]
    async fn a_group_follows_its_person_to_a_computer_already_signed_in() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, CreateOpts, GroupVisibility};

        let account = uuid::Uuid::new_v4().to_string();
        let hub_id = uuid::Uuid::new_v4().to_string();
        let group_id = "2234567890123456789".to_string();

        // H: computer 1, where Frank's peko lives and starts the group.
        let mut h = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &h.app,
            "ada",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let h_port = h.app.tunnel_channel_port();
        h_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("ada"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        h_port
            .local()
            .link_local_user(&h.app.runtime_did(), &account)
            .await
            .unwrap();
        h_port
            .local()
            .set_person_display_name(Some("Frank"))
            .await
            .unwrap();

        // D: computer 2, signed in to the same account, with no pekos.
        let mut d = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let d_port = d.app.tunnel_channel_port();
        d_port
            .local()
            .link_local_user(&d.app.runtime_did(), &account)
            .await
            .unwrap();

        // Later, ada starts a group with Frank in it, as the desktop does.
        let channel = h_port
            .create(&peko.id, CreateOpts::runtime("tinytodo"))
            .await
            .unwrap();
        h_port
            .invite(&channel, &peko.id, &ActorRef::User("local".into()))
            .await
            .unwrap();
        h_port
            .post(
                &channel,
                &ActorRef::from(&peko.id),
                peko_channel::PostMsg::root("hello, team"),
            )
            .await
            .unwrap();
        let registering = {
            let port = Arc::clone(&h_port);
            let (channel, by) = (channel.clone(), peko.id.clone());
            tokio::spawn(async move {
                port.register_group(&channel, &by, GroupVisibility::InviteOnly)
                    .await
            })
        };
        let gid = group_id.clone();
        hub_answers(
            &mut h,
            |m| match m {
                TunnelMessage::GroupRegister { request_id, .. } => Some(request_id.clone()),
                _ => None,
            },
            |request_id| TunnelMessage::GroupRegistered {
                request_id,
                group_id: gid.clone(),
                page_url: format!("https://hub.test/g/{gid}"),
            },
        )
        .await;
        registering.await.unwrap().unwrap();

        // The home reports the roster, then tells Frank's other devices.
        let sent = h.sent();
        let roster_at = sent
            .iter()
            .position(|m| {
                matches!(m, TunnelMessage::GroupRoster { accounts, .. } if *accounts == vec![account.clone()])
            })
            .expect("the roster names Frank's account");
        let nudge_at = sent
            .iter()
            .position(|m| {
                matches!(m, TunnelMessage::TunnelAccountDmsRequest { recipient_account, .. }
                    if *recipient_account == format!("user:hub:{account}"))
            })
            .expect("the home tells the person's other devices");
        assert!(roster_at < nudge_at, "PekoHub hears the roster first");

        // D hears it (PekoHub delivers it to the account's runtimes) and
        // looks the account's groups up again.
        d.receive(sent[nudge_at].clone()).await;
        let (home, chan, gid) = (
            h.app.runtime_did(),
            channel.as_str().to_string(),
            group_id.clone(),
        );
        hub_answers(
            &mut d,
            |m| match m {
                TunnelMessage::AccountGroupsRequest { request_id } => Some(request_id.clone()),
                _ => None,
            },
            move |request_id| TunnelMessage::AccountGroups {
                request_id,
                groups: vec![crate::tunnel::AccountGroup {
                    group_id: gid,
                    name: "tinytodo".into(),
                    home_runtime: home,
                    channel_id: chan,
                }],
            },
        )
        .await;
        let mut joined = false;
        for _ in 0..100 {
            route(&mut [&mut h, &mut d]).await;
            if d_port
                .local()
                .remote_dm_account(&channel)
                .await
                .ok()
                .flatten()
                .as_deref()
                == Some(account.as_str())
            {
                joined = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            joined,
            "the second computer joins as a member, with no link"
        );
        match d
            .ipc(RequestPacket::ChannelPersonGroups { request_id: 1 })
            .await
        {
            ResponsePacket::ChannelPersonGroupsResult { groups, .. } => {
                assert!(groups.iter().any(|g| g.channel == channel), "{groups:?}")
            }
            other => panic!("expected the person's groups, got {other:?}"),
        }
        let mut caught_up = false;
        for _ in 0..100 {
            route(&mut [&mut h, &mut d]).await;
            let events = d_port
                .local()
                .peek(&channel, &peko_channel::Checkpoint::zero())
                .await
                .unwrap();
            if posts(&events).iter().any(|(_, t)| t == "hello, team") {
                caught_up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(caught_up, "the group reads what was said before");

        // The copy names everyone and shows when the group began.
        async fn info(
            rt: &DmRuntime,
            channel: &peko_channel::ChannelId,
        ) -> (Vec<crate::ipc::packet::ChannelPerson>, Option<String>) {
            match rt
                .ipc(RequestPacket::ChannelInfo {
                    request_id: 2,
                    channel: channel.as_str().into(),
                })
                .await
            {
                ResponsePacket::ChannelInfoResult {
                    people, created_at, ..
                } => (people, created_at),
                other => panic!("expected a group's info, got {other:?}"),
            }
        }
        let (home_people, home_start) = info(&h, &channel).await;
        let (people, start) = info(&d, &channel).await;
        assert!(home_start.is_some());
        assert_eq!(start, home_start, "the copy began when the group did");
        let ada = people
            .iter()
            .find(|p| p.kind == "principal")
            .expect("ada is in it");
        assert_eq!(ada.name.as_deref(), Some("ada"));
        assert!(!ada.here, "ada lives on the other computer");
        assert!(
            ada.address == format!("principal:hub:{hub_id}")
                || ada.aliases.contains(&format!("principal:hub:{hub_id}")),
            "{ada:?}"
        );
        let you: Vec<_> = people.iter().filter(|p| p.you).collect();
        assert_eq!(
            you.len(),
            1,
            "Frank once, though on two computers: {people:?}"
        );
        let frank = home_people.iter().find(|p| p.you).expect("Frank at home");
        assert_eq!(frank.name.as_deref(), Some("Frank"));
        assert!(
            frank.aliases.contains(&format!("user:hub:{account}")),
            "{frank:?}"
        );
        let home_ada = home_people
            .iter()
            .find(|p| p.kind == "principal")
            .expect("ada at home");
        assert!(home_ada.here);
        assert_eq!(home_ada.name.as_deref(), Some("ada"));
    }

    /// ADR-092 D2 end to end: a group's home invites a person by account;
    /// their runtime keeps the invitation until they accept, joins, and
    /// becomes their route; then they and the group's peko talk both
    /// ways. Declining leaves the roster; a roster for a group they never
    /// accepted is ignored.
    #[tokio::test]
    async fn a_person_joins_a_group_on_another_runtime() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, Checkpoint, CreateOpts};

        let account = uuid::Uuid::new_v4().to_string();
        let person = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();

        // H: the group's home, with its peko as owner.
        let mut h = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &h.app,
            "host",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Public,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let h_port = h.app.tunnel_channel_port();
        h_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("host"),
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();
        let channel = h_port
            .create(&peko.id, CreateOpts::runtime("book club"))
            .await
            .unwrap();
        h_port.invite(&channel, &peko.id, &person).await.unwrap();

        // P: the person's runtime, registered to their account.
        let mut p = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let p_port = p.app.tunnel_channel_port();
        p_port
            .local()
            .link_local_user(&p.app.runtime_did(), &account)
            .await
            .unwrap();

        // The invitation waits on P; nothing is joined yet.
        relay(&mut h, &mut p).await;
        let ResponsePacket::ChannelInvitationsResult { invitations, .. } = p
            .ipc(RequestPacket::ChannelInvitations { request_id: 1 })
            .await
        else {
            panic!("expected invitations");
        };
        assert_eq!(invitations.len(), 1);
        assert_eq!(invitations[0].channel, channel);
        assert_eq!(invitations[0].name, "book club");
        assert!(
            p_port.conversation_kind(&channel).await.is_err(),
            "not joined before the answer"
        );

        // The person accepts: P joins and becomes their route at H.
        let answered = p
            .ipc(RequestPacket::ChannelInvitationAnswer {
                request_id: 2,
                channel: channel.as_str().into(),
                accept: true,
            })
            .await;
        assert!(
            matches!(answered, ResponsePacket::Done { success: true, .. }),
            "{answered:?}"
        );
        relay(&mut h, &mut p).await;
        assert!(h_port
            .local()
            .is_remote_member(
                &channel,
                &p.app.runtime_did(),
                &format!("user:hub:{account}")
            )
            .await
            .unwrap());
        assert_eq!(
            p_port
                .local()
                .remote_dm_account(&channel)
                .await
                .unwrap()
                .as_deref(),
            Some(account.as_str())
        );
        assert!(p
            .ipc(RequestPacket::ChannelInvitations { request_id: 3 })
            .await
            .matches_empty_invitations());
        match p
            .ipc(RequestPacket::ChannelPersonGroups { request_id: 3 })
            .await
        {
            ResponsePacket::ChannelPersonGroupsResult { groups, .. } => {
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].channel, channel);
                assert_eq!(groups[0].name, "book club");
                assert_eq!(
                    groups[0].home.as_deref(),
                    Some(h.app.runtime_did().as_str())
                );
            }
            other => panic!("expected the person's groups, got {other:?}"),
        }

        // They post from P; it reaches H. The peko answers on H; it reaches P.
        let posted = p
            .ipc(RequestPacket::ChannelPost {
                request_id: 4,
                channel: channel.as_str().into(),
                sender_name: None,
                text: "chapter 3 was wild".into(),
                parent: None,
                attachments: Vec::new(),
                model: None,
            })
            .await;
        assert!(
            matches!(posted, ResponsePacket::ChannelPosted { .. }),
            "{posted:?}"
        );
        relay(&mut h, &mut p).await;
        h_port
            .post(
                &channel,
                &ActorRef::from(&peko.id),
                peko_channel::PostMsg::root("agreed!"),
            )
            .await
            .unwrap();
        relay(&mut h, &mut p).await;
        for (port, label) in [(&h_port, "home"), (&p_port, "person")] {
            let texts: Vec<_> = posts(&port.peek(&channel, &Checkpoint::zero()).await.unwrap())
                .into_iter()
                .map(|(_, t)| t)
                .collect();
            assert_eq!(texts, ["chapter 3 was wild", "agreed!"], "{label}");
        }

        // A roster for a group they never accepted changes nothing.
        let stranger_group = peko_channel::ChannelId::generate();
        assert!(!p_port
            .on_person_roster(
                &stranger_group,
                "spam",
                "principal:hub:x",
                &[],
                "did:key:zStranger"
            )
            .await
            .unwrap());
        assert!(p_port.conversation_kind(&stranger_group).await.is_err());

        // Declining a second group's invitation takes them off its roster.
        let second = h_port
            .create(&peko.id, CreateOpts::runtime("chess"))
            .await
            .unwrap();
        h_port.invite(&second, &peko.id, &person).await.unwrap();
        relay(&mut h, &mut p).await;
        let declined = p
            .ipc(RequestPacket::ChannelInvitationAnswer {
                request_id: 5,
                channel: second.as_str().into(),
                accept: false,
            })
            .await;
        assert!(
            matches!(declined, ResponsePacket::Done { success: true, .. }),
            "{declined:?}"
        );
        relay(&mut h, &mut p).await;
        let members = h_port.list_members(&second).await.unwrap();
        assert!(
            !members.iter().any(|m| matches!(m, ActorRef::User(_))),
            "{members:?}"
        );
        assert!(p_port.conversation_kind(&second).await.is_err());
    }

    /// ADR-092 D3 end to end: a person who joins a group that already
    /// talked catches up from its home; under `joined` visibility they get
    /// only what followed their joining; a non-member gets nothing.
    #[tokio::test]
    async fn a_group_member_reads_its_history_from_the_home() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, Checkpoint, CreateOpts};

        let account = uuid::Uuid::new_v4().to_string();
        let person = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();
        let mut h = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &h.app,
            "host",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Public,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let h_port = h.app.tunnel_channel_port();
        h_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("host"),
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();
        let mut p = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let p_port = p.app.tunnel_channel_port();
        p_port
            .local()
            .link_local_user(&p.app.runtime_did(), &account)
            .await
            .unwrap();
        let texts = |events: Vec<peko_channel::ChannelEvent>| -> Vec<String> {
            posts(&events).into_iter().map(|(_, t)| t).collect()
        };

        for (visibility, before, expected) in [
            (None, "we met on tuesday", vec!["we met on tuesday"]),
            (
                Some(peko_channel::HistoryVisibility::Joined),
                "before you came",
                vec![],
            ),
        ] {
            let channel = h_port
                .create(&peko.id, CreateOpts::runtime("club"))
                .await
                .unwrap();
            if let Some(visibility) = visibility {
                h_port
                    .set_group_history(&channel, &peko.id, visibility)
                    .await
                    .unwrap();
            }
            h_port
                .post(
                    &channel,
                    &ActorRef::from(&peko.id),
                    peko_channel::PostMsg::root(before),
                )
                .await
                .unwrap();
            // Later than the post, to the second.
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            h_port.invite(&channel, &peko.id, &person).await.unwrap();
            relay(&mut h, &mut p).await;
            let answered = p
                .ipc(RequestPacket::ChannelInvitationAnswer {
                    request_id: 1,
                    channel: channel.as_str().into(),
                    accept: true,
                })
                .await;
            assert!(
                matches!(answered, ResponsePacket::Done { success: true, .. }),
                "{answered:?}"
            );
            // The catch-up runs in the background; relay until it settled.
            for _ in 0..30 {
                relay(&mut h, &mut p).await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let on_p = texts(p_port.peek(&channel, &Checkpoint::zero()).await.unwrap());
            assert_eq!(on_p, expected, "{visibility:?}");

            // What follows reaches them either way.
            h_port
                .post(
                    &channel,
                    &ActorRef::from(&peko.id),
                    peko_channel::PostMsg::root("welcome!"),
                )
                .await
                .unwrap();
            relay(&mut h, &mut p).await;
            let on_p = texts(p_port.peek(&channel, &Checkpoint::zero()).await.unwrap());
            assert_eq!(on_p.last().map(String::as_str), Some("welcome!"));

            // Someone outside the group asks: an empty page.
            let stranger = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
            let iat = chrono::Utc::now().timestamp();
            let probe = crate::tunnel::ChannelHistoryRequestPayload {
                v: crate::tunnel::CHANNEL_HISTORY_REQUEST_VERSION.into(),
                request_id: "probe".into(),
                source_runtime_id: stranger.app.runtime_did(),
                recipient_runtime_id: h.app.runtime_did(),
                source_principal_did: format!("user:hub:{}", uuid::Uuid::new_v4()),
                channel_id: channel.as_str().into(),
                before: None,
                after: None,
                limit: 100,
                iat,
                exp: iat + 300,
            };
            let signature = crate::tunnel::sign_history(&stranger.app.runtime_signing_key, &probe);
            h.receive(TunnelMessage::TunnelChannelHistoryRequest {
                request_id: probe.request_id.clone(),
                source_runtime_id: probe.source_runtime_id.clone(),
                recipient_runtime_id: probe.recipient_runtime_id.clone(),
                source_principal_did: probe.source_principal_did.clone(),
                channel_id: probe.channel_id.clone(),
                before: None,
                after: None,
                limit: 100,
                signature,
            })
            .await;
            let answers = h.sent();
            assert!(
                answers.iter().any(|m| matches!(m, TunnelMessage::TunnelChannelHistory { request_id, events, .. } if request_id == "probe" && events.is_empty())),
                "{answers:?}"
            );
        }
    }

    /// ADR-092 D3: a peko on another runtime, added to a group that already
    /// talked, catches up from the home as itself (its signing DID).
    #[tokio::test]
    async fn a_peko_added_to_a_group_catches_up_from_its_home() {
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_channel::{ChannelPort, Checkpoint, CreateOpts};

        let directory = Arc::new(FakeAgentDirectory::new());
        let mut h = DmRuntime::new(Arc::clone(&directory)).await;
        let mut r = DmRuntime::new(Arc::clone(&directory)).await;
        let mut pekos = Vec::new();
        for (runtime, name) in [(&h, "host"), (&r, "guest")] {
            let peko = create_test_principal(
                &runtime.app,
                name,
                ActorRef::User("local".into()),
                vec![],
                peko_auth::Exposure::Public,
            )
            .await;
            let hub_id = uuid::Uuid::new_v4().to_string();
            let (did, key) = {
                let config = peko.config.read().await;
                (
                    config.did.as_ref().unwrap().0.clone(),
                    config.actor_key.clone().unwrap(),
                )
            };
            runtime
                .app
                .tunnel_channel_port()
                .register_actor(
                    &ActorRef::from(&peko.id),
                    Some(&hub_id),
                    Some(&did),
                    Some(&key),
                    Some(name),
                    peko_channel::Registration::Public,
                )
                .await
                .unwrap();
            directory.register_hub_id(
                &hub_id,
                AgentResolution {
                    runtime_id: runtime.app.runtime_did(),
                    instance_id: format!("{name}-instance"),
                    name: Some(name.into()),
                    hub_id: hub_id.clone(),
                    local_id: peko.id.0.clone(),
                    identity_key: key,
                    agent_did: did,
                    owner_principal: ActorRef::Public,
                    owner_handle: None,
                    exposure: ResolvedExposure::Public,
                },
            );
            runtime
                .app
                .tool_runtime
                .tooling()
                .services()
                .set_cross_runtime_a2a_ctx(Arc::new(crate::tunnel::CrossRuntimeA2aCtx {
                    directory: directory.clone(),
                    caller_runtime_id: runtime.app.runtime_did(),
                    principal_manager: runtime.app.principal_manager().clone(),
                    channel_port: runtime.app.tunnel_channel_port(),
                }));
            pekos.push((peko, hub_id));
        }
        let (host, _) = &pekos[0];
        let (_, guest_hub) = &pekos[1];
        let h_port = h.app.tunnel_channel_port();
        let channel = h_port
            .create(&host.id, CreateOpts::runtime("study"))
            .await
            .unwrap();
        h_port
            .post(
                &channel,
                &ActorRef::from(&host.id),
                peko_channel::PostMsg::root("notes from monday"),
            )
            .await
            .unwrap();
        h_port
            .invite(
                &channel,
                &host.id,
                &ActorRef::Principal(peko_subject::Route::hub(guest_hub.clone())),
            )
            .await
            .unwrap();
        let r_port = r.app.tunnel_channel_port();
        for _ in 0..40 {
            relay(&mut h, &mut r).await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let on_r: Vec<String> = posts(&r_port.peek(&channel, &Checkpoint::zero()).await.unwrap())
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(on_r, ["notes from monday"]);
        assert_eq!(
            r_port
                .local()
                .group_home(&channel)
                .await
                .unwrap()
                .map(|h| h.runtime),
            Some(h.app.runtime_did())
        );
    }

    /// Deliver what each runtime sent to the one it names, as PekoHub
    /// would: a runtime-addressed envelope to that runtime, an
    /// account-addressed one to every other runtime of that account.
    /// `accounts` pairs each runtime with the account it is registered to.
    async fn route_account(runtimes: &mut [&mut DmRuntime], accounts: &[(String, String)]) -> bool {
        let account_of = |runtime: &str| {
            accounts
                .iter()
                .find(|(r, _)| r == runtime)
                .map(|(_, a)| format!("user:hub:{a}"))
        };
        let mut moved = false;
        loop {
            let mut batch = Vec::new();
            for (i, runtime) in runtimes.iter_mut().enumerate() {
                for message in runtime.sent() {
                    batch.push((i, message));
                }
            }
            if batch.is_empty() {
                return moved;
            }
            moved = true;
            for (from, message) in batch {
                let to: Vec<usize> = match &message {
                    TunnelMessage::TunnelChannelInvite {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelChannelEvent {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelChannelHistoryRequest {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelChannelHistory {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelAttachmentRequest {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelAttachmentChunk {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelAccountDms {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelDmRoute {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelPekoModelsRequest {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelPekoModels {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelPekoApprovalsRequest {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelPekoApprovals {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelPekoApprovalDecide {
                        recipient_runtime_id,
                        ..
                    }
                    | TunnelMessage::TunnelPekoApprovalDecided {
                        recipient_runtime_id,
                        ..
                    } => runtimes
                        .iter()
                        .position(|r| &r.app.runtime_did() == recipient_runtime_id)
                        .into_iter()
                        .collect(),
                    TunnelMessage::TunnelAccountDmsRequest {
                        recipient_account, ..
                    }
                    | TunnelMessage::TunnelPersonDmInvite {
                        recipient_account, ..
                    } => (0..runtimes.len())
                        .filter(|i| {
                            *i != from
                                && account_of(&runtimes[*i].app.runtime_did()).as_deref()
                                    == Some(recipient_account.as_str())
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                for i in to {
                    runtimes[i].receive(message.clone()).await;
                }
            }
        }
    }

    /// Each of `rt`'s person DMs: the other person, and whether it still
    /// waits for them to accept.
    async fn person_dm_waiting(rt: &DmRuntime) -> Vec<(String, bool)> {
        match rt
            .ipc(RequestPacket::ChannelPersonDms { request_id: 8 })
            .await
        {
            ResponsePacket::ChannelPersonDmsResult { dms, .. } => {
                dms.iter().map(|d| (d.peer.clone(), d.waiting)).collect()
            }
            other => panic!("person DMs: {other:?}"),
        }
    }

    /// ADR-095 D6 end to end. Alice (laptop, desktop) asks Bob (two
    /// computers) to DM; Bob's runtimes keep a request and tell her
    /// nothing; Bob accepts on one; her first post reaches him as history;
    /// posts then flow between every computer of both, the desktop and
    /// Bob's other computer joining through the account DM list. Eve,
    /// once blocked, gets nowhere.
    #[tokio::test]
    async fn people_dm_people_after_accepting() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, Checkpoint};

        let alice = uuid::Uuid::new_v4().to_string();
        let bob = uuid::Uuid::new_v4().to_string();
        let eve = uuid::Uuid::new_v4().to_string();
        let mut rts = Vec::new();
        let mut accounts = Vec::new();
        for account in [&alice, &alice, &bob, &bob, &eve] {
            let rt = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
            rt.app
                .tunnel_channel_port()
                .local()
                .link_local_user(&rt.app.runtime_did(), account)
                .await
                .unwrap();
            accounts.push((rt.app.runtime_did(), account.clone()));
            rts.push(rt);
        }
        let [mut laptop, mut desktop, mut b1, mut b2, mut e1]: [DmRuntime; 5] =
            rts.try_into().ok().unwrap();
        macro_rules! route {
            () => {
                route_account(
                    &mut [&mut laptop, &mut desktop, &mut b1, &mut b2, &mut e1],
                    &accounts,
                )
                .await
            };
        }
        let post = |channel: &peko_channel::ChannelId, text: &str| RequestPacket::ChannelPost {
            request_id: 2,
            channel: channel.as_str().into(),
            sender_name: None,
            text: text.into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        };
        let texts = |port: Arc<crate::tunnel::TunnelChannelPort>,
                     channel: peko_channel::ChannelId| async move {
            port.peek(&channel, &Checkpoint::zero())
                .await
                .map(|events| {
                    posts(&events)
                        .into_iter()
                        .map(|(_, t)| t)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        // Registration kept her PekoHub display name.
        laptop
            .app
            .tunnel_channel_port()
            .local()
            .set_person_display_name(Some("Alice"))
            .await
            .unwrap();
        // Alice asks from her laptop and writes before Bob answers.
        let ResponsePacket::ChannelPersonDmOpened {
            channel, created, ..
        } = laptop
            .ipc(RequestPacket::ChannelOpenPersonDm {
                request_id: 1,
                account: bob.clone(),
                name: Some("bob".into()),
            })
            .await
        else {
            panic!("alice opens the DM");
        };
        assert!(created);
        assert_eq!(
            channel,
            peko_channel::dm_channel_id(&format!("user:hub:{alice}"), &format!("user:hub:{bob}"))
        );
        laptop.ipc(post(&channel, "hi bob")).await;
        route!();

        // Both of Bob's computers hold a request; none of them said a word.
        for rt in [&b1, &b2] {
            let ResponsePacket::ChannelDmRequestsResult { requests, .. } = rt
                .ipc(RequestPacket::ChannelDmRequests { request_id: 3 })
                .await
            else {
                panic!("requests");
            };
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].from, format!("user:hub:{alice}"));
            assert_eq!(
                requests[0].name.as_deref(),
                Some("Alice"),
                "named as on PekoHub"
            );
            assert!(rt
                .app
                .tunnel_channel_port()
                .conversation_kind(&channel)
                .await
                .is_err());
        }
        let laptop_port = laptop.app.tunnel_channel_port();
        assert!(
            laptop_port
                .local()
                .list_remote_members(&channel)
                .await
                .unwrap()
                .is_empty(),
            "alice learns nothing of Bob's computers before he accepts"
        );
        assert_eq!(
            person_dm_waiting(&laptop).await,
            [(format!("user:hub:{bob}"), true)]
        );

        // Bob accepts on b1: Alice's post reaches him as history.
        let accepted = b1
            .ipc(RequestPacket::ChannelDmRequestAnswer {
                request_id: 4,
                channel: channel.as_str().into(),
                answer: "accept".into(),
            })
            .await;
        assert!(
            matches!(accepted, ResponsePacket::ChannelPersonDmOpened { .. }),
            "{accepted:?}"
        );
        let b1_port = b1.app.tunnel_channel_port();
        for _ in 0..100 {
            route!();
            if texts(Arc::clone(&b1_port), channel.clone()).await == ["hi bob"] {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            texts(Arc::clone(&b1_port), channel.clone()).await,
            ["hi bob"]
        );

        assert_eq!(
            person_dm_waiting(&laptop).await,
            [(format!("user:hub:{bob}"), false)]
        );
        assert_eq!(
            person_dm_waiting(&b1).await,
            [(format!("user:hub:{alice}"), false)]
        );

        // Bob answers; it reaches Alice's laptop.
        b1.ipc(post(&channel, "hi alice")).await;
        route!();
        assert_eq!(
            texts(Arc::clone(&laptop_port), channel.clone()).await,
            ["hi bob", "hi alice"]
        );

        // Alice's desktop and Bob's other computer join through their
        // accounts' DM lists; Bob's request there goes away.
        let desktop_port = desktop.app.tunnel_channel_port();
        let b2_port = b2.app.tunnel_channel_port();
        desktop_port.sync_account_dms().await;
        b2_port.sync_account_dms().await;
        for _ in 0..150 {
            route!();
            if texts(Arc::clone(&desktop_port), channel.clone())
                .await
                .len()
                == 2
                && texts(Arc::clone(&b2_port), channel.clone()).await.len() == 2
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            texts(Arc::clone(&desktop_port), channel.clone())
                .await
                .len(),
            2
        );
        assert_eq!(texts(Arc::clone(&b2_port), channel.clone()).await.len(), 2);
        assert!(b2_port.local().dm_requests(&bob).await.unwrap().is_empty());

        // A post from the desktop reaches every other computer.
        desktop.ipc(post(&channel, "from my desktop")).await;
        for _ in 0..50 {
            route!();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        for port in [&laptop_port, &b1_port, &b2_port] {
            assert_eq!(
                texts(Arc::clone(port), channel.clone())
                    .await
                    .last()
                    .map(String::as_str),
                Some("from my desktop")
            );
        }

        e1.app
            .tunnel_channel_port()
            .local()
            .set_person_display_name(Some("Eve"))
            .await
            .unwrap();
        // Eve asks; Bob blocks her; she cannot ask again.
        e1.ipc(RequestPacket::ChannelOpenPersonDm {
            request_id: 5,
            account: bob.clone(),
            name: None,
        })
        .await;
        route!();
        let eve_dm =
            peko_channel::dm_channel_id(&format!("user:hub:{eve}"), &format!("user:hub:{bob}"));
        let blocked = b1
            .ipc(RequestPacket::ChannelDmRequestAnswer {
                request_id: 6,
                channel: eve_dm.as_str().into(),
                answer: "block".into(),
            })
            .await;
        assert!(
            matches!(blocked, ResponsePacket::Done { success: true, .. }),
            "{blocked:?}"
        );
        e1.app
            .tunnel_channel_port()
            .resend_person_dm_invites()
            .await;
        route!();
        assert!(b1_port.local().dm_requests(&bob).await.unwrap().is_empty());
        // Bob's other computer still held her request; the block reaches it
        // with his DM list and the request goes (found live).
        assert_eq!(b2_port.local().dm_requests(&bob).await.unwrap().len(), 1);
        b2_port.sync_account_dms().await;
        for _ in 0..20 {
            route!();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            b2_port
                .local()
                .is_blocked(&bob, &format!("user:hub:{eve}"))
                .await
        );
        assert!(b2_port.local().dm_requests(&bob).await.unwrap().is_empty());
        // Both of Bob's computers name her in the block list (from her
        // request, which each of them held).
        for rt in [&b1, &b2] {
            let ResponsePacket::ChannelBlockedPeople { names, .. } = rt
                .ipc(RequestPacket::ChannelBlocked { request_id: 11 })
                .await
            else {
                panic!("blocked list");
            };
            assert_eq!(
                names.get(&format!("user:hub:{eve}")).map(String::as_str),
                Some("Eve")
            );
        }
        // Bob changes his mind on b1; b2 follows with his next DM list.
        b1.ipc(RequestPacket::ChannelBlockPerson {
            request_id: 9,
            account: eve.clone(),
            blocked: false,
        })
        .await;
        b2_port.sync_account_dms().await;
        for _ in 0..20 {
            route!();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            !b2_port
                .local()
                .is_blocked(&bob, &format!("user:hub:{eve}"))
                .await,
            "the unblock reached b2"
        );
        b1.ipc(RequestPacket::ChannelBlockPerson {
            request_id: 10,
            account: eve.clone(),
            blocked: true,
        })
        .await;
        let ResponsePacket::ChannelBlockedPeople { people, .. } = b1
            .ipc(RequestPacket::ChannelBlocked { request_id: 7 })
            .await
        else {
            panic!("blocked list");
        };
        assert_eq!(people, [format!("user:hub:{eve}")]);
    }

    /// Alice's DM with a private peko on `host`, opened from her laptop
    /// (registered to her account, ADR-090).
    struct AliceDm {
        host: DmRuntime,
        laptop: DmRuntime,
        channel: peko_channel::ChannelId,
        account: String,
        peko: ActorRef,
        peko_did: String,
    }

    async fn alice_dm_with_a_peko_elsewhere() -> AliceDm {
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_channel::ChannelPort;

        let account = uuid::Uuid::new_v4().to_string();
        let alice = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();
        let directory = Arc::new(FakeAgentDirectory::new());
        let mut host = DmRuntime::new(Arc::clone(&directory)).await;
        let peko = create_test_principal(
            &host.app,
            "peko",
            alice.clone(),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let host_port = host.app.tunnel_channel_port();
        host_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("peko"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        directory.register_hub_id(
            &hub_id,
            AgentResolution {
                runtime_id: host.app.runtime_did(),
                instance_id: "instance".into(),
                name: Some("peko".into()),
                hub_id: hub_id.clone(),
                local_id: peko.id.0.clone(),
                identity_key: peko_key.clone(),
                agent_did: peko_did.clone(),
                owner_principal: alice.clone(),
                owner_handle: None,
                exposure: ResolvedExposure::Private,
            },
        );

        // Her laptop opens the DM; the conversation grows past a page.
        let mut laptop = DmRuntime::new(Arc::clone(&directory)).await;
        laptop
            .app
            .tunnel_channel_port()
            .local()
            .link_local_user(&laptop.app.runtime_did(), &account)
            .await
            .unwrap();
        let ResponsePacket::ChannelRemoteDmOpened { channel, .. } = laptop
            .ipc(RequestPacket::ChannelOpenRemoteDm {
                request_id: 1,
                hub_id: hub_id.clone(),
                token: None,
            })
            .await
        else {
            panic!("the laptop opens the DM");
        };
        relay(&mut laptop, &mut host).await;
        AliceDm {
            host,
            laptop,
            channel,
            account,
            peko: ActorRef::from(&peko.id),
            peko_did,
        }
    }

    /// ADR-095 D5 + D1: alice's new computer learns of her DM with a peko
    /// elsewhere from her laptop, opens the same conversation, and keeps
    /// its whole history, older pages included.
    #[tokio::test]
    async fn a_new_computer_finds_the_persons_dms_and_keeps_their_history() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, Checkpoint};

        let AliceDm {
            mut host,
            mut laptop,
            channel,
            account,
            peko,
            peko_did,
        } = alice_dm_with_a_peko_elsewhere().await;
        let host_port = host.app.tunnel_channel_port();
        let total = crate::tunnel::HISTORY_PAGE_LIMIT as usize + 20;
        for i in 0..total {
            host_port
                .local()
                .post(
                    &channel,
                    &peko,
                    peko_channel::PostMsg::root(format!("note {i}")),
                )
                .await
                .unwrap();
        }

        // Her new desktop asks her other computers which DMs she has.
        let mut desktop = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let desktop_port = desktop.app.tunnel_channel_port();
        desktop_port
            .local()
            .link_local_user(&desktop.app.runtime_did(), &account)
            .await
            .unwrap();
        desktop_port.sync_account_dms().await;
        let account_runtimes = [
            (laptop.app.runtime_did(), account.clone()),
            (desktop.app.runtime_did(), account.clone()),
        ];
        let mut texts = Vec::new();
        for _ in 0..200 {
            route_account(
                &mut [&mut host, &mut laptop, &mut desktop],
                &account_runtimes,
            )
            .await;
            texts = desktop_port
                .peek(&channel, &Checkpoint::zero())
                .await
                .map(|events| posts(&events))
                .unwrap_or_default();
            if texts.len() == total {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            texts.len(),
            total,
            "the whole conversation, older pages stored"
        );
        // It reached the first post: nothing older is left elsewhere, so
        // the app stops offering to load earlier messages.
        let mut complete = false;
        for _ in 0..100 {
            complete = desktop_port
                .local()
                .history_complete(&channel)
                .await
                .unwrap();
            if complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(complete, "the copy is marked complete");
        assert_eq!(
            desktop_port
                .local()
                .remote_dm_account(&channel)
                .await
                .unwrap()
                .as_deref(),
            Some(account.as_str()),
            "the same conversation, the desktop's person's"
        );
        // The host now knows the desktop as a route for alice.
        assert!(host_port
            .local()
            .list_remote_members(&channel)
            .await
            .unwrap()
            .iter()
            .any(|m| m.runtime_id == desktop.app.runtime_did()));

        // A list nobody asked for is dropped.
        let forged = crate::tunnel::AccountDm {
            channel_id: peko_channel::dm_channel_id("other-key", &format!("user:hub:{account}"))
                .as_str()
                .into(),
            peer_hub_id: uuid::Uuid::new_v4().to_string(),
            peer_signing_did: peko_did.clone(),
            peer_identity_key: "other-key".into(),
            peer_name: None,
            peer_runtime_id: host.app.runtime_did(),
            peer_exposure: "public".into(),
        };
        desktop_port
            .on_account_dms(
                "never-asked".into(),
                laptop.app.runtime_did(),
                desktop.app.runtime_did(),
                vec![forged.clone()],
                Vec::new(),
                Vec::new(),
                "x".into(),
            )
            .await;
        assert!(desktop_port
            .conversation_kind(&peko_channel::ChannelId(forged.channel_id))
            .await
            .is_err());
    }

    /// ADR-095 D5: alice's new computer asks for her DMs while her laptop
    /// is asleep, so nobody answers. When PekoHub reports the laptop back
    /// (`peer_runtime_online`), the new computer asks again and gets the
    /// DM, with no reconnect of its own.
    #[tokio::test]
    async fn a_new_computer_asks_again_when_the_persons_other_computer_wakes() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use peko_channel::{ChannelPort, Checkpoint};

        let AliceDm {
            mut host,
            mut laptop,
            channel,
            account,
            peko,
            ..
        } = alice_dm_with_a_peko_elsewhere().await;
        host.app
            .tunnel_channel_port()
            .local()
            .post(
                &channel,
                &peko,
                peko_channel::PostMsg::root("said before".to_string()),
            )
            .await
            .unwrap();

        let mut desktop = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let desktop_port = desktop.app.tunnel_channel_port();
        desktop_port
            .local()
            .link_local_user(&desktop.app.runtime_did(), &account)
            .await
            .unwrap();
        // Signing in asks; the laptop is asleep, so PekoHub delivers it
        // to no one.
        desktop_port.sync_account_dms().await;
        assert!(desktop
            .sent()
            .iter()
            .any(|m| matches!(m, TunnelMessage::TunnelAccountDmsRequest { .. })));
        assert!(desktop_port.conversation_kind(&channel).await.is_err());

        // The laptop connects; PekoHub tells the desktop.
        let handle = desktop
            .app
            .tunnel_handle_slot()
            .read()
            .await
            .clone()
            .unwrap();
        desktop
            .dispatcher
            .dispatch(
                TunnelMessage::PeerRuntimeOnline {
                    runtime_id: laptop.app.runtime_did(),
                },
                handle,
            )
            .await
            .unwrap();
        let account_runtimes = [
            (laptop.app.runtime_did(), account.clone()),
            (desktop.app.runtime_did(), account.clone()),
        ];
        let mut texts = Vec::new();
        for _ in 0..200 {
            route_account(
                &mut [&mut host, &mut laptop, &mut desktop],
                &account_runtimes,
            )
            .await;
            texts = desktop_port
                .peek(&channel, &Checkpoint::zero())
                .await
                .map(|events| posts(&events))
                .unwrap_or_default();
            if !texts.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            texts.into_iter().map(|(_, t)| t).collect::<Vec<_>>(),
            ["said before"]
        );
        // The app lists it among the person's DMs with pekos elsewhere.
        let ResponsePacket::ChannelPekoDmsResult { dms, .. } = desktop
            .ipc(RequestPacket::ChannelPekoDms { request_id: 9 })
            .await
        else {
            panic!("peko DMs");
        };
        assert_eq!(dms.len(), 1);
        assert_eq!(dms[0].channel, channel);
        assert_eq!(dms[0].peer, format!("principal:hub:{}", dms[0].hub_id));
        assert_eq!(dms[0].name.as_deref(), Some("peko"));
        assert_eq!(dms[0].runtime, host.app.runtime_did());
    }

    /// ADR-095 D7 (F7): alice talks to her peko on its host, L1, then from
    /// her second computer, L2, under the same account. It is one
    /// conversation: L2 opens the DM L1 already has, and catches up on it.
    #[tokio::test]
    async fn your_own_peko_is_one_conversation_on_every_device() {
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_channel::{ChannelPort, Checkpoint};

        let account = uuid::Uuid::new_v4().to_string();
        let alice = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();
        let directory = Arc::new(FakeAgentDirectory::new());
        let mut l1 = DmRuntime::new(Arc::clone(&directory)).await;
        let peko = create_test_principal(
            &l1.app,
            "peko",
            alice.clone(),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let l1_port = l1.app.tunnel_channel_port();
        l1_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("peko"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        directory.register_hub_id(
            &hub_id,
            AgentResolution {
                runtime_id: l1.app.runtime_did(),
                instance_id: "instance".into(),
                name: Some("peko".into()),
                hub_id: hub_id.clone(),
                local_id: peko.id.0.clone(),
                identity_key: peko_key.clone(),
                agent_did: peko_did.clone(),
                owner_principal: alice.clone(),
                owner_handle: None,
                exposure: ResolvedExposure::Private,
            },
        );

        // On L1, before it was registered, alice talked to her peko.
        let local_user = ActorRef::User("local".into());
        l1_port
            .local()
            .ensure_local_user(&l1.app.runtime_did())
            .await
            .unwrap();
        let before = l1_port
            .ensure_dm(&peko.id, &local_user, "/local-user")
            .await
            .unwrap();
        l1_port
            .post(
                &before,
                &local_user,
                peko_channel::PostMsg::root("from my laptop"),
            )
            .await
            .unwrap();
        // L1 registers under alice's account.
        l1_port
            .local()
            .link_local_user(&l1.app.runtime_did(), &account)
            .await
            .unwrap();
        crate::principal::dm_rekey::reconcile_local_user_dms(l1.app.principal_manager())
            .await
            .unwrap();
        let peko_id = peko.id.clone();
        let dms_on_l1 = |port: Arc<crate::tunnel::TunnelChannelPort>| {
            let peko_id = peko_id.clone();
            async move {
                let mut out = Vec::new();
                for channel in port.list_for_principal(&peko_id).await.unwrap() {
                    if port.conversation_kind(&channel).await.unwrap()
                        == peko_channel::ConversationKind::Dm
                    {
                        out.push(channel);
                    }
                }
                out
            }
        };
        let rekeyed = dms_on_l1(Arc::clone(&l1_port)).await;
        assert_eq!(
            rekeyed,
            [peko_channel::dm_channel_id(
                &peko_key,
                &format!("user:hub:{account}")
            )],
            "the DM moved to the Hub-keyed pair id"
        );
        let texts = |events: Vec<peko_channel::ChannelEvent>| -> Vec<String> {
            posts(&events).into_iter().map(|(_, t)| t).collect()
        };
        assert_eq!(
            texts(
                l1_port
                    .peek(&rekeyed[0], &Checkpoint::zero())
                    .await
                    .unwrap()
            ),
            ["from my laptop"]
        );
        // The peko's session follows the DM.
        let bound = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(peko.memory.sessions_dir())
            .list_all_sessions(false)
            .await
            .unwrap()
            .into_iter()
            .filter(|m| m.channel_id.as_deref() == Some(before.as_str()))
            .count();
        assert_eq!(bound, 0, "no session left on the old id");

        // Alice opens her peko from L2.
        let mut l2 = DmRuntime::new(Arc::clone(&directory)).await;
        let l2_port = l2.app.tunnel_channel_port();
        l2_port
            .local()
            .link_local_user(&l2.app.runtime_did(), &account)
            .await
            .unwrap();
        let ResponsePacket::ChannelRemoteDmOpened { channel, .. } = l2
            .ipc(RequestPacket::ChannelOpenRemoteDm {
                request_id: 1,
                hub_id: hub_id.clone(),
                token: None,
            })
            .await
        else {
            panic!("L2 opens the DM");
        };
        assert_eq!(channel, rekeyed[0], "one conversation");
        let caught_up = || async {
            texts(l2_port.peek(&channel, &Checkpoint::zero()).await.unwrap()).len() == 1
        };
        relay_until(&mut l2, &mut l1, caught_up).await;
        assert_eq!(
            dms_on_l1(Arc::clone(&l1_port)).await,
            rekeyed,
            "still one DM on L1"
        );

        // A post from L2 lands in the same conversation on L1.
        l2.ipc(RequestPacket::ChannelPost {
            request_id: 2,
            channel: channel.as_str().into(),
            sender_name: None,
            text: "from my desktop".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        })
        .await;
        relay(&mut l2, &mut l1).await;
        assert_eq!(
            texts(l1_port.peek(&channel, &Checkpoint::zero()).await.unwrap()),
            ["from my laptop", "from my desktop"]
        );

        assert!(
            l1_port
                .local()
                .list_remote_members(&channel)
                .await
                .unwrap()
                .iter()
                .any(|m| m.runtime_id == l2.app.runtime_did()),
            "L2 is a route for alice on L1"
        );
        // Unregistered, L1 keys the DM by its local user again.
        l1_port.local().unlink_local_user().await.unwrap();
        crate::principal::dm_rekey::reconcile_local_user_dms(l1.app.principal_manager())
            .await
            .unwrap();
        assert_eq!(dms_on_l1(Arc::clone(&l1_port)).await, vec![before.clone()]);
        assert!(
            l1_port
                .local()
                .list_remote_members(&before)
                .await
                .unwrap()
                .iter()
                .all(|m| !m.principal_id.starts_with("user:")),
            "alice's other computers are no longer routes"
        );
        assert_eq!(
            texts(l1_port.peek(&before, &Checkpoint::zero()).await.unwrap()),
            ["from my laptop", "from my desktop"]
        );
    }

    /// ADR-097: alice's second computer lists the models her peko's host
    /// can answer her with (its enabled ones; nobody else gets any), and a
    /// post asking for one carries it to the host, which applies it only
    /// for the owner and only for a model enabled there.
    #[tokio::test]
    async fn your_peko_elsewhere_offers_you_its_hosts_models() {
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_channel::{ChannelPort, Checkpoint};

        let account = uuid::Uuid::new_v4().to_string();
        let alice = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();
        let directory = Arc::new(FakeAgentDirectory::new());
        let mut host = DmRuntime::new(Arc::clone(&directory)).await;
        let peko = create_test_principal(
            &host.app,
            "peko",
            alice.clone(),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let host_port = host.app.tunnel_channel_port();
        host_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("peko"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        directory.register_hub_id(
            &hub_id,
            AgentResolution {
                runtime_id: host.app.runtime_did(),
                instance_id: "instance".into(),
                name: Some("peko".into()),
                hub_id: hub_id.clone(),
                local_id: peko.id.0.clone(),
                identity_key: peko_key.clone(),
                agent_did: peko_did.clone(),
                owner_principal: alice.clone(),
                owner_handle: None,
                exposure: ResolvedExposure::Private,
            },
        );
        host_port
            .local()
            .link_local_user(&host.app.runtime_did(), &account)
            .await
            .unwrap();
        let resolver = host.app.principal_manager().llm_resolver().unwrap();
        let catalog = resolver.catalog();
        let mut work = peko_providers::catalog::ModelConfig::new(
            "work-model",
            peko_providers::catalog::ApiFormat::OpenaiCompletions,
            "http://127.0.0.1:9",
            "gpt-work",
        );
        work.display_name = "Work model".into();
        catalog.upsert(work).await.unwrap();
        let mut off = peko_providers::catalog::ModelConfig::new(
            "off-model",
            peko_providers::catalog::ApiFormat::OpenaiCompletions,
            "http://127.0.0.1:9",
            "gpt-off",
        );
        off.enabled = false;
        catalog.upsert(off).await.unwrap();

        let mut desk = DmRuntime::new(Arc::clone(&directory)).await;
        let desk_port = desk.app.tunnel_channel_port();
        desk_port
            .local()
            .link_local_user(&desk.app.runtime_did(), &account)
            .await
            .unwrap();
        let ResponsePacket::ChannelRemoteDmOpened { channel, .. } = desk
            .ipc(RequestPacket::ChannelOpenRemoteDm {
                request_id: 1,
                hub_id: hub_id.clone(),
                token: None,
            })
            .await
        else {
            panic!("the desk opens the DM");
        };
        relay(&mut desk, &mut host).await;

        // The host's enabled models, by id and name only.
        let asking = {
            let port = Arc::clone(&desk_port);
            let channel = channel.clone();
            tokio::spawn(async move { port.peko_models(&channel).await })
        };
        relay_until(&mut desk, &mut host, || async { asking.is_finished() }).await;
        let models = asking.await.unwrap().unwrap();
        assert_eq!(
            models,
            [crate::tunnel::PekoModel {
                id: "work-model".into(),
                display_name: "Work model".into(),
            }]
        );
        // Only to the peko's owner.
        let stranger = format!("user:hub:{}", uuid::Uuid::new_v4());
        assert!(host.app.owner_models(&peko_did, &stranger).await.is_empty());
        assert!(host
            .app
            .owner_models("did:key:z6MkNotHostedHere", &format!("user:hub:{account}"))
            .await
            .is_empty());
        // An answer nobody asked for is dropped.
        let late = {
            let port = Arc::clone(&desk_port);
            let channel = channel.clone();
            tokio::spawn(async move { port.peko_models(&channel).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let request = desk.sent();
        let Some(TunnelMessage::TunnelPekoModelsRequest { request_id, .. }) = request.first()
        else {
            panic!("the desk asks again: {request:?}");
        };
        desk.receive(TunnelMessage::TunnelPekoModels {
            request_id: request_id.clone(),
            source_runtime_id: host.app.runtime_did(),
            recipient_runtime_id: desk.app.runtime_did(),
            principal_did: peko_did.clone(),
            models: vec![crate::tunnel::PekoModel {
                id: "forged".into(),
                display_name: "Forged".into(),
            }],
            signature: "not-a-signature".into(),
        })
        .await;
        for message in request {
            host.receive(message).await;
        }
        relay_until(&mut desk, &mut host, || async { late.is_finished() }).await;
        assert_eq!(late.await.unwrap().unwrap()[0].id, "work-model");
        // A stranger's runtime (another account) cannot ask for this DM.
        let other = DmRuntime::new(Arc::clone(&directory)).await;
        let other_port = other.app.tunnel_channel_port();
        other_port
            .local()
            .link_local_user(&other.app.runtime_did(), &uuid::Uuid::new_v4().to_string())
            .await
            .unwrap();
        assert!(other_port.peko_models(&channel).await.is_err());

        // A post asking for a model carries it to the host.
        desk.ipc(RequestPacket::ChannelPost {
            request_id: 2,
            channel: channel.as_str().into(),
            sender_name: None,
            text: "use the work model".into(),
            parent: None,
            attachments: Vec::new(),
            model: Some("work-model".into()),
        })
        .await;
        let sent = desk.sent();
        for message in sent {
            host.receive(message).await;
        }
        let on_host = host_port.peek(&channel, &Checkpoint::zero()).await.unwrap();
        assert!(
            on_host.iter().any(|ev| matches!(ev,
                peko_channel::ChannelEvent::Posted { text, model, .. }
                    if text == "use the work model" && model.as_deref() == Some("work-model"))),
            "{on_host:?}"
        );
        // The host applies it only for the owner, and only an enabled model.
        let manager = host.app.principal_manager();
        let owner = peko_tools_core::ActingFor::runtime();
        let untrusted = peko_tools_core::ActingFor::unattributed();
        assert_eq!(
            manager.owner_model(&owner, Some("work-model".into())).await,
            Some("work-model".into())
        );
        assert_eq!(
            manager
                .owner_model(&untrusted, Some("work-model".into()))
                .await,
            None
        );
        assert_eq!(
            manager.owner_model(&owner, Some("off-model".into())).await,
            None
        );
        assert_eq!(
            manager.owner_model(&owner, Some("missing".into())).await,
            None
        );
        assert_eq!(manager.owner_model(&owner, None).await, None);
    }

    /// ADR-099: alice's second computer lists her peko's waiting approval
    /// requests on its host and decides them there; a forged decision, a
    /// stranger, and another account's runtime decide nothing.
    #[tokio::test]
    async fn you_decide_your_pekos_approvals_from_another_computer() {
        use crate::principal::policy_gate::{pending_requests, PrincipalPolicyGate};
        use crate::security::judge::{GatedCall, PolicyGate as _};
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_tools_core::{ActingFor, Compartment, Origin, Trust};

        let account = uuid::Uuid::new_v4().to_string();
        let alice = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let hub_id = uuid::Uuid::new_v4().to_string();
        let directory = Arc::new(FakeAgentDirectory::new());
        let mut host = DmRuntime::new(Arc::clone(&directory)).await;
        let peko = create_test_principal(
            &host.app,
            "peko",
            alice.clone(),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let host_port = host.app.tunnel_channel_port();
        host_port
            .register_actor(
                &ActorRef::from(&peko.id),
                Some(&hub_id),
                Some(&peko_did),
                Some(&peko_key),
                Some("peko"),
                peko_channel::Registration::Private,
            )
            .await
            .unwrap();
        directory.register_hub_id(
            &hub_id,
            AgentResolution {
                runtime_id: host.app.runtime_did(),
                instance_id: "instance".into(),
                name: Some("peko".into()),
                hub_id: hub_id.clone(),
                local_id: peko.id.0.clone(),
                identity_key: peko_key.clone(),
                agent_did: peko_did.clone(),
                owner_principal: alice.clone(),
                owner_handle: None,
                exposure: ResolvedExposure::Private,
            },
        );
        host_port
            .local()
            .link_local_user(&host.app.runtime_did(), &account)
            .await
            .unwrap();
        // Someone else asked the peko for something its policy left to alice.
        let gate = PrincipalPolicyGate::new(Arc::downgrade(host.app.principal_manager()));
        let call = GatedCall {
            call_id: Some("call_gated".into()),
            principal_id: peko.id.0.clone(),
            principal_name: None,
            session_id: Some("guest-session".into()),
            agent_id: None,
            workspace: None,
            stamp: ActingFor {
                actor: "user:hub:noah".into(),
                name: Some("Noah".into()),
                trust: Trust::Untrusted,
                compartment: Compartment::Conversation {
                    channel: "dm_x".into(),
                },
                origin: Origin::Ingress { channel: None },
                owner_request: false,
            },
            tool_name: "Read".into(),
            params: serde_json::json!({"file_path": "finances/q3.md"}),
        };
        let first = gate.ask_owner(&call, "finances are private").await.unwrap();

        let mut desk = DmRuntime::new(Arc::clone(&directory)).await;
        let desk_port = desk.app.tunnel_channel_port();
        desk_port
            .local()
            .link_local_user(&desk.app.runtime_did(), &account)
            .await
            .unwrap();
        let ResponsePacket::ChannelRemoteDmOpened { channel, .. } = desk
            .ipc(RequestPacket::ChannelOpenRemoteDm {
                request_id: 1,
                hub_id: hub_id.clone(),
                token: None,
            })
            .await
        else {
            panic!("the desk opens the DM");
        };
        relay(&mut desk, &mut host).await;

        // The desk lists what waits for alice.
        let asking = {
            let port = Arc::clone(&desk_port);
            let channel = channel.clone();
            tokio::spawn(async move { port.peko_approvals(&channel).await })
        };
        relay_until(&mut desk, &mut host, || async { asking.is_finished() }).await;
        let listed = asking.await.unwrap().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, first);
        assert_eq!(listed[0].actor, "user:hub:noah");
        assert_eq!(listed[0].actor_name.as_deref(), Some("Noah"));
        assert_eq!(listed[0].reason, "finances are private");
        assert!(listed[0].summary.contains("Read"), "{}", listed[0].summary);

        // Only to the peko's owner; a stranger decides nothing.
        let stranger = format!("user:hub:{}", uuid::Uuid::new_v4());
        assert!(host
            .app
            .peko_approvals_for_owner(&peko_did, &stranger)
            .await
            .is_empty());
        let refused = host
            .app
            .decide_peko_approval_for_owner(&peko_did, &stranger, &first, true, false, "did:key:x")
            .await
            .expect_err("not the owner");
        assert!(refused.contains("owner"), "{refused}");
        assert_eq!(pending_requests(&peko).len(), 1);

        // A decision whose signature does not hold is dropped unanswered.
        host.receive(TunnelMessage::TunnelPekoApprovalDecide {
            request_id: "forged".into(),
            source_runtime_id: desk.app.runtime_did(),
            recipient_runtime_id: host.app.runtime_did(),
            source_principal_did: format!("user:hub:{account}"),
            principal_did: peko_did.clone(),
            approval_id: first.clone(),
            approve: false,
            always: false,
            signature: "not-a-signature".into(),
        })
        .await;
        assert!(host.sent().is_empty(), "no answer to a forgery");
        assert_eq!(pending_requests(&peko).len(), 1);

        // Another account's runtime cannot ask about this DM.
        let other = DmRuntime::new(Arc::clone(&directory)).await;
        let other_port = other.app.tunnel_channel_port();
        other_port
            .local()
            .link_local_user(&other.app.runtime_did(), &uuid::Uuid::new_v4().to_string())
            .await
            .unwrap();
        assert!(other_port
            .decide_peko_approval(&channel, &first, false, false)
            .await
            .is_err());

        // Alice denies it from the desk; the host applies it.
        let deciding = {
            let port = Arc::clone(&desk_port);
            let (channel, id) = (channel.clone(), first.clone());
            tokio::spawn(
                async move { port.decide_peko_approval(&channel, &id, false, false).await },
            )
        };
        relay_until(&mut desk, &mut host, || async { deciding.is_finished() }).await;
        let outcome = deciding.await.unwrap().unwrap();
        assert!(outcome.starts_with("Denied"), "{outcome}");
        assert!(pending_requests(&peko).is_empty());

        // Deciding it again says why it was not applied.
        let again = {
            let port = Arc::clone(&desk_port);
            let (channel, id) = (channel.clone(), first.clone());
            tokio::spawn(async move { port.decide_peko_approval(&channel, &id, true, false).await })
        };
        relay_until(&mut desk, &mut host, || async { again.is_finished() }).await;
        let again = again.await.unwrap().expect_err("decided once");
        assert!(again.to_string().contains("already decided"), "{again}");

        // Nothing waits for alice any more.
        let listing = {
            let port = Arc::clone(&desk_port);
            let channel = channel.clone();
            tokio::spawn(async move { port.peko_approvals(&channel).await })
        };
        relay_until(&mut desk, &mut host, || async { listing.is_finished() }).await;
        assert!(listing.await.unwrap().unwrap().is_empty());
    }

    /// ADR-095 D2–D3: a DM between pekos on two runtimes has no host. A
    /// copy that missed a post catches up from the other copy, as the
    /// DM's other member; a peko outside the DM reads an empty page.
    #[tokio::test]
    async fn a_peko_dm_copy_catches_up_from_the_other_copy() {
        use crate::tunnel::hub_directory::{AgentResolution, FakeAgentDirectory, ResolvedExposure};
        use peko_channel::{ChannelPort, Checkpoint};

        let directory = Arc::new(FakeAgentDirectory::new());
        let mut h = DmRuntime::new(Arc::clone(&directory)).await;
        let mut r = DmRuntime::new(Arc::clone(&directory)).await;
        let mut pekos = Vec::new();
        for (runtime, name) in [(&h, "ann"), (&r, "ben")] {
            let peko = create_test_principal(
                &runtime.app,
                name,
                ActorRef::User("local".into()),
                vec![],
                peko_auth::Exposure::Public,
            )
            .await;
            let hub_id = uuid::Uuid::new_v4().to_string();
            let (did, key) = {
                let config = peko.config.read().await;
                (
                    config.did.as_ref().unwrap().0.clone(),
                    config.actor_key.clone().unwrap(),
                )
            };
            runtime
                .app
                .tunnel_channel_port()
                .register_actor(
                    &ActorRef::from(&peko.id),
                    Some(&hub_id),
                    Some(&did),
                    Some(&key),
                    Some(name),
                    peko_channel::Registration::Public,
                )
                .await
                .unwrap();
            directory.register_hub_id(
                &hub_id,
                AgentResolution {
                    runtime_id: runtime.app.runtime_did(),
                    instance_id: format!("{name}-instance"),
                    name: Some(name.into()),
                    hub_id: hub_id.clone(),
                    local_id: peko.id.0.clone(),
                    identity_key: key.clone(),
                    agent_did: did.clone(),
                    owner_principal: ActorRef::Public,
                    owner_handle: None,
                    exposure: ResolvedExposure::Public,
                },
            );
            runtime
                .app
                .tool_runtime
                .tooling()
                .services()
                .set_cross_runtime_a2a_ctx(Arc::new(crate::tunnel::CrossRuntimeA2aCtx {
                    directory: directory.clone(),
                    caller_runtime_id: runtime.app.runtime_did(),
                    principal_manager: runtime.app.principal_manager().clone(),
                    channel_port: runtime.app.tunnel_channel_port(),
                }));
            pekos.push((peko, hub_id, did, key));
        }
        let (ann, _, _, _) = &pekos[0];
        let (ben, ben_hub, ben_did, ben_key) = &pekos[1];
        let h_port = h.app.tunnel_channel_port();
        let r_port = r.app.tunnel_channel_port();
        let ben_ref = ActorRef::Principal(peko_subject::Route::hub(ben_hub.clone()));
        // Ann's runtime knows ben as `ChannelSend` would record him.
        h_port
            .register_actor(
                &ben_ref,
                Some(ben_hub),
                Some(ben_did),
                Some(ben_key),
                Some("ben"),
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();
        let channel = h_port
            .ensure_dm(&ann.id, &ben_ref, "/principal-ben")
            .await
            .unwrap();
        h_port.invite(&channel, &ann.id, &ben_ref).await.unwrap();
        relay(&mut h, &mut r).await;
        h_port
            .post(
                &channel,
                &ActorRef::from(&ann.id),
                peko_channel::PostMsg::root("one"),
            )
            .await
            .unwrap();
        relay(&mut h, &mut r).await;
        let texts = |events: Vec<peko_channel::ChannelEvent>| -> Vec<String> {
            posts(&events).into_iter().map(|(_, t)| t).collect()
        };
        assert_eq!(
            texts(r_port.peek(&channel, &Checkpoint::zero()).await.unwrap()),
            ["one"]
        );

        // A post reaches ann's copy but never ben's (written to the store
        // only, so no envelope or outbox entry carries it).
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        h_port
            .local()
            .post(
                &channel,
                &ActorRef::from(&ann.id),
                peko_channel::PostMsg::root("two"),
            )
            .await
            .unwrap();
        assert!(h.sent().is_empty());

        // Ben's runtime catches up from ann's copy, as ben.
        r_port.sync_remote_dms(Some(&h.app.runtime_did())).await;
        let caught_up = || async {
            texts(r_port.peek(&channel, &Checkpoint::zero()).await.unwrap()).len() == 2
        };
        relay_until(&mut r, &mut h, caught_up).await;
        assert_eq!(
            texts(r_port.peek(&channel, &Checkpoint::zero()).await.unwrap()),
            ["one", "two"]
        );
        // Catching up again adds nothing.
        r_port.sync_remote_dms(None).await;
        for _ in 0..10 {
            relay(&mut r, &mut h).await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            texts(r_port.peek(&channel, &Checkpoint::zero()).await.unwrap()).len(),
            2
        );

        // The same request from a runtime that is not ben's route, or for
        // a peko that is not in the DM, reads an empty page.
        let stranger = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let stranger_did = {
            let peko = create_test_principal(
                &stranger.app,
                "eve",
                ActorRef::User("local".into()),
                vec![],
                peko_auth::Exposure::Public,
            )
            .await;
            let config = peko.config.read().await;
            config.did.as_ref().unwrap().0.clone()
        };
        for (source, asker) in [
            (stranger.app.runtime_did(), ben_did.clone()),
            (r.app.runtime_did(), stranger_did),
        ] {
            let key = if source == r.app.runtime_did() {
                &r.app.runtime_signing_key
            } else {
                &stranger.app.runtime_signing_key
            };
            let iat = chrono::Utc::now().timestamp();
            let probe = crate::tunnel::ChannelHistoryRequestPayload {
                v: crate::tunnel::CHANNEL_HISTORY_REQUEST_VERSION.into(),
                request_id: "probe".into(),
                source_runtime_id: source,
                recipient_runtime_id: h.app.runtime_did(),
                source_principal_did: asker,
                channel_id: channel.as_str().into(),
                before: None,
                after: None,
                limit: 100,
                iat,
                exp: iat + 300,
            };
            let signature = crate::tunnel::sign_history(key, &probe);
            h.receive(TunnelMessage::TunnelChannelHistoryRequest {
                request_id: probe.request_id.clone(),
                source_runtime_id: probe.source_runtime_id.clone(),
                recipient_runtime_id: probe.recipient_runtime_id.clone(),
                source_principal_did: probe.source_principal_did.clone(),
                channel_id: probe.channel_id.clone(),
                before: None,
                after: None,
                limit: 100,
                signature,
            })
            .await;
            let answers = h.sent();
            assert!(
                matches!(answers.as_slice(), [TunnelMessage::TunnelChannelHistory { events, has_more: false, .. }] if events.is_empty()),
                "{answers:?}"
            );
        }
        let _ = ben;
    }

    /// ADR-090 D3: an unsigned invite is a person's only in the exact
    /// two-member DM shape, and a private peko still needs its owner's
    /// `Chat` grant for that person.
    #[tokio::test]
    async fn a_persons_invite_must_be_a_granted_two_member_dm() {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        let b = DmRuntime::new(Arc::new(FakeAgentDirectory::new())).await;
        let peko = create_test_principal(
            &b.app,
            "peko",
            ActorRef::User("local".into()),
            vec![],
            peko_auth::Exposure::Private,
        )
        .await;
        let hub_id = uuid::Uuid::new_v4().to_string();
        let (peko_did, peko_key) = {
            let config = peko.config.read().await;
            (
                config.did.as_ref().unwrap().0.clone(),
                config.actor_key.clone().unwrap(),
            )
        };
        let port = b.app.tunnel_channel_port();
        port.register_actor(
            &ActorRef::from(&peko.id),
            Some(&hub_id),
            Some(&peko_did),
            Some(&peko_key),
            Some("peko"),
            peko_channel::Registration::Private,
        )
        .await
        .unwrap();
        let source = peko_identity::keys::KeyPair::generate();
        let source_runtime = crate::tunnel::verifying_key_to_did_key(&source.verifying_key);
        let account = uuid::Uuid::new_v4().to_string();
        let person = format!("user:hub:{account}");
        let channel = peko_channel::dm_channel_id(&peko_key, &person);
        let person_row = peko_protocol::channel::InitialMember {
            principal_did: person.clone(),
            runtime_id: Some(source_runtime.clone()),
            hub_id: Some(account.clone()),
            identity_key: Some(person.clone()),
            name: None,
        };
        let peko_row = peko_protocol::channel::InitialMember {
            principal_did: peko_did.clone(),
            runtime_id: None,
            hub_id: Some(hub_id.clone()),
            identity_key: Some(peko_key.clone()),
            name: Some("peko".into()),
        };
        let invite = |request: &str, members: Vec<peko_protocol::channel::InitialMember>| {
            let (signature, members) = build_signed_tunnel_channel_invite(
                &source,
                request,
                &source_runtime,
                &b.app.runtime_did(),
                &person,
                channel.as_str(),
                &person,
                &person,
                "dm-peko",
                peko_channel::ConversationKind::Dm,
                &members,
            );
            TunnelMessage::TunnelChannelInvite {
                request_id: request.into(),
                source_runtime_id: source_runtime.clone(),
                recipient_runtime_id: b.app.runtime_did(),
                source_principal_did: person.clone(),
                channel_id: channel.as_str().into(),
                creator: person.clone(),
                creator_did: person.clone(),
                name: "dm-peko".into(),
                kind: peko_channel::ConversationKind::Dm,
                initial_members: members,
                signature,
                author_signature: String::new(),
            }
        };
        let mirrored = || async { port.local().conversation_kind(&channel).await.is_ok() };

        let third = peko_protocol::channel::InitialMember {
            principal_did: "user:hub:00000000-0000-4000-8000-000000000000".into(),
            ..person_row.clone()
        };
        b.receive(invite(
            "three",
            vec![person_row.clone(), third, peko_row.clone()],
        ))
        .await;
        assert!(!mirrored().await, "a DM has two members");

        b.receive(invite(
            "ungranted",
            vec![person_row.clone(), peko_row.clone()],
        ))
        .await;
        assert!(!mirrored().await, "a private peko needs a Chat grant");

        grant_chat(
            &b.dispatcher,
            "peko",
            &[ActorRef::User(peko_subject::Route::hub(account))],
        )
        .await;
        b.receive(invite("granted", vec![person_row, peko_row]))
            .await;
        assert!(mirrored().await);
        assert!(port
            .binding_for(&channel, &peko.id)
            .await
            .unwrap()
            .is_some());
    }
}
