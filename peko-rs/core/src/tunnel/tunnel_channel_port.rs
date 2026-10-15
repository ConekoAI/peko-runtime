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
use peko_protocol::channel::ChannelEvent;
use peko_subject::{PrincipalId, Subject};
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

/// Type alias re-exported so the trait impl below reads naturally.
/// `peko_channel` defines `pub type TaskId = String;` in `port.rs` but
/// does not re-export it; aliasing here avoids touching
/// `peko_channel::lib.rs` for a public type that's already in
/// `peko_protocol::channel::ChannelEvent`'s wire form.
type TaskId = String;

use crate::tunnel::channel_outbox::{ChannelOutbox, OutboxEntry, OutboxItem, ReceivedLog};
use crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx;
use std::str::FromStr;

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

    /// Envelopes not yet confirmed by their recipient runtime
    /// ([`crate::tunnel::channel_outbox`]).
    outbox: Arc<ChannelOutbox>,
    /// Inbound envelopes already applied, so a resend is confirmed but
    /// not applied twice.
    received: Arc<ReceivedLog>,
    /// Set once the periodic resend task runs.
    resender_started: Arc<std::sync::atomic::AtomicBool>,
}

/// How often pending envelopes are considered for a resend.
const RESEND_TICK: std::time::Duration = std::time::Duration::from_secs(30);

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
        let delivery_dir = local.config().runtime_dir.join("channel_delivery");
        Self {
            local,
            ctx: Arc::new(RwLock::new(None)),
            outbox: Arc::new(ChannelOutbox::new(delivery_dir.join("outbox"))),
            received: Arc::new(ReceivedLog::new(delivery_dir.join("received.jsonl"))),
            resender_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Install the cross-runtime dispatch context. Idempotent: a
    /// later call replaces the prior ctx — used by the daemon's
    /// tunnel-reconnect path so a fresh `runtime_id` (after a
    /// credential rotation) propagates without rebuilding every
    /// `TunnelChannelPort`.
    pub async fn set_ctx(&self, ctx: Arc<CrossRuntimeChannelCtx>) {
        {
            let mut guard = self.ctx.write().await;
            *guard = Some(ctx);
        }
        self.start_resender();
        // Envelopes queued before a restart go out as soon as there is
        // a tunnel to send them on.
        self.flush(true).await;
    }

    /// Resend pending envelopes every [`RESEND_TICK`] (each on its own
    /// backoff). Started once, on the first `set_ctx`.
    fn start_resender(&self) {
        use std::sync::atomic::Ordering;
        if self.resender_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let port = self.clone();
        runtime.spawn(async move {
            let mut tick = tokio::time::interval(RESEND_TICK);
            tick.tick().await;
            loop {
                tick.tick().await;
                port.flush(false).await;
            }
        });
    }

    /// The tunnel (re)connected: resend everything pending now.
    pub(crate) async fn on_tunnel_ready(&self) {
        self.flush(true).await;
    }

    /// Send the pending envelopes that are due (all of them with
    /// `force`), oldest first. Needs a ctx and a live tunnel; a send
    /// error (tunnel gone) stops the pass. Envelopes whose build fails
    /// stay queued and are retried — the cause (e.g. a locked vault)
    /// may clear.
    pub(crate) async fn flush(&self, force: bool) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let Some(handle) = ctx.tunnel.read().await.clone() else {
            return;
        };
        for entry in self.outbox.due(force).await {
            let envelope = match self.envelope_for(&ctx, &entry).await {
                Ok(envelope) => envelope,
                Err(e) => {
                    warn!(
                        request_id = %entry.request_id,
                        "channel outbox: envelope build failed; will retry: {e}"
                    );
                    self.outbox.attempted(entry.seq).await;
                    continue;
                }
            };
            self.outbox.attempted(entry.seq).await;
            if let Err(e) = handle.send(envelope) {
                warn!("channel outbox: tunnel send failed; resending later: {e}");
                break;
            }
        }
    }

    /// Queue `item` for `recipient` and try it at once. A build failure
    /// is the caller's error (and drops the entry); no tunnel just
    /// leaves it queued.
    async fn send_queued(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        recipient_runtime_id: &str,
        item: OutboxItem,
    ) -> std::result::Result<(), String> {
        let entry = self
            .outbox
            .enqueue(recipient_runtime_id, item)
            .await
            .map_err(|e| format!("queue envelope for {recipient_runtime_id}: {e}"))?;
        let envelope = match self.envelope_for(ctx, &entry).await {
            Ok(envelope) => envelope,
            Err(e) => {
                self.outbox.discard(entry.seq).await;
                return Err(e);
            }
        };
        let handle = ctx.tunnel.read().await.clone();
        let Some(handle) = handle else {
            warn!(
                recipient = %recipient_runtime_id,
                "channel envelope queued: tunnel not connected; sent on reconnect"
            );
            return Ok(());
        };
        self.outbox.attempted(entry.seq).await;
        if let Err(e) = handle.send(envelope) {
            warn!(recipient = %recipient_runtime_id, "channel envelope queued; send failed: {e}");
        }
        Ok(())
    }

    /// Build and sign the envelope for an outbox entry (fresh `iat`/`exp`,
    /// the entry's own `request_id`).
    async fn envelope_for(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        entry: &OutboxEntry,
    ) -> std::result::Result<crate::tunnel::TunnelMessage, String> {
        match &entry.item {
            OutboxItem::Event {
                channel,
                event,
                source,
            } => {
                let source = Subject::from_str(source)
                    .unwrap_or_else(|_| Subject::Principal(source.clone().into()));
                self.event_envelope(
                    ctx,
                    &entry.request_id,
                    &entry.recipient_runtime_id,
                    channel,
                    event,
                    &source,
                )
                .await
            }
            OutboxItem::Invite {
                channel,
                inviter,
                creator_did,
                invitee,
            } => {
                self.invite_envelope(
                    ctx,
                    &entry.request_id,
                    channel,
                    inviter,
                    creator_did,
                    invitee,
                    &entry.recipient_runtime_id,
                )
                .await
            }
        }
    }

    /// `recipient` confirmed the envelopes `request_ids`.
    pub(crate) async fn confirm_delivery(
        &self,
        recipient_runtime_id: &str,
        request_ids: &[String],
    ) {
        self.outbox.confirm(recipient_runtime_id, request_ids).await;
    }

    /// Wait up to `timeout` for every recipient to confirm message
    /// `msg_id`. `true` when confirmed (or nothing was queued for it).
    pub(crate) async fn wait_delivered(&self, msg_id: &str, timeout: std::time::Duration) -> bool {
        self.outbox.wait_confirmed(msg_id, timeout).await
    }

    /// Whether inbound envelope `request_id` was already applied.
    pub(crate) async fn already_applied(&self, request_id: &str) -> bool {
        self.received.contains(request_id).await
    }

    /// Record inbound envelope `request_id` as applied.
    pub(crate) async fn mark_applied(&self, request_id: &str) {
        self.received.record(request_id).await;
    }

    /// Confirm to `recipient` that this runtime applied `request_ids`
    /// for `channel`. Best-effort (a lost receipt means a resend, which
    /// is confirmed again); signed with the runtime key.
    pub(crate) async fn send_receipt(
        &self,
        recipient_runtime_id: &str,
        channel: &ChannelId,
        request_ids: Vec<String>,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let Some(handle) = ctx.tunnel.read().await.clone() else {
            return;
        };
        let event = ChannelEvent::Received {
            channel: channel.clone(),
            request_ids,
            at: chrono::Utc::now().to_rfc3339(),
        };
        let Ok(event_bytes) = serde_json::to_vec(&event) else {
            return;
        };
        let request_id = Uuid::new_v4().to_string();
        let iat = chrono::Utc::now().timestamp();
        let (signature, author_signature) = crate::tunnel::sign_channel_event(
            &ctx.signing_key,
            None,
            crate::tunnel::ChannelSignedFields {
                request_id: &request_id,
                source_runtime_id: &ctx.caller_runtime_id,
                recipient_runtime_id,
                source_principal_did: &ctx.caller_runtime_id,
                channel_id: channel.as_str(),
                event_bytes: &event_bytes,
            },
            iat,
            iat + 300,
        );
        let envelope = crate::tunnel::TunnelMessage::TunnelChannelEvent {
            request_id,
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: recipient_runtime_id.to_string(),
            source_principal_did: ctx.caller_runtime_id.clone(),
            channel_id: channel.as_str().to_string(),
            event,
            signature,
            author_signature,
        };
        if let Err(e) = handle.send(envelope) {
            warn!(recipient = %recipient_runtime_id, "channel receipt send failed: {e}");
        }
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

        // Queue one envelope per recipient runtime; each is sent now if
        // the tunnel is up and resent until its recipient confirms it
        // (`tunnel::channel_outbox`).
        let mut failures: Vec<String> = Vec::new();
        for recipient_runtime_id in unique_runtimes {
            let item = OutboxItem::Event {
                channel: channel.clone(),
                event: ev.clone(),
                source: source.to_string(),
            };
            if let Err(e) = self.send_queued(&ctx, &recipient_runtime_id, item).await {
                failures.push(e);
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    /// Build and sign one `TunnelChannelEvent` for `recipient_runtime_id`
    /// under `request_id` (fresh `iat`/`exp` on every call, so a resend
    /// is never stale).
    async fn event_envelope(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        request_id: &str,
        recipient_runtime_id: &str,
        channel: &ChannelId,
        ev: &ChannelEvent,
        source: &Subject,
    ) -> std::result::Result<crate::tunnel::TunnelMessage, String> {
        // Serialize the event ONCE so the bytes signed are the bytes
        // the receiver will re-serialize and verify (serde_json
        // emits struct fields in declaration order, so the round-trip
        // is stable).
        let mut wire_event = ev.clone();
        let actor_field = match &mut wire_event {
            ChannelEvent::Posted { author, .. } => Some(author),
            ChannelEvent::MemberLeft { member, .. } => Some(member),
            _ => None,
        };
        if let Some(author) = actor_field {
            let subject = author
                .parse::<Subject>()
                .unwrap_or_else(|_| Subject::Principal(author.clone().into()));
            {
                let actor = self.local.actor_info(&subject).await;
                if let Some(hub) = actor.hub_address() {
                    *author = hub;
                }
            }
        }
        let ev = &wire_event;
        let event_bytes =
            serde_json::to_vec(ev).map_err(|e| format!("serialize ChannelEvent: {e}"))?;

        // ADR-058 D2: resolve the author's own signing key. When the
        // source is a locally-hosted principal with a vault-backed
        // `did:key` identity, the envelope's `source_principal_did`
        // carries that DID and the envelope gains an author JWS over
        // the same payload segment as the runtime counter-signature.
        // Otherwise the envelope keeps the legacy wire id and goes
        // out runtime-vouched (`author_signature: ""`).
        //
        // ADR-058 review fix (fail-closed): a vault-backed `did:key`
        // principal whose signing key cannot be loaded (transient
        // vault failure) must FAIL the send — silently downgrading to
        // the legacy wire id would produce exactly the runtime-vouched
        // envelope the receiver's D2 author gate exists to refuse.
        let author: Option<(String, Arc<ed25519_dalek::SigningKey>)> = match source {
            Subject::Principal(did) => {
                let pid = PrincipalId::from_did(did);
                match ctx.principal_keys.did_for_principal(&pid).await {
                    Some(did) => match ctx.principal_keys.signing_key_for_did(&did).await {
                        Some(key) => Some((did, key)),
                        None => {
                            return Err(format!(
                                "author signing key unavailable for vault-backed principal \
                                     {did} (channel={}): refusing to send a runtime-vouched \
                                     envelope for a did:key author",
                                channel.as_str()
                            ));
                        }
                    },
                    None => None,
                }
            }
            _ => None,
        };
        let source_wire_id = match &author {
            Some((did, _)) => did.clone(),
            None if matches!(source, Subject::User(_) | Subject::Visitor(_)) => self
                .local
                .actor_info(source)
                .await
                .hub_address()
                .ok_or_else(|| "Human sender has no Hub identity".to_string())?,
            None => return Err("Principal signing key is unavailable".into()),
        };

        let request_id = request_id.to_string();
        let recipient_runtime_id = recipient_runtime_id.to_string();
        let signed = crate::tunnel::ChannelSignedFields {
            request_id: &request_id,
            source_runtime_id: &ctx.caller_runtime_id,
            recipient_runtime_id: &recipient_runtime_id,
            source_principal_did: &source_wire_id,
            channel_id: channel.as_str(),
            event_bytes: &event_bytes,
        };
        // ADR-058 D3: `iat = now`, `exp = now + 300` (seconds, UTC).
        let iat = chrono::Utc::now().timestamp();
        let (signature, author_signature) = crate::tunnel::sign_channel_event(
            &ctx.signing_key,
            author.as_ref().map(|(_, k)| k.as_ref()),
            signed,
            iat,
            iat + 300,
        );

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
            &crate::tunnel::tunnel_channel_audit::preview_event_payload(&preview_json),
        );

        let envelope = crate::tunnel::TunnelMessage::TunnelChannelEvent {
            request_id,
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: recipient_runtime_id.clone(),
            source_principal_did: source_wire_id.clone(),
            channel_id: channel.as_str().to_string(),
            event: ev.clone(),
            signature,
            author_signature,
        };

        Ok(envelope)
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

    /// Predicate: is the `(runtime_id, principal_id)` pair registered
    /// as a remote member of `channel`? ADR-058 D2: the inbound
    /// dispatcher gates `did:key`-authored events on this check —
    /// membership claims are now backed by principal signatures, so
    /// the check is meaningful for the first time.
    ///
    /// # Errors
    ///
    /// Surfaces the store's read errors (missing `members.json`,
    /// unknown channel) so the caller can fail closed.
    pub async fn is_remote_member(
        &self,
        channel: &ChannelId,
        runtime_id: &str,
        principal_id: &str,
    ) -> Result<bool> {
        self.local
            .is_remote_member(channel, runtime_id, principal_id)
            .await
    }

    /// Queue a signed channel invitation to an explicitly resolved runtime.
    pub(crate) async fn fanout_dm_invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        creator_did: &str,
        invitee: &PrincipalId,
        invitee_runtime_id: &str,
    ) -> std::result::Result<(), String> {
        let invitee_bare = invitee.0.clone();

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
        if channel.as_str().starts_with("dm_") {
            let old = self
                .local
                .replace_dm_peer_route(channel, &invitee_bare, invitee_runtime_id)
                .await
                .map_err(|e| e.to_string())?;
            for runtime in old {
                self.outbox
                    .retarget(channel, &runtime, invitee_runtime_id)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        self.add_remote_member(channel, invitee_runtime_id, &invitee_bare)
            .await
            .map_err(|e| {
                format!(
                    "add_remote_member failed (channel={}): {e}",
                    channel.as_str()
                )
            })?;

        if !channel.as_str().starts_with("dm_") {
            return self
                .sync_group_membership(channel, inviter, creator_did)
                .await
                .map_err(|e| e.to_string());
        }
        // Queue the invite; it is sent now if the tunnel is up and resent
        // until the invitee's runtime confirms it.
        self.send_queued(
            &ctx,
            invitee_runtime_id,
            OutboxItem::Invite {
                channel: channel.clone(),
                inviter: inviter.clone(),
                creator_did: creator_did.to_string(),
                invitee: invitee.clone(),
            },
        )
        .await
    }

    /// Announce the current roster to every remote principal after a group addition.
    async fn sync_group_membership(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        creator_did: &str,
    ) -> Result<()> {
        if channel.as_str().starts_with("dm_") {
            return Ok(());
        }
        let Some(ctx) = self.ctx.read().await.clone() else {
            return Ok(());
        };
        for member in self.local.list_remote_members(channel).await? {
            if !member.principal_id.starts_with("did:key:") {
                continue;
            }
            self.send_queued(
                &ctx,
                &member.runtime_id,
                OutboxItem::Invite {
                    channel: channel.clone(),
                    inviter: inviter.clone(),
                    creator_did: creator_did.into(),
                    invitee: PrincipalId(member.principal_id),
                },
            )
            .await
            .map_err(peko_channel::ChannelError::Adapter)?;
        }
        Ok(())
    }

    /// Build and sign the `TunnelChannelInvite` for `invitee` under
    /// `request_id`, from the channel's current meta and membership.
    async fn invite_envelope(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        request_id: &str,
        channel: &ChannelId,
        inviter: &PrincipalId,
        creator_did: &str,
        invitee: &PrincipalId,
        invitee_runtime_id: &str,
    ) -> std::result::Result<crate::tunnel::TunnelMessage, String> {
        let invitee_bare = invitee.0.clone();
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
        // ADR-058 D2: source-local principal rows are emitted under
        // the principal's vault-backed `did:key` DID when one exists —
        // the receiver files these rows as its `remote_members`, and
        // its inbound author gate (`is_remote_member`) matches the
        // event envelope's `source_principal_did`, which is the same
        // DID. Without the re-key the receiver would hold a local-id
        // row that a did:key-authored event can never match.
        let invitee_profile = self
            .local
            .actor_info(&Subject::Principal(invitee_bare.clone().into()))
            .await;
        let mut remote_addresses = Vec::new();
        for member in &remote {
            let subject = Subject::Principal(member.principal_id.clone().into());
            remote_addresses.push(self.local.actor_info(&subject).await.address);
        }
        let mut local_rows: Vec<peko_protocol::channel::InitialMember> = Vec::new();
        for p in &local {
            let profile = self.local.actor_info(p).await;
            if profile.address == invitee_profile.address
                || remote_addresses.contains(&profile.address)
            {
                continue;
            }
            if profile.hub_id.is_none() {
                return Err("Register channel participants before sharing through Hub".into());
            }
            let resolved = profile
                .hub_address()
                .unwrap_or_else(|| profile.address.clone());
            let mut wire = resolved
                .parse::<Subject>()
                .map(|s| subject_wire_form(&s))
                .unwrap_or_else(|_| subject_wire_form(p));
            if let Subject::Principal(did) = p {
                let pid = PrincipalId::from_did(did);
                if let Some(vault_did) = ctx.principal_keys.did_for_principal(&pid).await {
                    wire = vault_did;
                }
            }
            local_rows.push(peko_protocol::channel::InitialMember {
                hub_id: profile.hub_id,
                identity_key: Some(profile.identity_key),
                name: profile.name,
                principal_did: wire,
                runtime_id: Some(ctx.caller_runtime_id.clone()),
            });
        }
        let mut initial_members = local_rows;
        for member in remote
            .iter()
            .filter(|rm| !(rm.runtime_id == invitee_runtime_id && rm.principal_id == invitee_bare))
        {
            let subject = member
                .principal_id
                .parse::<Subject>()
                .unwrap_or_else(|_| Subject::Principal(member.principal_id.clone().into()));
            let profile = self.local.actor_info(&subject).await;
            initial_members.push(peko_protocol::channel::InitialMember {
                principal_did: member.principal_id.clone(),
                runtime_id: Some(member.runtime_id.clone()),
                hub_id: profile.hub_id,
                identity_key: Some(profile.identity_key),
                name: profile.name,
            });
        }
        let profile = self
            .local
            .actor_info(&Subject::Principal(invitee_bare.clone().into()))
            .await;
        initial_members.push(peko_protocol::channel::InitialMember {
            principal_did: invitee_bare.clone(),
            runtime_id: None,
            hub_id: profile.hub_id,
            identity_key: Some(profile.identity_key),
            name: profile.name,
        });

        // Serialize the snapshot once so the bytes signed are the
        // bytes the receiver will re-serialize and verify.
        let initial_members_bytes = serde_json::to_vec(&initial_members)
            .map_err(|e| format!("serialize initial_members for invite envelope: {e}"))?;

        let request_id = request_id.to_string();
        // ADR-058 D2: when the creator's DID is vault-backed (the DM
        // provisioning path passes the inviter's real `did:key`), the
        // envelope's `source_principal_did` carries that DID and the
        // envelope gains an author JWS over the same payload segment.
        // The bare `ChannelPort::invite` path passes a local id with
        // no vault key → legacy runtime-vouched invite.
        //
        // ADR-058 review fix (fail-closed): a `did:key` creator whose
        // signing key cannot be loaded must FAIL the invite — the
        // receiver's author gate drops unsigned did:key envelopes, so
        // silently downgrading would send an envelope the receiver
        // refuses while masking the vault failure here.
        let author_key = ctx.principal_keys.signing_key_for_did(creator_did).await;
        if author_key.is_none() {
            return Err(format!(
                "creator signing key unavailable for vault-backed creator {creator_did} \
                 (channel={}): refusing to send a runtime-vouched invite for a did:key creator",
                channel.as_str()
            ));
        }
        let creator_profile = self.local.actor_info(&Subject::from(inviter)).await;
        let creator_address = creator_profile
            .hub_address()
            .ok_or_else(|| "Register the inviter before using Hub transport".to_string())?;
        let source_principal_wire = if author_key.is_some() {
            creator_did.to_string()
        } else {
            inviter.to_string()
        };
        let signed = crate::tunnel::ChannelInviteSignedFields {
            request_id: &request_id,
            source_runtime_id: &ctx.caller_runtime_id,
            recipient_runtime_id: invitee_runtime_id,
            source_principal_did: &source_principal_wire,
            channel_id: channel.as_str(),
            creator: &creator_address,
            creator_did,
            name: &meta.name,
            passive_binding: meta.passive_binding.as_deref().unwrap_or(""),
            initial_members_bytes: &initial_members_bytes,
        };
        // ADR-058 D3: `iat = now`, `exp = now + 300` (seconds, UTC).
        let iat = chrono::Utc::now().timestamp();
        let (signature, author_signature) = crate::tunnel::sign_channel_invite(
            &ctx.signing_key,
            author_key.as_deref(),
            signed,
            iat,
            iat + 300,
        );

        // Audit on the source runtime side. The receiver's
        // `received_inbound` audit row is emitted by the
        // dispatcher's inbound handler.
        let preview_json = String::from_utf8_lossy(&initial_members_bytes).into_owned();
        crate::tunnel::emit_forwarded_outbound(
            &request_id,
            &ctx.caller_runtime_id,
            &inviter.to_string(),
            channel.as_str(),
            "channel_invite",
            &crate::tunnel::tunnel_channel_audit::preview_event_payload(&preview_json),
        );

        let envelope = crate::tunnel::TunnelMessage::TunnelChannelInvite {
            request_id,
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: invitee_runtime_id.to_string(),
            source_principal_did: source_principal_wire,
            channel_id: channel.as_str().to_string(),
            creator: creator_address,
            creator_did: creator_did.to_string(),
            name: meta.name.clone(),
            passive_binding: meta.passive_binding.clone(),
            initial_members,
            signature,
            author_signature,
        };

        Ok(envelope)
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
    async fn unhost_principal(&self, id: &PrincipalId) -> Result<()> {
        self.local.unhost_principal(id).await
    }

    async fn register_actor(
        &self,
        subject: &Subject,
        hub_id: Option<&str>,
        signing_did: Option<&str>,
        identity_key: Option<&str>,
        name: Option<&str>,
        registration: &str,
    ) -> Result<peko_channel::ActorInfo> {
        self.local
            .register_actor(
                subject,
                hub_id,
                signing_did,
                identity_key,
                name,
                registration,
            )
            .await
    }
    async fn known_actors(&self) -> Result<Vec<peko_channel::ActorInfo>> {
        self.local.known_actors().await
    }
    async fn known_contacts(
        &self,
        principal: &PrincipalId,
    ) -> Result<Vec<peko_channel::ActorInfo>> {
        self.local.known_contacts(principal).await
    }
    async fn remember_contact(&self, principal: &PrincipalId, actor: &Subject) -> Result<()> {
        self.local.remember_contact(principal, actor).await
    }
    async fn resolve_actor(&self, reference: &str) -> Result<peko_channel::ActorInfo> {
        self.local.resolve_actor(reference).await
    }

    async fn register_principal(&self, id: &PrincipalId, did: &str, name: &str) -> Result<()> {
        self.local.register_principal(id, did, name).await
    }
    async fn actor_info(&self, subject: &Subject) -> peko_channel::ActorInfo {
        self.local.actor_info(subject).await
    }
    async fn participants(&self, channel: &ChannelId) -> Result<Vec<peko_channel::ActorInfo>> {
        self.local.participants(channel).await
    }
    async fn binding_for(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
    ) -> Result<Option<String>> {
        self.local.binding_for(channel, principal).await
    }
    async fn ensure_dm(
        &self,
        principal: &PrincipalId,
        peer: &Subject,
        binding: &str,
    ) -> Result<ChannelId> {
        self.local.ensure_dm(principal, peer, binding).await
    }
    async fn create(&self, creator: &PrincipalId, opts: CreateOpts) -> Result<ChannelId> {
        self.local.create(creator, opts).await
    }

    async fn invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        invitee: &Subject,
    ) -> Result<()> {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return self.local.invite(channel, inviter, invitee).await;
        };
        let known = self.local.actor_info(invitee).await;
        let hosted = if let Some(local_id) = &known.local_id {
            ctx.principal_keys
                .did_for_principal(&PrincipalId(local_id.clone()))
                .await
                .is_some()
        } else {
            false
        };
        if (known.hub_id.is_none() && !invitee.subject_id().starts_with("hub:"))
            || !matches!(invitee, Subject::Principal(_))
            || hosted
        {
            self.local
                .invite(channel, inviter, &known.subject())
                .await?;
            if let Some(signer) = ctx.principal_keys.did_for_principal(inviter).await {
                self.sync_group_membership(channel, inviter, &signer)
                    .await?;
            }
            return Ok(());
        }
        let hub_id = known
            .hub_id
            .clone()
            .or_else(|| invitee.subject_id().strip_prefix("hub:").map(str::to_owned))
            .ok_or_else(|| {
                peko_channel::ChannelError::Adapter(
                    "Unknown principal; use Contacts.resolve with its Hub ID".into(),
                )
            })?;
        let creator_did = ctx
            .principal_keys
            .did_for_principal(inviter)
            .await
            .ok_or_else(|| {
                peko_channel::ChannelError::Adapter("Inviter signing key is unavailable".into())
            })?;
        let resolution = ctx
            .directory
            .resolve_by_hub_id_as(&hub_id, &creator_did)
            .await
            .map_err(|e| peko_channel::ChannelError::Adapter(e.to_string()))?;
        if resolution.hub_id != hub_id {
            return Err(peko_channel::ChannelError::Adapter(
                "Directory substituted a different actor".into(),
            ));
        }
        let subject = if resolution.runtime_id == ctx.caller_runtime_id {
            let id = PrincipalId(resolution.local_id.clone());
            let actor = self
                .local
                .resolve_actor(&format!("principal:local:{}", id.0))
                .await?;
            if actor.identity_key != resolution.identity_key
                || ctx.principal_keys.did_for_principal(&id).await.as_deref()
                    != Some(resolution.agent_did.as_str())
            {
                return Err(peko_channel::ChannelError::Adapter(
                    "Directory local identity does not match the hosted principal".into(),
                ));
            }
            Subject::from(&id)
        } else {
            Subject::Principal(format!("hub:{hub_id}").into())
        };
        let actor = self
            .local
            .register_actor(
                &subject,
                Some(&hub_id),
                Some(&resolution.agent_did),
                Some(&resolution.identity_key),
                None,
                if resolution.exposure == super::hub_directory::ResolvedExposure::Public {
                    "public"
                } else {
                    "private"
                },
            )
            .await?;
        self.local
            .invite(channel, inviter, &actor.subject())
            .await?;
        if resolution.runtime_id != ctx.caller_runtime_id {
            self.fanout_dm_invite(
                channel,
                inviter,
                &creator_did,
                &PrincipalId(resolution.agent_did),
                &resolution.runtime_id,
            )
            .await
            .map_err(peko_channel::ChannelError::Adapter)?;
        } else {
            self.sync_group_membership(channel, inviter, &creator_did)
                .await?;
        }
        Ok(())
    }

    async fn post(&self, channel: &ChannelId, sender: &Subject, msg: PostMsg) -> Result<TaskId> {
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

    async fn peek(&self, channel: &ChannelId, since: &Checkpoint) -> Result<Vec<ChannelEvent>> {
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

    /// Same pass-through rationale as `peek_tail`: the local store's
    /// page-aware scan, not the trait's full-walk default.
    async fn search(
        &self,
        channel: &ChannelId,
        query: &peko_channel::ChannelQuery,
    ) -> Result<peko_channel::SearchPage> {
        self.local.search(channel, query).await
    }

    async fn leave(&self, channel: &ChannelId, principal: &PrincipalId) -> Result<()> {
        let subject = Subject::from(principal);
        if !self.local.list_members(channel).await?.contains(&subject) {
            return Ok(());
        }
        self.local.leave(channel, principal).await?;
        let event = ChannelEvent::MemberLeft {
            channel: channel.clone(),
            member: subject.to_string(),
            at: chrono::Utc::now().to_rfc3339(),
        };
        self.fanout_event(channel, &event, &subject)
            .await
            .map_err(peko_channel::ChannelError::Adapter)
    }

    async fn list_members(&self, channel: &ChannelId) -> Result<Vec<Subject>> {
        // Preserve user/visitor kinds in mixed remote group snapshots. Bare
        // legacy IDs are principal IDs; prefixed actor addresses are typed.
        let mut subjects = self.local.list_members(channel).await?;
        subjects.extend(
            self.local
                .list_remote_members(channel)
                .await?
                .into_iter()
                .map(|r| {
                    r.principal_id
                        .parse::<Subject>()
                        .unwrap_or_else(|_| Subject::Principal(r.principal_id.into()))
                }),
        );
        let mut out = Vec::new();
        for subject in subjects {
            let local = self.local.actor_info(&subject).await.subject();
            if !out.contains(&local) {
                out.push(local);
            }
        }

        Ok(out)
    }

    async fn list_for_principal(&self, principal: &PrincipalId) -> Result<Vec<ChannelId>> {
        self.local.list_for_principal(principal).await
    }

    async fn membership(&self, channel: &ChannelId) -> Result<ChannelMembership> {
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

    /// Per-session digest read positions: delegate to the local
    /// store, same as `passive_binding` above — read marks are a
    /// local-persistence concern (the digest handler runs where the
    /// session's bound channel log lives), so there is no
    /// cross-runtime fan-out.
    async fn read_mark(
        &self,
        channel: &ChannelId,
        session_key: &str,
    ) -> Result<Option<peko_channel::port::TaskId>> {
        self.local.read_mark(channel, session_key).await
    }

    /// Advance the per-session digest read position on the local
    /// store (monotonic; see `ChannelStore::advance_read_mark`).
    async fn advance_read_mark(
        &self,
        channel: &ChannelId,
        session_key: &str,
        mark: peko_channel::port::TaskId,
    ) -> Result<()> {
        self.local
            .advance_read_mark(channel, session_key, mark)
            .await
    }

    /// Waiting lists are local state beside the read marks.
    async fn attention(
        &self,
        channel: &ChannelId,
        session_key: &str,
    ) -> Result<Vec<peko_channel::AttentionItem>> {
        self.local.attention(channel, session_key).await
    }

    async fn set_attention(
        &self,
        channel: &ChannelId,
        session_key: &str,
        items: Vec<peko_channel::AttentionItem>,
    ) -> Result<()> {
        self.local.set_attention(channel, session_key, items).await
    }

    async fn pin_to_shared(&self, channel: &ChannelId) -> Result<std::path::PathBuf> {
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
    use crate::tunnel::TunnelHandle;
    use peko_channel::{ChannelConfig, ChannelError};
    use peko_subject::PrincipalId;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn mixed_remote_members_keep_their_actor_kinds() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().into(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store.clone());
        let local = PrincipalId("prin_local".into());
        let channel = port
            .create(&local, CreateOpts::runtime("team"))
            .await
            .unwrap();
        for member in ["did:key:remote", "user:alice", "visitor:guest"] {
            store
                .add_remote_member(&channel, "remote-runtime", member)
                .await
                .unwrap();
        }
        let members = port.list_members(&channel).await.unwrap();
        assert_eq!(members.len(), 4);
        assert!(members.contains(&Subject::Principal("did:key:remote".into())));
        assert!(members.contains(&Subject::User("alice".into())));
        assert!(members.contains(&Subject::Visitor("guest".into())));
    }

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
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
            .await
            .unwrap();

        let remote_event = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_bob@runtime-B".into(),
            parent: None,
            text: "hello from B".into(),
            at: "2026-08-06T00:00:00Z".into(),
            via: None,
            msg_id: None,
            reply_to: None,
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
        let events = port.peek(&channel, &Checkpoint::default()).await.unwrap();
        assert_eq!(events.len(), 2, "Created + Posted = 2");
        assert!(
            events.iter().any(
                |ev| matches!(ev, ChannelEvent::Posted { text, .. } if text == "hello from B")
            ),
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
            via: None,
            msg_id: None,
            reply_to: None,
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
            .create(
                &PrincipalId("prin_alice".into()),
                CreateOpts::runtime("team"),
            )
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
            via: None,
            msg_id: None,
            reply_to: None,
        };
        port.append_remote_event(&channel, &remote_event)
            .await
            .expect("append_remote_event must succeed");

        // The subscriber must observe the event within a tight
        // timeout — no polling, no lag.
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
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
        port: &TunnelChannelPort,
        caller_runtime_id: &str,
    ) -> (
        Arc<CrossRuntimeChannelCtx>,
        mpsc::Receiver<crate::tunnel::TunnelMessage>,
    ) {
        use ed25519_dalek::SigningKey;
        let (tx, rx) = mpsc::channel::<crate::tunnel::TunnelMessage>(16);
        let handle = TunnelHandle::new(tx);
        let tunnel_slot: Arc<RwLock<Option<TunnelHandle>>> = Arc::new(RwLock::new(Some(handle)));
        let directory = Arc::new(FakeAgentDirectory::default());
        let remote_did = crate::tunnel::verifying_key_to_did_key(
            &SigningKey::from_bytes(&[10; 32]).verifying_key(),
        );
        directory.register_hub_id(
            "remote-hub",
            crate::tunnel::hub_directory::AgentResolution {
                name: None,
                hub_id: "remote-hub".into(),
                local_id: "prin_remote".into(),
                identity_key: "remote-key".into(),
                runtime_id: "did:key:zRuntimeB".into(),
                instance_id: "remote-instance".into(),
                agent_did: remote_did.clone(),
                owner_principal: Subject::Public,
                exposure: crate::tunnel::hub_directory::ResolvedExposure::Public,
            },
        );
        port.register_actor(
            &Subject::Principal("hub:remote-hub".into()),
            Some("remote-hub"),
            Some(&remote_did),
            Some("remote-key"),
            Some("Remote"),
            "public",
        )
        .await
        .unwrap();
        let signing_key = Arc::new(SigningKey::from_bytes(&[7u8; 32]));
        let keys = vec![
            (
                PrincipalId("prin_alice".into()),
                Arc::new(SigningKey::from_bytes(&[8; 32])),
            ),
            (
                PrincipalId("prin_bob_local".into()),
                Arc::new(SigningKey::from_bytes(&[9; 32])),
            ),
        ];
        for (id, key) in &keys {
            port.register_actor(
                &Subject::from(id),
                Some(&format!("hub-{}", id.0)),
                Some(&crate::tunnel::verifying_key_to_did_key(
                    &key.verifying_key(),
                )),
                None,
                Some(&id.0),
                "public",
            )
            .await
            .unwrap();
        }
        let ctx = Arc::new(CrossRuntimeChannelCtx {
            directory,
            signing_key,
            caller_runtime_id: caller_runtime_id.to_string(),
            tunnel: tunnel_slot,
            principal_keys: Arc::new(crate::tunnel::cross_runtime_channel::TestPrincipalKeys(
                keys,
            )),
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

        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
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
        let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("expected envelope within 1s")
            .expect("outbound TunnelChannelEvent must reach mock tunnel");
        let (
            request_id,
            source_runtime_id,
            recipient_runtime_id,
            source_principal_did,
            channel_id,
            event,
            signature,
            author_signature,
        ) = match env {
            crate::tunnel::TunnelMessage::TunnelChannelEvent {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                event,
                signature,
                author_signature,
            } => (
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                event,
                signature,
                author_signature,
            ),
            other => panic!("expected TunnelChannelEvent, got {other:?}"),
        };

        // Identity fields are populated from ctx.
        assert_eq!(source_runtime_id, "did:key:zRuntimeA");
        assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
        assert_eq!(
            source_principal_did,
            crate::tunnel::verifying_key_to_did_key(
                &ed25519_dalek::SigningKey::from_bytes(&[8; 32]).verifying_key()
            )
        );
        assert_eq!(channel_id, channel.as_str());
        // request_id is a fresh UUIDv4 (length 36 incl. hyphens).
        assert_eq!(request_id.len(), 36, "request_id should be a UUIDv4");
        assert!(!signature.is_empty(), "envelope must carry a signature");
        assert!(
            !author_signature.is_empty(),
            "Registered principal carries an author signature"
        );

        // The event body carries the post text + author.
        match &event {
            ChannelEvent::Posted { text, author, .. } => {
                assert_eq!(text, "hello from A");
                assert_eq!(author, "principal:hub:hub-prin_alice");
            }
            other => panic!("expected Posted in envelope, got {other:?}"),
        }

        // Signature verifies against ctx.signing_key (ADR-058 D3:
        // compact JWS whose payload carries every envelope field).
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let event_bytes = serde_json::to_vec(&event).unwrap();
        let (payload, _segment) =
            crate::tunnel::verify_channel_event(&ctx.signing_key.verifying_key(), &signature)
                .expect("outbound signature must verify");
        assert_eq!(payload.request_id, request_id);
        assert_eq!(payload.source_runtime_id, source_runtime_id);
        assert_eq!(payload.recipient_runtime_id, recipient_runtime_id);
        assert_eq!(payload.source_principal_did, source_principal_did);
        assert_eq!(payload.channel_id, channel_id);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&payload.event_b64u).unwrap(),
            event_bytes
        );
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

        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
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

        let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("expected envelope within 1s")
            .expect("exactly one envelope expected");
        assert!(
            matches!(env, crate::tunnel::TunnelMessage::TunnelChannelEvent { .. }),
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

        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
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
            let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("expected envelope within 1s")
                .expect("two envelopes expected");
            let src = match env {
                crate::tunnel::TunnelMessage::TunnelChannelEvent {
                    source_runtime_id, ..
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

        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
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
        let tunnel_slot: Arc<RwLock<Option<TunnelHandle>>> = Arc::new(RwLock::new(None));
        let ctx = Arc::new(CrossRuntimeChannelCtx {
            directory,
            signing_key,
            caller_runtime_id: "did:key:zRuntimeA".into(),
            tunnel: tunnel_slot,
            principal_keys: Arc::new(crate::tunnel::cross_runtime_channel::NoPrincipalKeys),
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

    /// The request id of a `TunnelChannelEvent`, plus its signature.
    fn event_ids(message: &crate::tunnel::TunnelMessage) -> (String, String) {
        match message {
            crate::tunnel::TunnelMessage::TunnelChannelEvent {
                request_id,
                signature,
                ..
            } => (request_id.clone(), signature.clone()),
            other => panic!("expected a channel event, got {other:?}"),
        }
    }

    /// A post made while the tunnel is down is kept, sent when the tunnel
    /// comes back under the same request id (signed afresh), resent on
    /// the next reconnect until confirmed, and forgotten once the
    /// recipient runtime confirms it. Another runtime cannot confirm it.
    #[tokio::test]
    async fn queued_posts_are_resent_until_their_recipient_confirms() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store);
        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
        let live = ctx.tunnel.write().await.take();
        port.set_ctx(Arc::clone(&ctx)).await;

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
        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_alice".into())),
            PostMsg::root("while offline").with_msg_id("msg_offline"),
        )
        .await
        .expect("a post never fails on a missing tunnel");
        assert!(rx.try_recv().is_err(), "nothing sent without a tunnel");
        assert!(
            !port
                .wait_delivered("msg_offline", std::time::Duration::from_millis(10))
                .await
        );

        // The tunnel comes back: the queued post goes out.
        *ctx.tunnel.write().await = live;
        port.on_tunnel_ready().await;
        let (request_id, first_signature) = event_ids(&rx.try_recv().unwrap());

        // Unconfirmed, it is sent again on the next reconnect — same
        // request id, fresh signature.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        port.on_tunnel_ready().await;
        let (again, second_signature) = event_ids(&rx.try_recv().unwrap());
        assert_eq!(again, request_id);
        assert_ne!(second_signature, first_signature, "re-signed for freshness");

        port.confirm_delivery("did:key:zRuntimeC", std::slice::from_ref(&request_id))
            .await;
        assert!(
            !port
                .wait_delivered("msg_offline", std::time::Duration::from_millis(10))
                .await,
            "only the recipient can confirm"
        );
        port.confirm_delivery("did:key:zRuntimeB", std::slice::from_ref(&request_id))
            .await;
        assert!(
            port.wait_delivered("msg_offline", std::time::Duration::from_millis(10))
                .await
        );
        port.on_tunnel_ready().await;
        assert!(rx.try_recv().is_err(), "a confirmed post is not resent");

        // The queue is on disk: a fresh port after a restart has nothing.
        let restarted = TunnelChannelPort::new(Arc::clone(port.local()));
        assert!(
            restarted
                .wait_delivered("msg_offline", std::time::Duration::from_millis(10))
                .await
        );
    }

    /// Envelopes queued before a restart go out when the restarted
    /// runtime gets its tunnel.
    #[tokio::test]
    async fn queued_posts_survive_a_restart() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(Arc::clone(&store));
        let (ctx, _rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
        ctx.tunnel.write().await.take();
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
        port.post(
            &channel,
            &Subject::from(&PrincipalId("prin_alice".into())),
            PostMsg::root("before the restart"),
        )
        .await
        .unwrap();

        let restarted = TunnelChannelPort::new(store);
        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
        restarted.set_ctx(ctx).await;
        let message = rx.try_recv().expect("sent once the tunnel is back");
        let crate::tunnel::TunnelMessage::TunnelChannelEvent {
            recipient_runtime_id,
            event,
            ..
        } = message
        else {
            panic!("expected a channel event");
        };
        assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
        assert!(
            matches!(event, ChannelEvent::Posted { ref text, .. } if text == "before the restart")
        );
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
    async fn group_additions_update_existing_remote_rosters_and_leaves_fan_out() {
        let temp = TempDir::new().unwrap();
        let port =
            TunnelChannelPort::new(Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
                runtime_dir: temp.path().into(),
                shared_dir: None,
            })));
        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
        port.set_ctx(ctx).await;
        let alice = PrincipalId("prin_alice".into());
        let bob = PrincipalId("prin_bob_local".into());
        let channel = port
            .create(&alice, CreateOpts::runtime("team"))
            .await
            .unwrap();
        port.invite(
            &channel,
            &alice,
            &"principal:hub:remote-hub".parse().unwrap(),
        )
        .await
        .unwrap();
        let _ = rx.recv().await.unwrap();
        port.invite(&channel, &alice, &Subject::from(&bob))
            .await
            .unwrap();
        match rx.recv().await.unwrap() {
            crate::tunnel::TunnelMessage::TunnelChannelInvite {
                initial_members, ..
            } => {
                assert_eq!(initial_members.len(), 3);
                assert!(initial_members
                    .iter()
                    .any(|m| m.hub_id.as_deref() == Some("hub-prin_bob_local")));
            }
            other => panic!("Expected updated roster, got {other:?}"),
        }
        port.leave(&channel, &bob).await.unwrap();
        match rx.recv().await.unwrap() {
            crate::tunnel::TunnelMessage::TunnelChannelEvent {
                event: ChannelEvent::MemberLeft { member, .. },
                author_signature,
                ..
            } => {
                assert_eq!(member, "principal:hub:hub-prin_bob_local");
                assert!(!author_signature.is_empty());
            }
            other => panic!("Expected signed leave, got {other:?}"),
        }
    }

    async fn registered_outbound_invite(dm: bool) {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(peko_channel::ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().into(),
            shared_dir: None,
        }));
        let port = TunnelChannelPort::new(store);
        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeA").await;
        port.set_ctx(ctx.clone()).await;
        let inviter = PrincipalId("prin_alice".into());
        let signer = ctx
            .principal_keys
            .did_for_principal(&inviter)
            .await
            .unwrap();
        let channel = if dm {
            port.ensure_dm(
                &inviter,
                &Subject::Principal("hub:remote-hub".into()),
                "/remote",
            )
            .await
            .unwrap()
        } else {
            port.create(&inviter, CreateOpts::runtime("team"))
                .await
                .unwrap()
        };
        port.invite(
            &channel,
            &inviter,
            &"principal:hub:remote-hub".parse().unwrap(),
        )
        .await
        .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let crate::tunnel::TunnelMessage::TunnelChannelInvite {
            creator,
            creator_did,
            source_principal_did,
            recipient_runtime_id,
            initial_members,
            passive_binding,
            signature,
            author_signature,
            ..
        } = message
        else {
            panic!("expected signed invite")
        };
        assert_eq!(creator, "principal:hub:hub-prin_alice");
        assert_eq!(creator_did, signer);
        assert_eq!(source_principal_did, signer);
        assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
        assert_eq!(passive_binding.is_some(), dm);
        assert!(initial_members
            .iter()
            .all(|m| m.hub_id.is_some() && m.identity_key.is_some()));
        let (payload, segment) =
            crate::tunnel::verify_channel_invite(&ctx.signing_key.verifying_key(), &signature)
                .unwrap();
        crate::tunnel::verify_author_signature(&author_signature, &segment, &signer).unwrap();
        assert_eq!(payload.creator, creator);
        assert_eq!(
            port.local
                .list_remote_members(&channel)
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn invite_with_remote_invitee_emits_signed_envelope() {
        registered_outbound_invite(false).await;
    }
    #[tokio::test]
    async fn dm_invite_carries_both_ids_and_a_channel_binding_marker() {
        registered_outbound_invite(true).await;
    }

    /// The DM entry point: a bound channel invited via
    /// `fanout_dm_invite` with the creator's real DID emits an
    /// envelope whose `creator_did` + `passive_binding` DM marker
    /// sign and verify.
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
        let (ctx, mut rx) = build_test_ctx(&port, "did:key:zRuntimeB").await;
        port.set_ctx(ctx).await;

        let channel = ChannelId("chan_mirror01".to_string());
        let initial_members = vec![
            peko_protocol::channel::InitialMember {
                hub_id: None,
                identity_key: None,
                name: None,
                principal_did: "prin_alice".to_string(),
                runtime_id: Some("did:key:zRuntimeA".to_string()),
            },
            peko_protocol::channel::InitialMember {
                hub_id: None,
                identity_key: None,
                name: None,
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
        let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("expected envelope within 1s")
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
