//! `TunnelChannelPort` — `ChannelPort` impl that adds cross-runtime
//! fan-out on top of a local `ChannelStore`.
//!
//! peko-channel cross-runtime PR-B commits 2 + 3. Commit 2 added
//! the skeleton + the inbound append path. Commit 3 adds the
//! outbound fan-out: `post` writes to the local store and then
//! pushes a signed `TunnelMessage::TunnelChannelEvent` to every
//! runtime that hosts a remote member of the channel.
//!
//! ## Why this lives in `peko-core` (not `peko-channel`)
//!
//! `TunnelChannelPort` reaches into [`crate::tunnel`] for the
//! `TunnelMessage` enum, the signing key, and the live
//! `TunnelHandle`. `peko-channel` is forbidden from depending on
//! `peko-core` per the workspace dep rules (see `peko-channel`'s
//! `Cargo.toml`), so the cross-runtime impl naturally lives here.
//! `peko-channel` keeps `ChannelPort` (the trait), `ChannelStore`
//! (the file-backed local impl), `RemoteMember` (the membership row
//! type), and `NoopChannelPort` (the test-only fallback).
//!
//! ## Inbound vs. outbound (commit scope)
//!
//! - **Inbound** (commit 2): the tunnel dispatcher calls
//!   [`TunnelChannelPort::append_remote_event`] after verifying the
//!   `TunnelChannelEvent` envelope signature. We delegate straight
//!   to the local store's
//!   [`peko_channel::ChannelStore::append_remote_event`] — same
//!   "skip local membership verification, trust upstream" semantics
//!   the store gained in commit 1.
//! - **Outbound** (commit 3): `post` iterates the channel's
//!   `remote_members`, groups them by `runtime_id`, and pushes a
//!   signed `TunnelChannelEvent` to each unique recipient runtime.
//!   Each event is signed with `ctx.signing_key` (the runtime's own
//!   `PekoHubCredential` key) so the recipient can verify against
//!   `source_runtime_id`. The `request_id` is a fresh UUIDv4 per
//!   fan-out so the hub can scope replay protection.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::{
    subject_wire_form, ChannelId, ChannelMembership, ChannelPort, Checkpoint, CreateOpts, PostMsg,
    Result,
};
use tokio::sync::RwLock;
use peko_protocol::channel::ChannelEvent;
use peko_subject::{PrincipalId, Subject};
use tracing::warn;
use uuid::Uuid;

/// Type alias re-exported so the trait impl below reads naturally.
/// `peko_channel` defines `pub type TaskId = String;` in `port.rs` but
/// does not re-export it; aliasing here avoids touching
/// `peko_channel::lib.rs` for a public type that's already in
/// `peko_protocol::channel::ChannelEvent`'s wire form.
type TaskId = String;

use crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx;

/// Concrete `ChannelPort` impl that wraps a local [`ChannelStore`]
/// and (in commit 3) fans events out to the channel's remote
/// members over the tunnel.
///
/// `TunnelChannelPort` is `Send + Sync` via `Arc` clones (every field
/// is shared and cheap to clone). Construction is via
/// [`TunnelChannelPort::new`]; the cross-runtime ctx is set via
/// [`TunnelChannelPort::set_ctx`] once `AppState::install_cross_runtime_channel_ctx`
/// has built it (the directory HTTP client + signing-key + tunnel
/// handle slot aren't ready until the tunnel has been provisioned,
/// which happens after `AppState` construction).
#[derive(Clone)]
pub struct TunnelChannelPort {
    /// File-backed local mirror. All `ChannelPort` operations
    /// delegate here — `TunnelChannelPort` does not duplicate the
    /// append-only JSONL logic, it just adds cross-runtime routing
    /// around it.
    local: Arc<peko_channel::ChannelStore>,

    /// Slot for the cross-runtime dispatch context. `None` until
    /// `AppState::install_cross_runtime_channel_ctx` fills it in
    /// (post-construction, since the directory client + signing key
    /// aren't ready at `AppState::with_data_dir` time). The slot is
    /// an `Arc<RwLock<...>>` rather than a plain `Option` so the
    /// outbound `post` path can hot-reload it without rebuilding
    /// `TunnelChannelPort` (same pattern as `ctx.tunnel` for the
    /// live `TunnelHandle`).
    ctx: Arc<RwLock<Option<Arc<CrossRuntimeChannelCtx>>>>,
}

impl std::fmt::Debug for TunnelChannelPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelChannelPort")
            .field("local", &"<Arc<ChannelStore>>")
            .field("ctx", &"<Arc<RwLock<Option<...>>>>")
            .finish()
    }
}

impl TunnelChannelPort {
    /// Construct a `TunnelChannelPort` that wraps `local`. The cross-runtime
    /// ctx slot starts as `None`; call [`Self::set_ctx`] (typically from
    /// `AppState::install_cross_runtime_channel_ctx`) to enable outbound
    /// fan-out.
    #[must_use]
    pub fn new(local: Arc<peko_channel::ChannelStore>) -> Self {
        Self {
            local,
            ctx: Arc::new(RwLock::new(None)),
        }
    }

    /// Install the cross-runtime dispatch context. Idempotent: a
    /// later call replaces the prior ctx — used by the daemon's
    /// tunnel-reconnect path so a fresh `runtime_id` (after a
    /// credential rotation) propagates without rebuilding every
    /// `TunnelChannelPort`.
    pub async fn set_ctx(&self, ctx: Arc<CrossRuntimeChannelCtx>) {
        let mut guard = self.ctx.write().await;
        *guard = Some(ctx);
    }

    /// Borrow the wrapped local store (used by tests that want to
    /// assert on the on-disk layout directly).
    #[must_use]
    pub fn local(&self) -> &Arc<peko_channel::ChannelStore> {
        &self.local
    }

    /// Append a [`ChannelEvent`] that originated from a different
    /// runtime (the hub forwarded it via
    /// `TunnelMessage::TunnelChannelEvent`). Used by the tunnel
    /// dispatcher after it has verified the envelope signature.
    ///
    /// Delegates straight to [`peko_channel::ChannelStore::append_remote_event`]
    /// — same "skip local membership verification, trust upstream
    /// signature" semantics the store gained in commit 1. The
    /// cross-runtime wrapper intentionally does not re-verify or
    /// re-check; the dispatcher is the single source of truth for
    /// signature validity, and layering a second check here would
    /// either duplicate work or risk drift between the two checks.
    ///
    /// # Errors
    ///
    /// Surfaces [`ChannelError::NotFound`] when the local mirror
    /// directory does not exist for the given channel id (the
    /// channel must be `create`-d locally before inbound events can be
    /// appended — see the dispatcher's TODO comment history for why
    /// this is the right shape).
    pub async fn append_remote_event(
        &self,
        channel: &ChannelId,
        ev: &ChannelEvent,
    ) -> Result<TaskId> {
        self.local.append_remote_event(channel, ev).await
    }

    /// Borrow the wrapped cross-runtime ctx slot. Returns `None` if
    /// `set_ctx` has not been called yet (commit 2 default;
    /// commit 3 production wiring flips to `Some`).
    #[must_use]
    #[allow(dead_code)]
    pub(crate) async fn ctx(&self) -> Option<Arc<CrossRuntimeChannelCtx>> {
        self.ctx.read().await.clone()
    }

    // -----------------------------------------------------------------
    // Outbound fan-out (PR-B commit 3)
    // -----------------------------------------------------------------

    /// Build + sign + send one `TunnelChannelEvent` per **unique**
    /// recipient runtime of `channel`. Local-only channels (no
    /// remote members) are a no-op. Failures are aggregated and
    /// returned as a single string error so the caller can decide
    /// whether to log / escalate.
    ///
    /// `source` is the local subject who authored the event (the
    /// `sender` for `Posted`, the `invitee` / `inviter` for
    /// `MemberJoined`/`MemberLeft`). The signature pre-image always
    /// carries the runtime-level identity (`source_runtime_id`); the
    /// subject id is recorded for audit correlation on the receiving
    /// runtime. ADR-049 Phase 1: principal senders keep the legacy
    /// bare-id form here (via [`subject_wire_form`]) so the signed
    /// pre-image is byte-identical to the pre-ADR-049 shape; user
    /// senders (Phase 2+) take the canonical `user:<id>` form.
    async fn fanout_event(
        &self,
        channel: &ChannelId,
        ev: &ChannelEvent,
        source: &Subject,
    ) -> std::result::Result<(), String> {
        let ctx = match self.ctx.read().await.clone() {
            Some(c) => c,
            // No ctx wired → no cross-runtime fan-out (commit 2 default;
            // commit 3 flips production to `Some(...)`).
            None => return Ok(()),
        };

        // Read remote members from the local mirror.
        let remote_members = self
            .local
            .list_remote_members(channel)
            .await
            .map_err(|e| format!("list_remote_members failed: {e}"))?;
        if remote_members.is_empty() {
            // No remote subscribers — nothing to do. The local store
            // is authoritative for the channel.
            return Ok(());
        }

        // Group remote members by runtime id so we send one message
        // per recipient runtime (a single runtime can host multiple
        // remote channel members; the receiver still gets one
        // `MemberJoined` event per principal in a follow-up
        // `MemberJoined`-per-principal fan-out — see commit 3a).
        let unique_runtimes: HashSet<String> = remote_members
            .iter()
            .map(|rm| rm.runtime_id.clone())
            .collect();

        // Snapshot the live tunnel handle once. If the tunnel is not
        // currently connected, we skip the whole fan-out with a warn
        // (the next event or a tunnel reconnect will not retry —
        // channel events are push-only and the next fan-out will
        // pick up new state).
        let handle = {
            let guard = ctx.tunnel.read().await;
            guard.clone()
        };
        let handle = match handle {
            Some(h) => h,
            None => {
                warn!(
                    "outbound TunnelChannelEvent fan-out skipped: tunnel not connected \
                     (channel={}, remote runtimes={})",
                    channel.as_str(),
                    unique_runtimes.len()
                );
                return Ok(());
            }
        };

        // Serialize the event ONCE so the bytes signed are the bytes
        // the receiver will re-serialize and verify (serde_json
        // emits struct fields in declaration order, so the round-trip
        // is stable).
        let event_bytes = serde_json::to_vec(ev)
            .map_err(|e| format!("serialize ChannelEvent: {e}"))?;

        let mut failures: Vec<String> = Vec::new();
        let source_wire_id = subject_wire_form(source);
        for recipient_runtime_id in unique_runtimes {
            // Fresh request_id per recipient — the hub can scope
            // replay protection per request id and the audit rows
            // join on it (`forwarded_outbound` on the source side
            // ↔ `received_inbound` on each recipient).
            let request_id = Uuid::new_v4().to_string();
            let signed = crate::tunnel::ChannelSignedFields {
                request_id: &request_id,
                source_runtime_id: &ctx.caller_runtime_id,
                recipient_runtime_id: &recipient_runtime_id,
                source_principal_did: &source_wire_id,
                channel_id: channel.as_str(),
                event_bytes: &event_bytes,
            };
            let signature =
                crate::tunnel::sign_channel_event(&ctx.signing_key, signed);

            // Audit on the source runtime side. The receiver's
            // `received_inbound` audit row is emitted by the
            // dispatcher's inbound handler.
            let event_kind = ev.kind();
            let preview_json = String::from_utf8_lossy(&event_bytes).into_owned();
            crate::tunnel::emit_forwarded_outbound(
                &request_id,
                &ctx.caller_runtime_id,
                &source_wire_id,
                channel.as_str(),
                event_kind,
                &crate::tunnel::tunnel_channel_audit::preview_event_payload(
                    &preview_json,
                ),
            );

            let envelope = crate::tunnel::TunnelMessage::TunnelChannelEvent {
                request_id,
                source_runtime_id: ctx.caller_runtime_id.clone(),
                recipient_runtime_id: recipient_runtime_id.clone(),
                source_principal_did: source_wire_id.clone(),
                channel_id: channel.as_str().to_string(),
                event: ev.clone(),
                signature,
            };

            if let Err(e) = handle.send(envelope) {
                failures.push(format!(
                    "send to {recipient_runtime_id} failed: {e}"
                ));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    /// Add a remote-member row to the channel's `members.json` and
    /// persist it. Used by the outbound DM invite path
    /// ([`Self::fanout_dm_invite`]) to record the recipient before
    /// fan-out so [`Self::fanout_event`] sees the channel as
    /// cross-runtime and later posts actually leave the runtime.
    pub(crate) async fn add_remote_member(
        &self,
        channel: &ChannelId,
        runtime_id: &str,
        principal_id: &str,
    ) -> Result<()> {
        self.local
            .add_remote_member(channel, runtime_id, principal_id)
            .await
    }

    /// Outbound fan-out for a `ChannelPort::invite`: build + sign +
    /// send one `TunnelChannelInvite` envelope to the invitee's
    /// hosting runtime. Parallel to [`Self::fanout_event`] but for
    /// the invite bootstrap path (peko-channel cross-runtime PR-3a
    /// commit 3; DM-aware since sprint 3 Phase 12a).
    ///
    /// ## `invitee_runtime_id` resolution
    ///
    /// Today this is a "did:key:z" / `@<runtime-id>` suffix
    /// heuristic — sufficient for the test surface and for
    /// principals whose DID carries an `@<runtime>` suffix (the
    /// common cross-runtime naming convention). The full
    /// directory-backed lookup
    /// (`CrossRuntimeChannelCtx.directory.resolve_principal`)
    /// threads through here in a follow-up; until then an invitee
    /// with no `@<runtime-id>` suffix is treated as local and
    /// produces no envelope (matching the no-cross-runtime-invite
    /// default — the local `ChannelPort::invite` already recorded
    /// the `MemberJoined` row).
    ///
    /// ## Phase 12a additions
    ///
    /// - **The remote-member row is recorded FIRST.** Before the
    ///   envelope is built, the invitee is filed in the source's own
    ///   `members.json` as a `RemoteMember` (`runtime_id` = the
    ///   invitee's runtime, `principal_id` = the invitee's bare
    ///   id/DID with the `@<runtime>` suffix stripped). This is what
    ///   makes [`Self::fanout_event`] see the channel as
    ///   cross-runtime, so the source's later posts actually leave
    ///   the runtime. Local bookkeeping deliberately does NOT depend
    ///   on tunnel connectivity: the row is written before the
    ///   tunnel-handle check.
    /// - **`creator_did`** — the channel creator principal's stable
    ///   DID — joins the envelope (and its signed pre-image): the
    ///   receiver names its peer child for the creator from it
    ///   (`principal:<creator_did>`), which `creator` /
    ///   `source_principal_did` (source-local ids) cannot support.
    ///   The bare `ChannelPort::invite` path has no DID resolver on
    ///   the trait surface, so it passes the inviter's local id; the
    ///   DM provisioning path (Phase 12b) calls this method directly
    ///   with the real DID.
    /// - **DM marker.** The channel's own `passive_binding` (read
    ///   from `meta.json`) rides the envelope. Only its PRESENCE is
    ///   meaningful — the receiver derives its own `/​<slug>`
    ///   binding from its own session tree (slug collision suffixes
    ///   are runtime-local).
    /// - **Snapshot re-keying.** The port-owner's own membership
    ///   rows are emitted with `runtime_id:
    ///   Some(ctx.caller_runtime_id)` (from the receiver's view they
    ///   are remote); pre-existing remote rows keep their runtime;
    ///   the invitee row is emitted bare-DID with `runtime_id:
    ///   None` — "addressed to you, receiver". The receiver's
    ///   `join_remote` maps that row to its own local principal id.
    ///
    /// ## Failure mode
    ///
    /// Returns `Ok(())` if the invitee is local (no envelope to
    /// send) or if the tunnel handle is `None` (logged + dropped —
    /// the local invite + remote-member row already succeeded, so a
    /// reconnect-era post still fans out). Returns `Err(...)` if the
    /// envelope construction or `TunnelHandle::send` fails so the
    /// caller can decide to log / escalate.
    pub(crate) async fn fanout_dm_invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        creator_did: &str,
        invitee: &PrincipalId,
    ) -> std::result::Result<(), String> {
        // Resolve invitee → runtime_id via the @suffix convention.
        // No suffix → local invite only; no envelope to send.
        let Some(at) = invitee.0.rfind('@') else {
            return Ok(());
        };
        let invitee_runtime_id = invitee.0[at + 1..].to_string();
        // The invitee's identity as the REMOTE runtime knows it —
        // suffix stripped. Recorded as the remote-member row's
        // `principal_id` and emitted as the invitee row's DID.
        let invitee_bare = invitee.0[..at].to_string();

        // Self-invite → local only; no envelope to send.
        let ctx = match self.ctx.read().await.clone() {
            Some(c) => c,
            None => return Ok(()),
        };
        if invitee_runtime_id == ctx.caller_runtime_id {
            return Ok(());
        }

        // Record the remote-member row FIRST (Phase 12a): this is
        // what unwires the previously dead `add_remote_member` —
        // without it, `fanout_event` saw no remote members and every
        // post stayed local-only. Done before the tunnel-handle
        // check so a transient disconnect doesn't lose the routing
        // state.
        self.add_remote_member(channel, &invitee_runtime_id, &invitee_bare)
            .await
            .map_err(|e| {
                format!(
                    "add_remote_member failed (channel={}): {e}",
                    channel.as_str()
                )
            })?;

        // Snapshot the live tunnel handle once.
        let handle = {
            let guard = ctx.tunnel.read().await;
            guard.clone()
        };
        let handle = match handle {
            Some(h) => h,
            None => {
                warn!(
                    "outbound TunnelChannelInvite fan-out skipped: tunnel not connected \
                     (channel={}, invitee_runtime_id={invitee_runtime_id})",
                    channel.as_str(),
                );
                return Ok(());
            }
        };

        // Read meta.json for creator + name + the DM marker. We read
        // it directly via the store's `channel_dir` so a missing
        // meta surfaces as an error rather than silently using
        // defaults.
        let chan_dir = self.local.channel_dir(channel);
        let meta_bytes = match tokio::fs::read(chan_dir.join("meta.json")).await {
            Ok(b) => b,
            Err(e) => {
                return Err(format!(
                    "read meta.json for invite envelope (channel={}): {e}",
                    channel.as_str()
                ));
            }
        };
        #[derive(serde::Deserialize)]
        struct MetaSnapshot {
            creator: String,
            name: String,
            // Phase 12a: the DM marker. Absent on unbound channels
            // (`skip_serializing_if` on the store's write side).
            passive_binding: Option<String>,
        }
        let meta: MetaSnapshot = serde_json::from_slice(&meta_bytes).map_err(|e| {
            format!(
                "decode meta.json for invite envelope (channel={}): {e}",
                channel.as_str()
            )
        })?;

        // Build `initial_members` re-keyed to the receiver's view
        // (Phase 12a): the source's own rows are remote from the
        // receiver's side, so they carry `Some(caller_runtime_id)`;
        // pre-existing remote rows keep their runtime; the invitee
        // row carries `None` — "addressed to you". The invitee's
        // local row (written by `ChannelStore::invite` when the
        // caller came through the trait) and its just-recorded
        // remote row are both excluded — the invitee is neither
        // local to the source nor remote-to-itself.
        let local = self.local.list_members(channel).await.map_err(|e| {
            format!(
                "list_members failed while building invite envelope (channel={}): {e}",
                channel.as_str()
            )
        })?;
        let remote = self.local.list_remote_members(channel).await.map_err(|e| {
            format!(
                "list_remote_members failed while building invite envelope (channel={}): {e}",
                channel.as_str()
            )
        })?;
        let initial_members: Vec<peko_protocol::channel::InitialMember> = local
            .iter()
            .filter(|p| p.subject_id() != invitee.0)
            .map(|p| peko_protocol::channel::InitialMember {
                principal_did: subject_wire_form(p),
                runtime_id: Some(ctx.caller_runtime_id.clone()),
            })
            .chain(
                remote
                    .iter()
                    .filter(|rm| {
                        !(rm.runtime_id == invitee_runtime_id
                            && rm.principal_id == invitee_bare)
                    })
                    .map(|rm| peko_protocol::channel::InitialMember {
                        principal_did: rm.principal_id.clone(),
                        runtime_id: Some(rm.runtime_id.clone()),
                    }),
            )
            .chain(std::iter::once(peko_protocol::channel::InitialMember {
                principal_did: invitee_bare.clone(),
                runtime_id: None,
            }))
            .collect();

        // Serialize the snapshot once so the bytes signed are the
        // bytes the receiver will re-serialize and verify.
        let initial_members_bytes =
            serde_json::to_vec(&initial_members).map_err(|e| {
                format!("serialize initial_members for invite envelope: {e}")
            })?;

        let request_id = Uuid::new_v4().to_string();
        let signed = crate::tunnel::ChannelInviteSignedFields {
            request_id: &request_id,
            source_runtime_id: &ctx.caller_runtime_id,
            recipient_runtime_id: &invitee_runtime_id,
            source_principal_did: &inviter.to_string(),
            channel_id: channel.as_str(),
            creator: &meta.creator,
            creator_did,
            name: &meta.name,
            passive_binding: meta.passive_binding.as_deref().unwrap_or(""),
            initial_members_bytes: &initial_members_bytes,
        };
        let signature = crate::tunnel::sign_channel_invite(&ctx.signing_key, signed);

        // Audit on the source runtime side. The receiver's
        // `received_inbound` audit row is emitted by the
        // dispatcher's inbound handler.
        let preview_json =
            String::from_utf8_lossy(&initial_members_bytes).into_owned();
        crate::tunnel::emit_forwarded_outbound(
            &request_id,
            &ctx.caller_runtime_id,
            &inviter.to_string(),
            channel.as_str(),
            "channel_invite",
            &crate::tunnel::tunnel_channel_audit::preview_event_payload(
                &preview_json,
            ),
        );

        let envelope = crate::tunnel::TunnelMessage::TunnelChannelInvite {
            request_id,
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: invitee_runtime_id.clone(),
            source_principal_did: inviter.to_string(),
            channel_id: channel.as_str().to_string(),
            creator: meta.creator.clone(),
            creator_did: creator_did.to_string(),
            name: meta.name.clone(),
            passive_binding: meta.passive_binding.clone(),
            initial_members,
            signature,
        };

        handle.send(envelope).map_err(|e| {
            format!("send TunnelChannelInvite to {invitee_runtime_id} failed: {e}")
        })?;

        Ok(())
    }

    /// Bootstrap a local mirror for a cross-runtime channel the
    /// receiver was invited to. Called by the AppState's
    /// `TunnelHost::dm_channel_mirror_bootstrap` on inbound
    /// `TunnelChannelInvite` envelopes (sprint 3 Phase 12a) **after**
    /// the signature has verified — the caller is responsible for
    /// that gate.
    ///
    /// Thin pass-through to
    /// [`peko_channel::ChannelStore::join_remote`]. The wrapper does
    /// not need a `CrossRuntimeChannelCtx` (no outbound fan-out is
    /// triggered on a join; the synthetic `ChannelEvent::Created`
    /// the store appends lives entirely on the receiver side and is
    /// what PR-2b's `peko-stream` listener picks up).
    ///
    /// Idempotent: a second call for the same `channel` is a no-op
    /// (delegates to the store's `meta.json`-existence check). This
    /// is the contract that makes the dispatcher safe to retry on a
    /// duplicate envelope.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn join_remote(
        &self,
        channel: &ChannelId,
        creator: &str,
        name: &str,
        initial_members: &[peko_protocol::channel::InitialMember],
        self_principal: &PrincipalId,
        source_runtime_id: &str,
        passive_binding: Option<String>,
    ) -> Result<()> {
        self.local
            .join_remote(
                channel,
                creator,
                name,
                initial_members,
                self_principal,
                source_runtime_id,
                passive_binding,
            )
            .await
    }
}

// ---------------------------------------------------------------------------
// ChannelPort impl — pure delegation to the local store
// ---------------------------------------------------------------------------

#[async_trait]
impl ChannelPort for TunnelChannelPort {
    async fn create(
        &self,
        creator: &PrincipalId,
        opts: CreateOpts,
    ) -> Result<ChannelId> {
        self.local.create(creator, opts).await
    }

    async fn invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        invitee: &Subject,
    ) -> Result<()> {
        // 1. Local-only invite — delegate straight to the store.
        // The store records the `MemberJoined` event and updates
        // `members.json`. We use the underlying `invite` method
        // directly because cross-runtime resolution (which runtime
        // hosts the invitee?) belongs above this layer (the
        // cross-runtime a2a path uses `peko_directory` for
        // runtime-by-DID lookups; `ChannelPort::invite` is
        // transport-agnostic by design).
        self.local.invite(channel, inviter, invitee).await?;

        // 2. Outbound cross-runtime fan-out: if the invitee lives on
        // a non-self runtime, record its remote-member row and emit a
        // signed `TunnelChannelInvite` envelope to that runtime so
        // the receiver can bootstrap a local mirror (Phase 12a). The
        // envelope snapshots the channel's `members.json` (re-keyed
        // to the receiver's view) + `meta.json` (creator / name / the
        // DM marker) so the receiver's mirror matches what the
        // inviter just persisted.
        //
        // We resolve `invitee → runtime_id` via a "did:key:z"
        // heuristic: principal DIDs that include `@<runtime-id>`
        // carry the runtime as a suffix. The full
        // directory-backed lookup lands in a follow-up that
        // threads `CrossRuntimeChannelCtx.directory` through this
        // path; today `invitee_runtime_id` defaults to "self" if no
        // suffix is present, which mirrors the
        // no-cross-runtime-invite default.
        //
        // The trait surface carries no principal-DID resolver, so
        // `creator_did` degrades to the inviter's source-local id
        // here; the DM provisioning path (Phase 12b) calls
        // `fanout_dm_invite` directly with the principal's real DID.
        //
        // ADR-049 Phase 1: fan-out is principal-only. A user invitee
        // is runtime-local (there is no cross-runtime user routing
        // yet), so the local invite above is the whole operation.
        //
        // Failures here never error the local invite — the local
        // mirror is authoritative; remote runtimes hydrate off the
        // next event or via a follow-up invite.
        if let Subject::Principal(did) = invitee {
            if let Err(e) = self
                .fanout_dm_invite(channel, inviter, &inviter.to_string(), &PrincipalId::from_did(did))
                .await
            {
                warn!(
                    "outbound TunnelChannelInvite fan-out partial-failure for channel={} \
                     (local invite succeeded): {}",
                    channel.as_str(),
                    e
                );
            }
        }

        Ok(())
    }

    async fn post(
        &self,
        channel: &ChannelId,
        sender: &Subject,
        msg: PostMsg,
    ) -> Result<TaskId> {
        // 1. Local write — same path the bare `ChannelStore` would
        // take. We use `post_with_event` so we get the
        // `ChannelEvent` back for outbound fan-out (no
        // re-derivation, no timestamp drift).
        let (line, ev) = self.local.post_with_event(channel, sender, msg).await?;

        // 2. Outbound fan-out. Failures here never error the local
        // post — the local mirror is authoritative; remote runtimes
        // hydrate off the next event or via `peek` reconciliation.
        if let Err(e) = self.fanout_event(channel, &ev, sender).await {
            warn!(
                "outbound TunnelChannelEvent fan-out partial-failure for channel={} \
                 (local post succeeded at line {}): {}",
                channel.as_str(),
                line,
                e
            );
        }

        Ok(line)
    }

    /// Phase 11: attributed posts (peer-DM inbound attribution).
    /// Identical fan-out semantics to [`Self::post`] — the local
    /// write carries the explicit `author`, and the same event (with
    /// the custom author) is fanned out.
    async fn post_attributed(
        &self,
        channel: &ChannelId,
        sender: &Subject,
        author: &str,
        msg: PostMsg,
    ) -> Result<TaskId> {
        // 1. Local attributed write — same path as `post`, but with
        // the caller-supplied author string on the event.
        let (line, ev) = self
            .local
            .post_attributed_with_event(channel, sender, author, msg)
            .await?;

        // 2. Outbound fan-out. Failures here never error the local
        // post — the local mirror is authoritative; remote runtimes
        // hydrate off the next event or via `peek` reconciliation.
        if let Err(e) = self.fanout_event(channel, &ev, sender).await {
            warn!(
                "outbound TunnelChannelEvent fan-out partial-failure for channel={} \
                 (local attributed post succeeded at line {}): {}",
                channel.as_str(),
                line,
                e
            );
        }

        Ok(line)
    }

    async fn peek(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<ChannelEvent>> {
        self.local.peek(channel, since).await
    }

    async fn peek_with_ids(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<(TaskId, ChannelEvent)>> {
        self.local.peek_with_ids(channel, since).await
    }

    /// Delegate to the local store's tail read so the default
    /// (full-walk + slice) impl doesn't run — the tunnel wrapper adds
    /// fan-out on writes only; reads are pure pass-throughs.
    async fn peek_tail(
        &self,
        channel: &ChannelId,
        limit: usize,
        before: Option<&TaskId>,
    ) -> Result<peko_channel::TailPage> {
        self.local.peek_tail(channel, limit, before).await
    }

    async fn leave(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
    ) -> Result<()> {
        self.local.leave(channel, principal).await
    }

    async fn list_members(
        &self,
        channel: &ChannelId,
    ) -> Result<Vec<Subject>> {
        // Merge local + remote. The `RemoteMember.principal_id` is a
        // stringified principal id (the on-disk shape matches the
        // legacy bare local member rows), so each remote row wraps as
        // a `Subject::Principal`.
        let mut out = self.local.list_members(channel).await?;
        for rm in self.local.list_remote_members(channel).await? {
            out.push(Subject::Principal(rm.principal_id.into()));
        }
        Ok(out)
    }

    async fn list_for_principal(
        &self,
        principal: &PrincipalId,
    ) -> Result<Vec<ChannelId>> {
        self.local.list_for_principal(principal).await
    }

    async fn membership(
        &self,
        channel: &ChannelId,
    ) -> Result<ChannelMembership> {
        self.local.membership(channel).await
    }

    /// Phase 4 (agent-session paradigm sprint): delegate the
    /// `meta.json` passive-binding read to the local store — the
    /// wrapper's `ChannelPort` impl overrides every other method, so
    /// without this the trait's `None` default would silently shadow
    /// the store's real answer and bound channels would look unbound.
    async fn passive_binding(&self, channel: &ChannelId) -> Result<Option<String>> {
        self.local.passive_binding(channel).await
    }

    async fn pin_to_shared(
        &self,
        channel: &ChannelId,
    ) -> Result<std::path::PathBuf> {
        self.local.pin_to_shared(channel).await
    }

    async fn subscribe_events(
        &self,
        channel: &ChannelId,
    ) -> tokio::sync::broadcast::Receiver<ChannelEvent> {
        self.local.subscribe_events_broadcast(channel).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel::hub_directory::fake::FakeAgentDirectory;
    use crate::tunnel::known_runtimes::KnownRuntimes;
    use crate::tunnel::TunnelHandle;
    use peko_channel::{ChannelConfig, ChannelError};
    use peko_subject::PrincipalId;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    /// `append_remote_event` delegates to the local store and lands
    /// the event in the local mirror. The store-level back-compat
    /// tests in `peko_channel::store::tests` already cover the
    /// on-disk invariants; this test pins the delegation contract
    /// from `TunnelChannelPort`'s side.
    #[tokio::test]
    async fn append_remote_event_delegates_to_local_store() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        // Create the channel locally so `append_remote_event` has a
        // mirror directory to write into.
        let channel = port
            .create(&PrincipalId("prin_alice".into()), CreateOpts::runtime("team"))
            .await
            .unwrap();

        let remote_event = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_bob@runtime-B".into(),
            parent: None,
            text: "hello from B".into(),
            at: "2026-08-06T00:00:00Z".into(),
        };
        let line = port
            .append_remote_event(&channel, &remote_event)
            .await
            .expect("delegate append must succeed");
        assert_eq!(
            line, "1",
            "Created is line 0; the remote event must land at line 1"
        );

        // The local mirror's `peek` returns the remote event — the
        // inbound append path worked end-to-end.
        let events = port
            .peek(&channel, &Checkpoint::default())
            .await
            .unwrap();
        assert_eq!(events.len(), 2, "Created + Posted = 2");
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, ChannelEvent::Posted { text, .. } if text == "hello from B")),
            "remote Posted event must show up in local peek"
        );
    }

    /// Every other `ChannelPort` method on `TunnelChannelPort`
    /// delegates straight to the local store. This single test
    /// exercises the four most-trafficked ones (`create`, `invite`,
    /// `post`, `list_members`) to catch a regression where a future
    /// commit adds a method but forgets the delegation arm.
    #[tokio::test]
    async fn channel_port_methods_delegate_to_local_store() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store);

        let creator = PrincipalId("prin_alice".into());
        let channel = port
            .create(&creator, CreateOpts::runtime("team"))
            .await
            .unwrap();

        port.invite(
            &channel,
            &creator,
            &Subject::from(&PrincipalId("prin_carol".into())),
        )
        .await
        .unwrap();

        let line = port
            .post(
                &channel,
                &Subject::from(&creator),
                PostMsg::root("hello world"),
            )
            .await
            .unwrap();
        assert_eq!(line, "2", "Created=0, MemberJoined=1, Posted=2");

        let members = port.list_members(&channel).await.unwrap();
        let member_strs: Vec<&str> = members.iter().map(|p| p.subject_id()).collect();
        assert_eq!(member_strs, vec!["prin_alice", "prin_carol"]);
    }

    /// `append_remote_event` on an unknown channel surfaces
    /// `NotFound` (delegated from the store). The dispatcher never
    /// reaches this path for a valid signed envelope, but defense
    /// in depth: an unknown channel id should still fail cleanly.
    #[tokio::test]
    async fn append_remote_event_on_unknown_channel_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store);

        let bogus = ChannelId::generate();
        let ev = ChannelEvent::Posted {
            channel: bogus.clone(),
            author: "prin_bob".into(),
            parent: None,
            text: "should not land".into(),
            at: "2026-08-06T00:00:00Z".into(),
        };
        let result = port.append_remote_event(&bogus, &ev).await;
        assert!(
            matches!(result, Err(ChannelError::NotFound(_))),
            "unknown channel must surface NotFound; got: {result:?}"
        );
    }

    /// PR-2b end-to-end: an inbound remote event delivered via
    /// `TunnelChannelPort::append_remote_event` (the dispatcher's
    /// entry point after signature verification) fires the
    /// `ChannelStore::notify_event` broadcast, so a peer that has
    /// called `ChannelPort::subscribe_events` receives the event in
    /// real time. This is the contract that powers the desktop's
    /// `ChannelEventsWatch` stream — without it, cross-runtime posts
    /// would only land on a polling refresh.
    #[tokio::test]
    async fn append_remote_event_notifies_subscribers() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store);

        // Create the channel locally so `append_remote_event` has a
        // mirror directory to write into.
        let channel = port
            .create(&PrincipalId("prin_alice".into()), CreateOpts::runtime("team"))
            .await
            .unwrap();

        // Subscribe BEFORE the inbound event arrives.
        let mut rx = port.subscribe_events(&channel).await;

        // Simulate the dispatcher handing off a signed inbound
        // `TunnelChannelEvent` to the local store.
        let remote_event = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_bob@runtime-B".into(),
            parent: None,
            text: "live from B".into(),
            at: "2026-08-06T00:00:00Z".into(),
        };
        port.append_remote_event(&channel, &remote_event)
            .await
            .expect("append_remote_event must succeed");

        // The subscriber must observe the event within a tight
        // timeout — no polling, no lag.
        let observed = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            rx.recv(),
        )
        .await
        .expect("subscriber must receive the event within 1s")
        .expect("broadcast must not be closed");
        match observed {
            ChannelEvent::Posted { text, author, .. } => {
                assert_eq!(text, "live from B");
                assert_eq!(author, "prin_bob@runtime-B");
            }
            other => panic!("expected Posted, got {other:?}"),
        }
    }

    /// Pin: a `TunnelChannelPort` without a `ctx` is still safe to
    /// use for the local-only path. Commit 3 will require `ctx` for
    /// the outbound fan-out, but commit 2 must not break pre-existing
    /// tests / fixtures that don't construct one.
    #[tokio::test]
    async fn new_without_ctx_is_local_only_but_safe() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store);
        // No-ctx port returns `None` for `ctx()`.
        assert!(port.ctx().await.is_none());

        // Local-only `post` still works (commit 3's fan-out is a
        // *post*-local-write addition, not a replacement).
        let channel = port
            .create(&PrincipalId("prin_alice".into()), CreateOpts::runtime("t"))
            .await
            .unwrap();
        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_alice".into())),
            PostMsg::root("hello"),
        )
        .await
        .unwrap();
    }

    /// The `Tier::Shared` round-trip works through the wrapper
    /// (delegates to the store). Catches a future change where a
    /// commit accidentally drops the `pin_to_shared` arm. We
    /// `create` a Runtime-tier channel and then `pin_to_shared`
    /// promotes to Shared — `pin_to_shared` copies from Runtime to
    /// Shared, not within a tier.
    #[tokio::test]
    async fn pin_to_shared_delegates_to_local_store() {
        let tmp = TempDir::new().unwrap();
        let shared = tmp.path().join("shared");
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: Some(shared.clone()),
        }));
        let port = TunnelChannelPort::new(store);

        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();
        let shared_dir = port.pin_to_shared(&channel).await.unwrap();
        assert!(
            shared_dir.starts_with(&shared),
            "shared path must live under shared_dir; got: {}",
            shared_dir.display()
        );
    }

    // -----------------------------------------------------------------
    // Outbound fan-out (commit 3)
    //
    // These tests verify the full outbound path:
    //   `post` → `post_with_event` → `fanout_event` → `TunnelChannelEvent`
    //   → `TunnelHandle::send` → mock `mpsc::Receiver`.
    //
    // The `directory` + `signing_key` + `tunnel` slot all need to be
    // real (not `None`) so we exercise the production wiring shape
    // end-to-end. `KnownRuntimes` is a fresh default — direct-LAN
    // transport is deferred, so the registry is read-only at this
    // commit.
    // -----------------------------------------------------------------

    /// Build a `CrossRuntimeChannelCtx` for unit tests. Centralizes
    /// the boilerplate so the fan-out tests stay focused on the
    /// assertion shape.
    async fn build_test_ctx(
        caller_runtime_id: &str,
    ) -> (
        Arc<CrossRuntimeChannelCtx>,
        mpsc::Receiver<crate::tunnel::TunnelMessage>,
    ) {
        use ed25519_dalek::SigningKey;
        let (tx, rx) = mpsc::channel::<crate::tunnel::TunnelMessage>(16);
        let handle = TunnelHandle::new(tx);
        let tunnel_slot: Arc<RwLock<Option<TunnelHandle>>> =
            Arc::new(RwLock::new(Some(handle)));
        let directory: Arc<dyn crate::tunnel::AgentDirectory> =
            Arc::new(FakeAgentDirectory::default());
        let signing_key = Arc::new(SigningKey::from_bytes(&[7u8; 32]));
        let known_runtimes = Arc::new(RwLock::new(KnownRuntimes::new()));
        let ctx = Arc::new(CrossRuntimeChannelCtx {
            directory,
            signing_key,
            caller_runtime_id: caller_runtime_id.to_string(),
            tunnel: tunnel_slot,
            known_runtimes,
        });
        (ctx, rx)
    }

    /// `post` with a remote member: the local store gets the event
    /// (line 2 after Created + MemberJoined is line 0; the local
    /// member joined row is line 1), the outbound `TunnelChannelEvent`
    /// is sent on the mock `TunnelHandle` for the unique recipient
    /// runtime, and the signature on the wire verifies against the
    /// runtime's signing key.
    #[tokio::test]
    async fn post_fans_out_to_unique_recipient_runtime() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        let (ctx, mut rx) = build_test_ctx("did:key:zRuntimeA").await;
        port.set_ctx(ctx.clone()).await;

        // Create a channel and add a remote member on runtime B.
        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();
        port.add_remote_member(&channel, "did:key:zRuntimeB", "prin_bob")
            .await
            .unwrap();

        // Post a message — fans out to runtime B.
        let line = port
            .post(
                &channel,
                &Subject::from(&PrincipalId("prin_alice".into())),
                PostMsg::root("hello from A"),
            )
            .await
            .expect("post must succeed");

        // Local mirror landed the event.
        let local_events = port
            .peek_with_ids(&channel, &Checkpoint::default())
            .await
            .unwrap();
        // add_remote_member only mutates members.json — it does
        // NOT append a MemberJoined event to the events log. So
        // the local log is just Created (line 0) + Posted (line 1).
        assert_eq!(local_events.len(), 2, "Created + Posted");
        assert_eq!(line, "1");
        let last = &local_events.last().unwrap().1;
        match last {
            ChannelEvent::Posted { text, .. } => {
                assert_eq!(text, "hello from A");
            }
            other => panic!("expected Posted, got {other:?}"),
        }

        // Outbound: exactly one `TunnelChannelEvent` reached the
        // mock tunnel, addressed to runtime B's `channel_id`.
        let env = rx
            .recv()
            .await
            .expect("outbound TunnelChannelEvent must reach mock tunnel");
        let (request_id, source_runtime_id, recipient_runtime_id, source_principal_did, channel_id, event, signature) =
            match env {
                crate::tunnel::TunnelMessage::TunnelChannelEvent {
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    channel_id,
                    event,
                    signature,
                } => (
                    request_id,
                    source_runtime_id,
                    recipient_runtime_id,
                    source_principal_did,
                    channel_id,
                    event,
                    signature,
                ),
                other => panic!("expected TunnelChannelEvent, got {other:?}"),
            };

        // Identity fields are populated from ctx.
        assert_eq!(source_runtime_id, "did:key:zRuntimeA");
        assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
        assert_eq!(source_principal_did, "prin_alice");
        assert_eq!(channel_id, channel.as_str());
        // request_id is a fresh UUIDv4 (length 36 incl. hyphens).
        assert_eq!(request_id.len(), 36, "request_id should be a UUIDv4");
        assert!(!signature.is_empty(), "envelope must carry a signature");

        // The event body carries the post text + author.
        match &event {
            ChannelEvent::Posted { text, author, .. } => {
                assert_eq!(text, "hello from A");
                assert_eq!(author, "prin_alice");
            }
            other => panic!("expected Posted in envelope, got {other:?}"),
        }

        // Signature verifies against ctx.signing_key.
        let event_bytes = serde_json::to_vec(&event).unwrap();
        // Re-derive the recipient runtime id from the outbound path
        // so this assertion matches the bytes the source runtime
        // actually signed (the test ctx has a single recipient).
        let recipient_runtime_id = "did:key:zRuntimeB".to_string();
        crate::tunnel::verify_channel_event(
            &ctx.signing_key.verifying_key(),
            crate::tunnel::ChannelSignedFields {
                request_id: &request_id,
                source_runtime_id: &source_runtime_id,
                recipient_runtime_id: &recipient_runtime_id,
                source_principal_did: &source_principal_did,
                channel_id: &channel_id,
                event_bytes: &event_bytes,
            },
            &signature,
        )
        .expect("outbound signature must verify");
    }

    /// Two remote members on the same runtime fan out to a single
    /// recipient (dedupe by `runtime_id`). A channel with 1 host
    /// runtime + 2 remote members on runtime B produces 1 outbound
    /// envelope, not 2.
    #[tokio::test]
    async fn post_fans_out_once_per_unique_recipient_runtime() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        let (ctx, mut rx) = build_test_ctx("did:key:zRuntimeA").await;
        port.set_ctx(ctx).await;

        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();
        // Two remote members both on runtime B → dedupe to 1.
        port.add_remote_member(&channel, "did:key:zRuntimeB", "prin_bob")
            .await
            .unwrap();
        port.add_remote_member(&channel, "did:key:zRuntimeB", "prin_carol")
            .await
            .unwrap();

        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_alice".into())),
            PostMsg::root("hello"),
        )
        .await
        .unwrap();

        let env = rx.recv().await.expect("exactly one envelope expected");
        assert!(
            matches!(
                env,
                crate::tunnel::TunnelMessage::TunnelChannelEvent { .. }
            ),
            "first envelope must be TunnelChannelEvent"
        );
        // No second envelope — the dedupe held.
        assert!(
            rx.try_recv().is_err(),
            "must not send a second envelope for the same runtime"
        );
    }

    /// Remote members on **two distinct** runtimes produce two
    /// outbound envelopes — one per recipient runtime.
    #[tokio::test]
    async fn post_fans_out_to_each_distinct_recipient_runtime() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        let (ctx, mut rx) = build_test_ctx("did:key:zRuntimeA").await;
        port.set_ctx(ctx).await;

        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();
        port.add_remote_member(&channel, "did:key:zRuntimeB", "prin_bob")
            .await
            .unwrap();
        port.add_remote_member(&channel, "did:key:zRuntimeC", "prin_dave")
            .await
            .unwrap();

        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_alice".into())),
            PostMsg::root("hi all"),
        )
        .await
        .unwrap();

        let mut recipients = std::collections::HashSet::new();
        for _ in 0..2 {
            let env = rx.recv().await.expect("two envelopes expected");
            let src = match env {
                crate::tunnel::TunnelMessage::TunnelChannelEvent {
                    source_runtime_id,
                    ..
                } => source_runtime_id,
                other => panic!("expected TunnelChannelEvent, got {other:?}"),
            };
            recipients.insert(src);
        }
        // Both runtimes appear in the outbound set (the source
        // runtime is *not* a recipient — `fanout_event` only
        // iterates remote members).
        assert_eq!(
            recipients.len(),
            1,
            "source runtime must not appear in recipient set; got: {recipients:?}"
        );
        assert!(
            recipients.contains("did:key:zRuntimeA"),
            "set contained: {recipients:?}"
        );
    }

    /// `post` to a channel with no remote members is a pure local
    /// write — no outbound envelope is sent. The fan-out code
    /// short-circuits before consulting the tunnel slot.
    #[tokio::test]
    async fn post_with_no_remote_members_is_local_only() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        let (ctx, mut rx) = build_test_ctx("did:key:zRuntimeA").await;
        port.set_ctx(ctx).await;

        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();
        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_alice".into())),
            PostMsg::root("solo"),
        )
        .await
        .unwrap();

        assert!(
            rx.try_recv().is_err(),
            "no remote members → no outbound envelope"
        );
    }

    /// `fanout_event` short-circuits to Ok(()) when the tunnel
    /// slot is empty (`None`). Production can land in this state
    /// during tunnel reconnects; local post still succeeds.
    #[tokio::test]
    async fn post_with_no_tunnel_handle_does_not_error() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        // Build a ctx but leave the tunnel slot empty.
        use ed25519_dalek::SigningKey;
        let directory: Arc<dyn crate::tunnel::AgentDirectory> =
            Arc::new(FakeAgentDirectory::default());
        let signing_key = Arc::new(SigningKey::from_bytes(&[7u8; 32]));
        let tunnel_slot: Arc<RwLock<Option<TunnelHandle>>> =
            Arc::new(RwLock::new(None));
        let ctx = Arc::new(CrossRuntimeChannelCtx {
            directory,
            signing_key,
            caller_runtime_id: "did:key:zRuntimeA".into(),
            tunnel: tunnel_slot,
            known_runtimes: Arc::new(RwLock::new(KnownRuntimes::new())),
        });
        port.set_ctx(ctx).await;

        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();
        port.add_remote_member(&channel, "did:key:zRuntimeB", "prin_bob")
            .await
            .unwrap();

        // Post must still succeed — fan-out silently no-ops on a
        // missing tunnel handle, never errors the local write.
        let line = port
            .post(
                &channel,
                &Subject::from(&PrincipalId("prin_alice".into())),
                PostMsg::root("patience"),
            )
            .await
            .expect("post must succeed even without tunnel");
        assert_eq!(line, "1");
    }

    // -----------------------------------------------------------------
    // Outbound fan-out (PR-3a commit 3)
    //
    // These tests verify the invite fan-out path:
    //   `invite` → `fanout_invite` → `TunnelChannelInvite` →
    //   `TunnelHandle::send` → mock `mpsc::Receiver`.
    //
    // The 1-test scope per the plan: `invite_with_remote_invitee_emits_signed_envelope`
    // exercises the full end-to-end fan-out (with a runtime-suffixed
    // invitee DID) and verifies the signature on the wire against
    // the runtime's signing key.
    // -----------------------------------------------------------------

    /// `ChannelPort::invite` to a principal whose DID carries an
    /// `@<runtime-id>` suffix emits exactly one signed
    /// `TunnelChannelInvite` envelope to that runtime's mock
    /// `TunnelHandle`. Phase 12a: the invitee is ALSO recorded as a
    /// `RemoteMember` row on the source (so later posts fan out —
    /// the previously dead `add_remote_member` is wired), the
    /// snapshot is re-keyed to the receiver's view (source-local
    /// rows carry `Some(source_runtime_id)`; the invitee row is
    /// bare with `None` — "addressed to you"), and the envelope
    /// carries `creator_did` + the DM marker.
    ///
    /// Mirrors `post_fans_out_to_unique_recipient_runtime` for the
    /// invite path. Pins the contract for PR-3's desktop invite UX:
    /// inviting a remote runtime's principal produces a wire
    /// envelope the receiver's `handle_inbound_tunnel_channel_invite`
    /// can verify and bootstrap from.
    #[tokio::test]
    async fn invite_with_remote_invitee_emits_signed_envelope() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        let (ctx, mut rx) = build_test_ctx("did:key:zRuntimeA").await;
        port.set_ctx(ctx.clone()).await;

        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();

        // Invite a principal whose DID carries the @<runtime> suffix
        // — this is the convention that flags it as a
        // cross-runtime invite. The suffix resolves to runtime B.
        port.invite(
            &channel,
            &PrincipalId("prin_alice".into()),
            &Subject::from(&PrincipalId("prin_bob@did:key:zRuntimeB".into())),
        )
        .await
        .expect("local invite must succeed before fan-out");

        // Phase 12a: the invitee is filed as a RemoteMember on the
        // SOURCE (bare id, suffix stripped) so subsequent posts fan
        // out to runtime B.
        let remote = port.local().list_remote_members(&channel).await.unwrap();
        assert_eq!(
            remote,
            vec![peko_channel::port::RemoteMember {
                runtime_id: "did:key:zRuntimeB".to_string(),
                principal_id: "prin_bob".to_string(),
            }],
            "invite must record the remote-member row before sending"
        );

        // Outbound: exactly one TunnelChannelInvite reached the
        // mock tunnel, addressed to runtime B.
        let env = rx
            .recv()
            .await
            .expect("outbound TunnelChannelInvite must reach mock tunnel");
        let (
            request_id,
            source_runtime_id,
            recipient_runtime_id,
            source_principal_did,
            channel_id,
            creator,
            creator_did,
            name,
            passive_binding,
            initial_members,
            signature,
        ) = match env {
            crate::tunnel::TunnelMessage::TunnelChannelInvite {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                creator,
                creator_did,
                name,
                passive_binding,
                initial_members,
                signature,
            } => (
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                creator,
                creator_did,
                name,
                passive_binding,
                initial_members,
                signature,
            ),
            other => panic!("expected TunnelChannelInvite, got {other:?}"),
        };

        // Identity + routing fields are populated from ctx + the
        // resolved recipient.
        assert_eq!(source_runtime_id, "did:key:zRuntimeA");
        assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
        assert_eq!(source_principal_did, "prin_alice");
        assert_eq!(channel_id, channel.as_str());
        assert_eq!(creator, "prin_alice");
        // The bare `invite` trait path has no DID resolver — the
        // inviter's local id stands in (see the `invite` comment).
        assert_eq!(creator_did, "prin_alice");
        assert_eq!(name, "team");
        assert_eq!(passive_binding, None, "unbound channel → no DM marker");
        assert_eq!(request_id.len(), 36, "request_id should be a UUIDv4");
        assert!(!signature.is_empty(), "envelope must carry a signature");

        // Phase 12a snapshot re-keying: the source's own row (alice)
        // is remote from the receiver's view; the invitee row is
        // bare + `None` ("addressed to you"). Exactly two rows.
        assert_eq!(initial_members.len(), 2, "alice + bob in this channel");
        assert_eq!(initial_members[0].principal_did, "prin_alice");
        assert_eq!(
            initial_members[0].runtime_id.as_deref(),
            Some("did:key:zRuntimeA"),
            "source-local rows are emitted as remote for the receiver"
        );
        assert_eq!(initial_members[1].principal_did, "prin_bob");
        assert_eq!(
            initial_members[1].runtime_id, None,
            "the invitee row is the receiver-addressed one"
        );

        // Signature verifies against ctx.signing_key.
        let initial_members_bytes = serde_json::to_vec(&initial_members).unwrap();
        crate::tunnel::verify_channel_invite(
            &ctx.signing_key.verifying_key(),
            crate::tunnel::ChannelInviteSignedFields {
                request_id: &request_id,
                source_runtime_id: &source_runtime_id,
                recipient_runtime_id: &recipient_runtime_id,
                source_principal_did: &source_principal_did,
                channel_id: &channel_id,
                creator: &creator,
                creator_did: &creator_did,
                name: &name,
                passive_binding: "",
                initial_members_bytes: &initial_members_bytes,
            },
            &signature,
        )
        .expect("outbound invite signature must verify");

        // No second envelope — the invitee resolves to exactly one
        // unique runtime.
        assert!(
            rx.try_recv().is_err(),
            "must not send a second envelope for the same recipient runtime"
        );

        // Phase 12a: with the remote-member row recorded, a post now
        // fans out — before the fix this was a silent no-op.
        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_alice".into())),
            PostMsg::root("after-invite post"),
        )
        .await
        .unwrap();
        let env = rx
            .recv()
            .await
            .expect("post after invite must fan out (remote row recorded)");
        assert!(
            matches!(
                env,
                crate::tunnel::TunnelMessage::TunnelChannelEvent { .. }
            ),
            "expected TunnelChannelEvent after the invite, got {env:?}"
        );
    }

    /// The DM entry point: a bound channel invited via
    /// `fanout_dm_invite` with the creator's real DID emits an
    /// envelope whose `creator_did` + `passive_binding` DM marker
    /// sign and verify.
    #[tokio::test]
    async fn dm_invite_carries_creator_did_and_binding_marker() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        let (ctx, mut rx) = build_test_ctx("did:key:zRuntimeA").await;
        port.set_ctx(ctx.clone()).await;

        // A DM-tier channel (bound), as `ensure_peer_dm_channel`
        // provisions it.
        let channel = port
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("dm-principal-bob").with_passive_binding("/principal-bob"),
            )
            .await
            .unwrap();

        port.fanout_dm_invite(
            &channel,
            &PrincipalId("prin_alice".into()),
            "did:peko:principal:alice",
            &PrincipalId("did:peko:principal:bob@did:key:zRuntimeB".into()),
        )
        .await
        .expect("DM invite fan-out must succeed");

        let env = rx.recv().await.expect("DM invite envelope expected");
        let (
            request_id,
            source_runtime_id,
            recipient_runtime_id,
            creator_did,
            passive_binding,
            initial_members,
            signature,
        ) = match env {
            crate::tunnel::TunnelMessage::TunnelChannelInvite {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                creator_did,
                passive_binding,
                initial_members,
                signature,
                ..
            } => (
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                creator_did,
                passive_binding,
                initial_members,
                signature,
            ),
            other => panic!("expected TunnelChannelInvite, got {other:?}"),
        };

        assert_eq!(source_runtime_id, "did:key:zRuntimeA");
        assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
        assert_eq!(creator_did, "did:peko:principal:alice");
        assert_eq!(
            passive_binding.as_deref(),
            Some("/principal-bob"),
            "the DM marker rides the envelope"
        );
        // The invitee row is the receiver-addressed one: bare DID,
        // no runtime id.
        let invitee_row = initial_members
            .iter()
            .find(|m| m.runtime_id.is_none())
            .expect("invitee row present");
        assert_eq!(invitee_row.principal_did, "did:peko:principal:bob");

        // The full signed field set verifies.
        let initial_members_bytes = serde_json::to_vec(&initial_members).unwrap();
        crate::tunnel::verify_channel_invite(
            &ctx.signing_key.verifying_key(),
            crate::tunnel::ChannelInviteSignedFields {
                request_id: &request_id,
                source_runtime_id: &source_runtime_id,
                recipient_runtime_id: &recipient_runtime_id,
                source_principal_did: "prin_alice",
                channel_id: channel.as_str(),
                creator: "prin_alice",
                creator_did: &creator_did,
                name: "dm-principal-bob",
                passive_binding: "/principal-bob",
                initial_members_bytes: &initial_members_bytes,
            },
            &signature,
        )
        .expect("DM invite signature must verify");
    }

    /// Mirror-side fan-back (Phase 12a): after `join_remote` with the
    /// new parameters, the receiver's own principal is a local member
    /// (its post passes the membership check) and the creator's
    /// remote row routes the post back to the source runtime — an
    /// envelope is captured on the receiver's mock tunnel.
    #[tokio::test]
    async fn mirror_side_post_fans_back_to_source_runtime() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());

        // The RECEIVER's ctx: its own runtime is B; the source was A.
        let (ctx, mut rx) = build_test_ctx("did:key:zRuntimeB").await;
        port.set_ctx(ctx).await;

        let channel = ChannelId("chan_mirror01".to_string());
        let initial_members = vec![
            peko_protocol::channel::InitialMember {
                principal_did: "prin_alice".to_string(),
                runtime_id: Some("did:key:zRuntimeA".to_string()),
            },
            peko_protocol::channel::InitialMember {
                principal_did: "did:peko:principal:bob".to_string(),
                runtime_id: None,
            },
        ];
        port.join_remote(
            &channel,
            "prin_alice",
            "dm-principal-bob",
            &initial_members,
            &PrincipalId("prin_bob_local".into()),
            "did:key:zRuntimeA",
            Some("/principal-alice".to_string()),
        )
        .await
        .expect("join_remote must succeed");

        // The receiver posts a reply on its mirror...
        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_bob_local".into())),
            PostMsg::root("hello back"),
        )
        .await
        .expect("receiver is a local member of its mirror");

        // ...and the post fans back out to the source runtime.
        let env = rx
            .recv()
            .await
            .expect("mirror-side post must fan out to the source runtime");
        match env {
            crate::tunnel::TunnelMessage::TunnelChannelEvent {
                source_runtime_id,
                recipient_runtime_id,
                ..
            } => {
                assert_eq!(source_runtime_id, "did:key:zRuntimeB");
                assert_eq!(recipient_runtime_id, "did:key:zRuntimeA");
            }
            other => panic!("expected TunnelChannelEvent, got {other:?}"),
        }
    }
}