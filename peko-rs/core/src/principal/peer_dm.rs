//! Peer DM channel auto-provisioning (agent-session paradigm, sprint
//! 3 Phase 10).
//!
//! Every peer who gets a standing child session on first contact
//! ([`crate::principal::peer_children::ensure_peer_child`]) also gets a
//! 1:1 **DM channel** — the channel-tier home of that peer's
//! conversation with the principal:
//!
//! - name: `dm-<peer_child_slug>` (e.g. `dm-local-user`, `dm-user-a`,
//!   `dm-principal-<fragment>`), matching the fixture convention the
//!   passive-binding tests already use;
//! - `passive_binding`: the peer child's `/`-path (`/<slug>`) — the
//!   shape the `PassiveBindingResponder` fixtures
//!   (`daemon::channel_binding`) and `SessionStoreBindingResolver`'s
//!   `/`-path resolution both speak. Raw session ids would also pass
//!   the resolver through, but the path form is the established
//!   convention and stays human-readable in `meta.json`;
//! - membership: the principal itself (the channel creator is
//!   auto-added as first member by `ChannelStore::create` — the
//!   responder's reply post and the `ChannelSend` tool both post with
//!   sender = the principal and require membership).
//!
//! ## Remote principals (Phase 12 gap)
//!
//! For `principal:<did>` peers only the LOCAL channel is provisioned.
//! Cross-runtime invite / `join_remote` fan-out (so the remote
//! runtime mirrors the DM channel) is deliberately NOT wired here —
//! that is sprint 3 Phase 12.
//!
//! ## Concurrency
//!
//! `ensure_peer_child` runs on EVERY ingress, so the channel check
//! must be cheap and race-tolerant. The scan side is
//! `list_for_principal` + one `meta.json` read per channel (small;
//! channels per principal are few). The create side is NOT name- or
//! binding-checked by the store (`ChannelStore::create` always
//! generates a fresh `ChannelId`), so find-or-create is serialized by
//! a caller-supplied per-principal `Mutex` — the same
//! `session_creation_lock` the manager already uses to serialize
//! first-contact session work. Two concurrent first-contacts for the
//! same peer then cannot double-create.
//!
//! ## Live subscriber
//!
//! The caller fires the [`PeerDmSubscriberHook`] when (and only when)
//! this helper reports `created: true`; the daemon installs a hook
//! that forwards to
//! `ChannelBindingSupervisor::ensure_subscriber`, so the new channel
//! gets its `PassiveBindingResponder` subscriber immediately —
//! without waiting for the next boot sweep.
//!
//! ## Conversation home (Phase 11)
//!
//! Phase 11 routes the actual peer conversation onto the DM channel:
//! the ingress handlers post the inbound message attributed to the
//! peer ([`post_peer_dm_inbound`]) and the reply back as the
//! principal ([`post_peer_dm_reply`]); `peko log` reads the channel
//! log back through [`find_peer_dm_channel`]. The responder's
//! author-based skip rule (`daemon::channel_binding`) keeps it from
//! double-driving turns on these posts.

use std::sync::Arc;

use anyhow::Result;
use peko_auth::Subject;
use peko_channel::{ChannelId, ChannelPort, CreateOpts, PostMsg};
use peko_session::manager::SessionManager;
use peko_subject::PrincipalId;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

use crate::principal::peer_children::peer_child_slug;

/// Post-create kickoff for a freshly provisioned DM channel. The
/// daemon installs a closure forwarding to
/// `ChannelBindingSupervisor::ensure_subscriber`; a plain `Fn` keeps
/// `principal` free of any `daemon` module dependency.
pub(crate) type PeerDmSubscriberHook = Arc<dyn Fn(PrincipalId, ChannelId) + Send + Sync>;

/// Outcome of [`ensure_peer_dm_channel`]: the DM channel id and
/// whether THIS call created it (the hook fires only on `created`).
#[derive(Debug, Clone)]
pub(crate) struct PeerDmProvision {
    pub channel: ChannelId,
    pub created: bool,
}

/// Read the peer child's REAL slug back from session metadata (the
/// `-N` collision suffix case means the derived base slug is only a
/// fallback). Factored out of [`ensure_peer_dm_channel`] so the Phase
/// 12a cross-runtime DM mirror bootstrap
/// (`TunnelHost::dm_channel_mirror_bootstrap`) derives the
/// receiver-local binding from the same source of truth.
pub(crate) async fn peer_child_slug_readback(
    session_manager: &Arc<RwLock<SessionManager>>,
    child_id: &str,
    peer: &Subject,
) -> Result<String> {
    let metas = session_manager
        .write()
        .await
        .list_all_sessions(false)
        .await?;
    match metas
        .iter()
        .find(|m| m.session_id == child_id)
        .and_then(|m| m.slug.clone())
    {
        Some(slug) => Ok(slug),
        // `ensure_peer_child` guarantees a slug was assigned; fall
        // back to the derived base slug if the metadata read somehow
        // comes up empty.
        None => peer_child_slug(peer),
    }
}

/// Find-or-create the peer's DM channel. Idempotent per peer: a
/// second call returns the same channel with `created: false`.
///
/// `child_id` is the peer's standing-child session id (the
/// [`crate::principal::peer_children::ensure_peer_child`] result); the
/// child's actual slug (which may carry a `-N` collision suffix) is
/// read back from session metadata so both the channel name and the
/// binding path track the real child. `lock` serializes the
/// find-or-create per principal (module docs, "Concurrency").
pub(crate) async fn ensure_peer_dm_channel(
    port: &Arc<dyn ChannelPort>,
    principal: &PrincipalId,
    peer: &Subject,
    child_id: &str,
    session_manager: &Arc<RwLock<SessionManager>>,
    lock: &Mutex<()>,
) -> Result<PeerDmProvision> {
    let slug = peer_child_slug_readback(session_manager, child_id, peer).await?;
    let name = format!("dm-{slug}");
    let binding = format!("/{slug}");

    let _guard = lock.lock().await;

    // Find: a channel whose binding IS this child's path is the peer's
    // DM channel (see `find_peer_dm_channel`).
    if let Some(existing) = find_peer_dm_channel(port, principal, &binding).await? {
        return Ok(PeerDmProvision {
            channel: existing,
            created: false,
        });
    }

    // Create: the principal is the creator (auto-member). For
    // `principal:<did>` peers this provisions the LOCAL channel only —
    // cross-runtime invite/`join_remote` fan-out is Phase 12.
    let channel = port
        .create(
            principal,
            CreateOpts::runtime(name.clone()).with_passive_binding(binding.clone()),
        )
        .await?;
    tracing::info!(
        channel = %channel,
        name = %name,
        binding = %binding,
        peer = %peer,
        "peer DM channel provisioned"
    );
    Ok(PeerDmProvision {
        channel,
        created: true,
    })
}

/// Find-only variant of [`ensure_peer_dm_channel`]: returns the
/// channel whose passive binding IS `binding` (the semantic identity,
/// not the display name), or `None` when the principal has no such
/// channel. Per-channel read failures (a partially written channel
/// dir) skip that channel rather than failing the whole scan.
///
/// Phase 11: used by `peko log` (`read_principal_log`) to locate the
/// peer's DM channel without provisioning one.
pub(crate) async fn find_peer_dm_channel(
    port: &Arc<dyn ChannelPort>,
    principal: &PrincipalId,
    binding: &str,
) -> Result<Option<ChannelId>> {
    for candidate in port.list_for_principal(principal).await? {
        match port.passive_binding(&candidate).await {
            Ok(Some(existing)) if existing == binding => {
                return Ok(Some(candidate));
            }
            Ok(_) => {}
            Err(e) => {
                debug!(
                    channel = %candidate,
                    "peer DM lookup: skipping channel with unreadable binding: {e}"
                );
            }
        }
    }
    Ok(None)
}

/// Phase 11: post a peer's inbound message to the DM channel,
/// attributed to the peer. `sender` stays the principal (the
/// channel's creator/member — the human peer is deliberately not a
/// member); `author` is the peer's Subject wire form
/// (`peer.to_string()`: `user:alice`, `user:local`, `principal:did:…`),
/// which is also what the responder's author-based skip rule matches
/// so it never double-drives the turn.
pub(crate) async fn post_peer_dm_inbound(
    port: &Arc<dyn ChannelPort>,
    principal: &PrincipalId,
    channel: &ChannelId,
    author: &str,
    text: &str,
) -> Result<()> {
    port.post_attributed(channel, principal, author, PostMsg::root(text))
        .await?;
    Ok(())
}

/// Phase 11: post the principal's reply back to the DM channel
/// (plain `post` — author = `principal.id`, e.g. `prin_<uuid>`).
///
/// Warn-only on failure, mirroring the failure posture of the
/// responder's reply post (`daemon::channel_binding::ResponderInner`):
/// the reply has already been delivered over the IPC stream; the
/// channel row is the durable projection, not the delivery mechanism.
/// Empty/whitespace replies are skipped (mirrors the responder's own
/// guard).
pub(crate) async fn post_peer_dm_reply(
    port: &Arc<dyn ChannelPort>,
    principal: &PrincipalId,
    channel: &ChannelId,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    if let Err(e) = port.post(channel, principal, PostMsg::root(text)).await {
        warn!(
            channel = %channel,
            "peer DM reply projection failed (reply already delivered): {e}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::peer_children::ensure_peer_child;
    use peko_channel::{ChannelConfig, ChannelStore};

    async fn fixture() -> (
        tempfile::TempDir,
        Arc<RwLock<SessionManager>>,
        Arc<ChannelStore>,
        Arc<dyn ChannelPort>,
        Mutex<()>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new().with_sessions_dir_internal(dir.path().join("sessions"));
        let store = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: dir.path().join("runtime"),
            shared_dir: None,
        }));
        let port: Arc<dyn ChannelPort> = store.clone();
        (
            dir,
            Arc::new(RwLock::new(manager)),
            store,
            port,
            Mutex::new(()),
        )
    }

    fn owner() -> Subject {
        Subject::User("local".to_string())
    }

    fn principal_id() -> PrincipalId {
        PrincipalId("prin_self".to_string())
    }

    #[tokio::test]
    async fn creates_dm_channel_named_and_bound_to_the_peer_child() {
        let (_dir, manager, store, port, lock) = fixture().await;
        let peer = Subject::User("alice".to_string());
        let child_id = ensure_peer_child("root", &owner(), &peer, &manager)
            .await
            .unwrap();

        let provision =
            ensure_peer_dm_channel(&port, &principal_id(), &peer, &child_id, &manager, &lock)
                .await
                .unwrap();
        assert!(provision.created);

        // Name + binding track the peer child.
        let binding = store.passive_binding(&provision.channel).await.unwrap();
        assert_eq!(binding.as_deref(), Some("/user-alice"));
        let membership = store.membership(&provision.channel).await.unwrap();
        assert_eq!(membership.name, "dm-user-alice");
        // The principal is a member (creator auto-added) — the
        // responder's reply post + ChannelSend both require it.
        let members = store.list_members(&provision.channel).await.unwrap();
        assert_eq!(members, vec![principal_id()]);
    }

    #[tokio::test]
    async fn suffixed_child_slug_tracks_through_to_channel_name_and_binding() {
        let (_dir, manager, store, port, lock) = fixture().await;
        // Two peers whose ids sanitize to the same base slug: the
        // second gets a `-2` child, and its DM channel must follow.
        let peer_a = Subject::User("foo-bar".to_string());
        let peer_b = Subject::User("foo bar".to_string());
        let a_id = ensure_peer_child("root", &owner(), &peer_a, &manager)
            .await
            .unwrap();
        let b_id = ensure_peer_child("root", &owner(), &peer_b, &manager)
            .await
            .unwrap();

        let a = ensure_peer_dm_channel(&port, &principal_id(), &peer_a, &a_id, &manager, &lock)
            .await
            .unwrap();
        let b = ensure_peer_dm_channel(&port, &principal_id(), &peer_b, &b_id, &manager, &lock)
            .await
            .unwrap();
        assert_ne!(a.channel, b.channel);
        assert_eq!(
            store.passive_binding(&a.channel).await.unwrap().as_deref(),
            Some("/user-foo-bar")
        );
        assert_eq!(
            store.passive_binding(&b.channel).await.unwrap().as_deref(),
            Some("/user-foo-bar-2")
        );
        assert_eq!(
            store.membership(&b.channel).await.unwrap().name,
            "dm-user-foo-bar-2"
        );
    }

    #[tokio::test]
    async fn second_call_is_idempotent() {
        let (_dir, manager, _store, port, lock) = fixture().await;
        let peer = Subject::User("alice".to_string());
        let child_id = ensure_peer_child("root", &owner(), &peer, &manager)
            .await
            .unwrap();

        let first =
            ensure_peer_dm_channel(&port, &principal_id(), &peer, &child_id, &manager, &lock)
                .await
                .unwrap();
        let second =
            ensure_peer_dm_channel(&port, &principal_id(), &peer, &child_id, &manager, &lock)
                .await
                .unwrap();
        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.channel, second.channel);
        assert_eq!(
            port.list_for_principal(&principal_id())
                .await
                .unwrap()
                .len(),
            1,
            "idempotent ensure must not create a second channel"
        );
    }

    /// Concurrent first-contact ensures for the same peer (the two
    /// racing IPC/A2A arrivals case) produce exactly ONE channel when
    /// the caller serializes on the shared per-principal lock.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_ensures_create_exactly_one_channel() {
        let (_dir, manager, _store, port, _lock) = fixture().await;
        let peer = Subject::User("alice".to_string());
        let child_id = ensure_peer_child("root", &owner(), &peer, &manager)
            .await
            .unwrap();

        let shared_lock = Arc::new(Mutex::new(()));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let port = Arc::clone(&port);
            let manager = Arc::clone(&manager);
            let lock = Arc::clone(&shared_lock);
            let principal_id = principal_id();
            let peer = peer.clone();
            let child_id = child_id.clone();
            handles.push(tokio::spawn(async move {
                ensure_peer_dm_channel(&port, &principal_id, &peer, &child_id, &manager, &lock)
                    .await
            }));
        }
        let results = futures::future::join_all(handles).await;
        let channels: std::collections::HashSet<ChannelId> = results
            .into_iter()
            .map(|r| r.unwrap().unwrap().channel)
            .collect();
        assert_eq!(
            channels.len(),
            1,
            "all ensures must converge on one channel"
        );
        assert_eq!(
            port.list_for_principal(&principal_id())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The owner peer (`user:local`) provisions `dm-local-user` bound
    /// to `/local-user` — the privileged owner child.
    #[tokio::test]
    async fn owner_peer_dm_uses_local_user_slug() {
        let (_dir, manager, store, port, lock) = fixture().await;
        let peer = owner();
        let child_id = ensure_peer_child("root", &owner(), &peer, &manager)
            .await
            .unwrap();

        let provision =
            ensure_peer_dm_channel(&port, &principal_id(), &peer, &child_id, &manager, &lock)
                .await
                .unwrap();
        assert!(provision.created);
        assert_eq!(
            store
                .passive_binding(&provision.channel)
                .await
                .unwrap()
                .as_deref(),
            Some("/local-user")
        );
        assert_eq!(
            store.membership(&provision.channel).await.unwrap().name,
            "dm-local-user"
        );
    }

    // -- Phase 11: find-only lookup + post helpers ---------------------

    /// `find_peer_dm_channel` hits on the provisioned binding and
    /// misses on an unknown one — without creating anything.
    #[tokio::test]
    async fn find_peer_dm_channel_hit_and_miss() {
        let (_dir, manager, _store, port, lock) = fixture().await;
        let peer = Subject::User("alice".to_string());
        let child_id = ensure_peer_child("root", &owner(), &peer, &manager)
            .await
            .unwrap();

        // Miss: nothing provisioned yet.
        assert!(find_peer_dm_channel(&port, &principal_id(), "/user-alice")
            .await
            .unwrap()
            .is_none());

        let provision =
            ensure_peer_dm_channel(&port, &principal_id(), &peer, &child_id, &manager, &lock)
                .await
                .unwrap();

        // Hit on the binding.
        assert_eq!(
            find_peer_dm_channel(&port, &principal_id(), "/user-alice")
                .await
                .unwrap(),
            Some(provision.channel.clone())
        );
        // Still a miss for a different binding; no new channels created.
        assert!(find_peer_dm_channel(&port, &principal_id(), "/user-bob")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            port.list_for_principal(&principal_id())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// `post_peer_dm_inbound` writes the peer's Subject wire form as
    /// the event author while `sender` remains the principal; a
    /// follow-up `post_peer_dm_reply` lands as the principal.
    #[tokio::test]
    async fn post_helpers_land_with_expected_authors() {
        let (_dir, manager, store, port, lock) = fixture().await;
        let peer = Subject::User("alice".to_string());
        let child_id = ensure_peer_child("root", &owner(), &peer, &manager)
            .await
            .unwrap();
        let provision =
            ensure_peer_dm_channel(&port, &principal_id(), &peer, &child_id, &manager, &lock)
                .await
                .unwrap();

        post_peer_dm_inbound(
            &port,
            &principal_id(),
            &provision.channel,
            &peer.to_string(),
            "hello there",
        )
        .await
        .unwrap();
        post_peer_dm_reply(&port, &principal_id(), &provision.channel, "hi alice").await;

        let events = store
            .peek(&provision.channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        let posted: Vec<(String, String)> = events
            .iter()
            .filter_map(|ev| match ev {
                peko_channel::ChannelEvent::Posted { author, text, .. } => {
                    Some((author.clone(), text.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            posted,
            vec![
                ("user:alice".to_string(), "hello there".to_string()),
                ("prin_self".to_string(), "hi alice".to_string()),
            ]
        );
    }

    /// Empty/whitespace replies are skipped (mirror of the
    /// responder's own guard) — no event lands.
    #[tokio::test]
    async fn post_peer_dm_reply_skips_empty_text() {
        let (_dir, manager, store, port, lock) = fixture().await;
        let peer = Subject::User("alice".to_string());
        let child_id = ensure_peer_child("root", &owner(), &peer, &manager)
            .await
            .unwrap();
        let provision =
            ensure_peer_dm_channel(&port, &principal_id(), &peer, &child_id, &manager, &lock)
                .await
                .unwrap();

        post_peer_dm_reply(&port, &principal_id(), &provision.channel, "   ").await;

        let events = store
            .peek(&provision.channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .all(|ev| !matches!(ev, peko_channel::ChannelEvent::Posted { .. })),
            "whitespace reply must not land in the channel log"
        );
    }
}
