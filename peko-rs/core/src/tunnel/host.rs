//! Host port for the tunnel dispatcher (F5).
//!
//! Dependency-inversion seam: the tunnel (transport/protocol layer) must not
//! depend upward on `daemon` (the application shell). The dispatcher reaches
//! daemon services through this narrow trait; `daemon::state::AppState` is the
//! only type that implements it, and the dispatcher holds an
//! `Arc<dyn TunnelHost>`.
//!
//! The surface is exactly the operations the dispatcher needs, returned as
//! owned values so the trait is trivially object-safe. Boundary rule 9
//! (`src/tunnel/` must not import `crate::daemon`) keeps the seam from
//! regressing.

use std::sync::Arc;

use tokio::sync::RwLock;

use super::invite_token::InviteRevocationSet;
use super::TunnelChannelPort;
use super::TunnelHandle;
use crate::principal::PrincipalManager;
use peko_auth::jwt::JwtValidator;
use peko_observability::Observability;

/// Everything the host needs to bootstrap the local mirror of an
/// inbound `TunnelChannelInvite` (sprint 3 Phase 12a). Bundled as a
/// struct so the trait method stays readable; the dispatcher fills it
/// from the verified envelope.
#[derive(Debug, Clone)]
pub struct DmChannelInviteBootstrap {
    /// The channel id (`chan_<8 base36>`) the mirror is bootstrapped
    /// under.
    pub channel_id: String,
    /// The source-runtime-local creator id (display only — written
    /// into the mirror's `meta.json`).
    pub creator: String,
    /// The creator principal's stable DID. The receiver names its
    /// peer child for the creator from this (`principal:<did>`) —
    /// `creator` / `source_principal_did` are source-local ids and
    /// cannot support that.
    pub creator_did: String,
    /// Human-readable channel name snapshot.
    pub name: String,
    /// Source-keyed membership snapshot. The single row with
    /// `runtime_id: None` is the invitee row ("addressed to you")
    /// and carries the invited principal's DID — the host resolves
    /// WHICH local principal was invited from it.
    pub initial_members: Vec<peko_protocol::channel::InitialMember>,
    /// The source runtime's `did:key` (signature already verified by
    /// the dispatcher before this is called).
    pub source_runtime_id: String,
    /// DM marker. `Some(_)` means the channel is DM-tier on the
    /// source; the VALUE is ignored (the receiver derives its own
    /// binding from its own session tree — slug suffixes are
    /// runtime-local).
    pub passive_binding: Option<String>,
}

/// Narrow host interface the tunnel dispatcher uses to reach daemon services.
///
/// Implemented only by `daemon::state::AppState`. Production and tests hand
/// the dispatcher an `Arc<dyn TunnelHost>`.
#[async_trait::async_trait]
pub trait TunnelHost: Send + Sync {
    /// Principal manager used to list/lookup principals for announce + receive.
    fn principal_manager(&self) -> Arc<PrincipalManager>;

    /// This runtime's DID (used to derive stable instance IDs and audit tags).
    fn runtime_did(&self) -> String;

    /// Human-readable runtime display name for announce payloads.
    fn runtime_display_name(&self) -> String;

    // B5 cleanup: `runtime_direct_endpoint` was retired — direct
    // cross-runtime transport was removed (all cross-runtime traffic
    // flows through the tunnel relay). The trait accessor and all
    // impls are deleted.

    /// JWT validator for verifying PekoHub-proxied caller identity.
    fn jwt_validator(&self) -> Option<JwtValidator>;

    /// Observability handle for emitting audit events.
    fn observability(&self) -> Arc<Observability>;

    /// Slot the dispatcher writes the live outbound tunnel handle into on
    /// every inbound message, so the `CrossRuntimeA2aCtx` always sends on the
    /// freshest handle.
    fn tunnel_handle_slot(&self) -> Arc<RwLock<Option<TunnelHandle>>>;

    /// Runtime's ed25519 verifying key, derived from the same
    /// `runtime_signing_key` that mints invite tokens (PR #11). The
    /// dispatcher verifies inbound invite-token claims against this
    /// key. Returned as a `VerifyingKey` (cheap to clone) so the
    /// trait stays object-safe.
    fn runtime_verifying_key(&self) -> ed25519_dalek::VerifyingKey;

    /// In-memory revocation set for invite tokens. The dispatcher
    /// checks this alongside signature/expiry/principal-name before
    /// allowing a token to bypass the exposure-based ACL.
    fn invite_revocation_set(&self) -> Arc<InviteRevocationSet>;

    /// Cross-runtime channel port. The dispatcher's
    /// `handle_inbound_tunnel_channel_event` calls
    /// [`TunnelChannelPort::append_remote_event`] on this after
    /// verifying the envelope signature — peko-channel cross-runtime
    /// PR-B commit 2 wires this in. The `ChannelStore` underneath
    /// does the actual mirror append; `TunnelChannelPort` is just the
    /// typed port the dispatcher reaches through.
    fn tunnel_channel_port(&self) -> Arc<TunnelChannelPort>;

    /// Sprint 3 Phase 12a: bootstrap the local mirror for an inbound
    /// `TunnelChannelInvite` and give it a live subscriber. The
    /// dispatcher calls this AFTER the envelope signature verifies,
    /// in place of a bare `join_remote`.
    ///
    /// The host owns the parts the tunnel layer cannot know:
    ///
    /// - WHICH local principal was invited (the invitee row's DID →
    ///   the principal registry);
    /// - for DM invites (`passive_binding: Some`), the receiver-local
    ///   peer child for `principal:<creator_did>` (child-only ensure
    ///   — no local-only DM channel) and the `/​<slug>` binding
    ///   derived from that child's real slug;
    /// - the post-bootstrap `ChannelBindingSupervisor::ensure_subscriber`
    ///   kickoff, so the mirror's `PassiveBindingResponder` starts
    ///   immediately (closing the Phase 10 live-hook gap).
    ///
    /// Non-DM invites (`passive_binding: None`) bootstrap an unbound
    /// mirror and still get the subscriber (the meter-only `Noop`
    /// responder), matching the boot sweep's treatment of unbound
    /// channels. An invite whose invitee DID matches no loaded
    /// principal is logged and skipped (Ok) — invites are push-only.
    async fn dm_channel_mirror_bootstrap(
        &self,
        invite: DmChannelInviteBootstrap,
    ) -> anyhow::Result<()>;
}
