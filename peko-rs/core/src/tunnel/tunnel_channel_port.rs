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
    ChannelId, ChannelMembership, ChannelPort, Checkpoint, CreateOpts, PostMsg, Result,
};
use tokio::sync::RwLock;
use peko_protocol::channel::ChannelEvent;
use peko_subject::PrincipalId;
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
    /// `source_principal_did` is the local principal who authored
    /// the event (the `sender` for `Posted`, the `invitee` /
    /// `inviter` for `MemberJoined`/`MemberLeft`). The signature
    /// pre-image always carries the runtime-level identity
    /// (`source_runtime_id`); the principal-DID is recorded for
    /// audit correlation on the receiving runtime.
    async fn fanout_event(
        &self,
        channel: &ChannelId,
        ev: &ChannelEvent,
        source_principal_did: &PrincipalId,
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
                source_principal_did: &source_principal_did.to_string(),
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
                &source_principal_did.to_string(),
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
                source_principal_did: source_principal_did.to_string(),
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
    /// persist it. Used by the outbound `invite` path when the
    /// directory resolves the invitee to a non-self runtime. Called
    /// before the fan-out so the receiver-side append has a
    /// matching row in the local mirror.
    #[allow(dead_code)]
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

    /// Bootstrap a local mirror for a cross-runtime channel the
    /// receiver was invited to. Called by the dispatcher on inbound
    /// `TunnelChannelInvite` envelopes (peko-channel cross-runtime
    /// PR-3a commit 2) **after** the signature has verified — the
    /// caller is responsible for that gate.
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
    #[allow(dead_code)]
    pub(crate) async fn join_remote(
        &self,
        channel: &ChannelId,
        creator: &str,
        name: &str,
        initial_members: &[peko_protocol::channel::InitialMember],
    ) -> Result<()> {
        self.local
            .join_remote(channel, creator, name, initial_members)
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
        invitee: &PrincipalId,
    ) -> Result<()> {
        self.local.invite(channel, inviter, invitee).await
    }

    async fn post(
        &self,
        channel: &ChannelId,
        sender: &PrincipalId,
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
    ) -> Result<Vec<PrincipalId>> {
        // Merge local + remote. The `RemoteMember.principal_id` is
        // already a stringified `PrincipalId` (the on-disk shape is
        // the same as the local `members` rows), so a parse-free
        // re-wrap suffices.
        let mut out = self.local.list_members(channel).await?;
        for rm in self.local.list_remote_members(channel).await? {
            out.push(PrincipalId(rm.principal_id));
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

    async fn pin_to_shared(
        &self,
        channel: &ChannelId,
    ) -> Result<std::path::PathBuf> {
        self.local.pin_to_shared(channel).await
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
            &PrincipalId("prin_carol".into()),
        )
        .await
        .unwrap();

        let line = port
            .post(
                &channel,
                &creator,
                PostMsg::root("hello world"),
            )
            .await
            .unwrap();
        assert_eq!(line, "2", "Created=0, MemberJoined=1, Posted=2");

        let members = port.list_members(&channel).await.unwrap();
        let member_strs: Vec<&str> = members.iter().map(|p| p.0.as_str()).collect();
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
            &PrincipalId("prin_alice".into()),
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
                &PrincipalId("prin_alice".into()),
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
            &PrincipalId("prin_alice".into()),
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
            &PrincipalId("prin_alice".into()),
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
            &PrincipalId("prin_alice".into()),
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
                &PrincipalId("prin_alice".into()),
                PostMsg::root("patience"),
            )
            .await
            .expect("post must succeed even without tunnel");
        assert_eq!(line, "1");
    }
}