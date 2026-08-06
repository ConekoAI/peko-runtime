//! `TunnelChannelPort` — `ChannelPort` impl that adds cross-runtime
//! fan-out on top of a local `ChannelStore`.
//!
//! peko-channel cross-runtime PR-B commit 2. This commit ships the
//! skeleton + the inbound append path. The outbound fan-out (the
//! "send a `TunnelChannelEvent` to every remote member of this
//! channel" path) lands in commit 3 alongside the
//! `CrossRuntimeChannelCtx` wiring.
//!
//! ## Why this lives in `peko-core` (not `peko-channel`)
//!
//! `TunnelChannelPort` reaches into [`crate::tunnel`] for the
//! `TunnelMessage` enum, the signing key, and (in commit 3) the live
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
//! - **Outbound** (commit 3): `post` / `invite` will iterate the
//!   channel's `remote_members` and push a signed
//!   `TunnelChannelEvent` to each recipient runtime. That requires
//!   holding an `Arc<CrossRuntimeChannelCtx>`; this commit does NOT
//!   take one (commit 3 adds the field).

use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::{
    ChannelId, ChannelMembership, ChannelPort, Checkpoint, CreateOpts, PostMsg, Result, Tier,
};
use peko_protocol::channel::ChannelEvent;
use peko_subject::PrincipalId;

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
/// [`TunnelChannelPort::new`]; the outbound `ctx` field is added in
/// commit 3 alongside the fan-out code.
#[derive(Clone)]
pub struct TunnelChannelPort {
    /// File-backed local mirror. All `ChannelPort` operations
    /// delegate here — `TunnelChannelPort` does not duplicate the
    /// append-only JSONL logic, it just adds cross-runtime routing
    /// around it.
    local: Arc<peko_channel::ChannelStore>,

    /// Cross-runtime dispatch context. **Reserved for commit 3.** The
    /// outbound `post` / `invite` paths will read the tunnel handle
    /// from `ctx.tunnel` and the directory from `ctx.directory` to
    /// route the fan-out. Commit 2 keeps the field unused (but
    /// present) so commit 3 can land as a single self-contained diff
    /// that doesn't touch `AppState::with_data_dir` again.
    ///
    /// `None` means "no cross-runtime fan-out wired yet" — tests and
    /// pre-commit-3 production builds pass `None`. Commit 3 flips
    /// production to `Some(...)`.
    #[allow(dead_code)]
    ctx: Option<Arc<CrossRuntimeChannelCtx>>,
}

impl std::fmt::Debug for TunnelChannelPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelChannelPort")
            .field("local", &"<Arc<ChannelStore>>")
            .field("ctx", &self.ctx.is_none())
            .finish()
    }
}

impl TunnelChannelPort {
    /// Construct a `TunnelChannelPort` that wraps `local`. Pass `None`
    /// for `ctx` when the cross-runtime fan-out is not wired (tests,
    /// commit 2 default). Production callers pass
    /// `Some(Arc::new(ctx))` — commit 3 lands that change.
    #[must_use]
    pub fn new(
        local: Arc<peko_channel::ChannelStore>,
        ctx: Option<Arc<CrossRuntimeChannelCtx>>,
    ) -> Self {
        Self { local, ctx }
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

    /// Borrow the optional cross-runtime ctx (commit 3 will use it).
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn ctx(&self) -> Option<&Arc<CrossRuntimeChannelCtx>> {
        self.ctx.as_ref()
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
        // Commit 3 adds the outbound fan-out here: after the local
        // store accepts the write, iterate `list_remote_members` and
        // push a signed `TunnelChannelEvent` to each recipient.
        self.local.post(channel, sender, msg).await
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
        // Commit 3 will union with `list_remote_members` here so
        // cross-runtime peers show up alongside local ones.
        self.local.list_members(channel).await
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
    use peko_channel::{ChannelConfig, ChannelError};
    use peko_subject::PrincipalId;
    use tempfile::TempDir;

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
        let port = TunnelChannelPort::new(store.clone(), None);

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
        let port = TunnelChannelPort::new(store, None);

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
        let port = TunnelChannelPort::new(store, None);

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
        let port = TunnelChannelPort::new(store, None);
        // No-ctx port returns `None` for `ctx()`.
        assert!(port.ctx().is_none());

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
        let port = TunnelChannelPort::new(store, None);

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
}