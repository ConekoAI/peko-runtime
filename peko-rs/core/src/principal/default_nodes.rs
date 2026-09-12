//! Create-once default session nodes (`/tmp`, `/trash`).
//!
//! Every new principal is seeded with two ordinary standing sessions
//! under its (still-dangling) trunk:
//!
//! - `/tmp` — a parking spot for TRANSIENT, single-use sessions.
//!   Work sessions that exist to be thrown away go here, keeping the
//!   rest of the session tree clean and organized.
//! - `/trash` — the destination for sessions slated for removal.
//!   `session move` stages a session (recursively — the subtree moves
//!   with it) into `/trash`; a later `session remove recursive:true`
//!   performs the permanent purge.
//!
//! ## Semantics (deliberate)
//!
//! - **Create-once.** Seeding happens ONLY in
//!   `PrincipalManager::create` — never at boot, never at root-agent
//!   run setup. A principal that removes either node keeps it removed
//!   (the Session tool has no create action, so there is no built-in
//!   resurrection path). This is the essential difference from
//!   `children::ensure_declared_children`, which re-creates missing
//!   declared children on every root run.
//! - **No reserved semantics.** The nodes are ordinary sessions:
//!   renameable, moveable, removable, subject to the same guards.
//!   No tool action, guard, or sweeper treats them specially. The
//!   trash convention is advisory — enforced only by the root agent's
//!   prompt and the Session tool's description.
//! - **No auto-clean.** Entries in `/trash` persist until explicitly
//!   removed. A TTL sweeper may be layered on later as a
//!   config-driven background job without changing this seeding.
//! - **Standing + spawn flags** mirror declared children: `standing`
//!   exempts the node from index maintenance pruning, and
//!   `trigger == "spawn"` keeps the resume guard stack consistent for
//!   a non-base caller. Neither node carries an AGENT.md — runs
//!   attach to their CHILDREN, never to the nodes themselves.
//!
//! ## Dangling trunk
//!
//! Like `ensure_declared_children`, seeding runs before the trunk
//! session exists (its lifecycle belongs to the engine's first
//! self-turn): the nodes' `parent_session_id` dangles until then,
//! which the ownership layer tolerates by design (see
//! `children.rs` module docs, "Dangling trunk").

use std::sync::Arc;

use anyhow::Result;
use peko_auth::Subject;
use peko_session::manager::SessionManager;
use peko_session::{SessionCreateOptions, SessionMetadata};
use tokio::sync::RwLock;

use crate::principal::routers::root::trunk_session_id;

/// Slug of the transient-work node (`/tmp`).
pub const TMP_SLUG: &str = "tmp";
/// Slug of the removal-staging node (`/trash`).
pub const TRASH_SLUG: &str = "trash";

const TMP_TITLE: &str = "Transient sessions (single-use; remove when done)";
const TRASH_TITLE: &str = "Removed sessions (holding area before permanent purge)";

/// Seed the default session nodes for a freshly created principal.
///
/// Idempotent within a run (a node whose slug is already claimed by
/// ANY session under the trunk — standing or not — is skipped), but
/// the intended call shape is exactly once, from
/// `PrincipalManager::create`. `agent_name` is stamped on created
/// sessions' metadata; the caller passes the root agent name it knows
/// at that point (the compiled-in default `"root"` — the stamp is
/// informational and not consulted by ownership or guards).
///
/// Returns the number of nodes created. Storage errors propagate so
/// the caller can warn with the real cause.
pub async fn seed_default_nodes(
    agent_name: &str,
    session_manager: &Arc<RwLock<SessionManager>>,
) -> Result<usize> {
    let trunk = trunk_session_id();
    let mut mgr = session_manager.write().await;
    let metas = mgr.list_all_sessions(false).await?;

    let mut created = 0;
    for (slug, title) in [(TMP_SLUG, TMP_TITLE), (TRASH_SLUG, TRASH_TITLE)] {
        if default_node_exists(&metas, &trunk, slug) {
            continue;
        }
        // Peer subject mirrors the `standing_{name}` placeholder
        // `ensure_declared_children` uses: these nodes are principal
        // infrastructure, not bound to any external peer.
        let peer = Subject::Principal(format!("default_nodes_{slug}").into());
        let options = SessionCreateOptions::new()
            .with_parent(trunk.clone())
            // `with_parent` presets trigger="branch"; the explicit
            // trigger must be applied after it (spawn semantics — the
            // resume guard stack keys on `trigger == "spawn"`).
            .with_trigger("spawn")
            .with_title(title);
        let handle = mgr.create_session(agent_name, &peer, options).await?;
        let node_id = handle.session_id().to_string();
        mgr.set_session_slug(&node_id, Some(slug.to_string()))
            .await?;
        mgr.set_standing(&node_id, true).await?;
        created += 1;
        tracing::info!(
            "seed_default_nodes: created default node '/{slug}' as session {node_id} under {trunk}"
        );
    }
    Ok(created)
}

/// A node counts as present when ANY session under `trunk` (standing
/// or not) already carries the slug — a plain session squatting on
/// `tmp` must never be adopted or collided with.
fn default_node_exists(metas: &[SessionMetadata], trunk: &str, slug: &str) -> bool {
    metas.iter().any(|m| {
        m.slug.as_deref() == Some(slug)
            && m.parent_session_id
                .as_ref()
                .map(|p| p.to_string())
                .as_deref()
                == Some(trunk)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_session::SessionId;

    async fn fixture() -> (tempfile::TempDir, Arc<RwLock<SessionManager>>) {
        let dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new().with_sessions_dir_internal(dir.path().join("sessions"));
        (dir, Arc::new(RwLock::new(manager)))
    }

    async fn metas_of(manager: &Arc<RwLock<SessionManager>>) -> Vec<SessionMetadata> {
        manager
            .write()
            .await
            .list_all_sessions(false)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn seeds_both_nodes_with_standing_spawn_flags() {
        let (_dir, manager) = fixture().await;
        let created = seed_default_nodes("root", &manager).await.unwrap();
        assert_eq!(created, 2);

        let metas = metas_of(&manager).await;
        let trunk = trunk_session_id();
        for (slug, title_hint) in [(TMP_SLUG, "Transient"), (TRASH_SLUG, "Removed")] {
            let node = metas
                .iter()
                .find(|m| m.slug.as_deref() == Some(slug))
                .unwrap_or_else(|| panic!("node '{slug}' exists"));
            assert!(node.standing, "'{slug}' must be standing");
            assert_eq!(node.trigger, "spawn");
            assert_eq!(
                node.parent_session_id.map(|id| id.to_string()),
                Some(SessionId::from("root:self").to_string()),
                "'{slug}' must be parented at the trunk (dangling until the first self-turn)"
            );
            assert!(
                node.title
                    .as_deref()
                    .unwrap_or_default()
                    .contains(title_hint),
                "'{slug}' title must be self-explanatory in list output"
            );
            assert!(default_node_exists(&metas, &trunk, slug));
        }
    }

    #[tokio::test]
    async fn repeated_call_is_noop() {
        let (_dir, manager) = fixture().await;
        assert_eq!(seed_default_nodes("root", &manager).await.unwrap(), 2);
        assert_eq!(seed_default_nodes("root", &manager).await.unwrap(), 0);
        assert_eq!(metas_of(&manager).await.len(), 2);
    }

    /// A plain (non-standing) session squatting on the slug must not
    /// be adopted, collided with, or duplicated — the node is skipped
    /// while its sibling still seeds.
    #[tokio::test]
    async fn plain_session_on_slug_blocks_only_that_node() {
        let (_dir, manager) = fixture().await;
        {
            let mut mgr = manager.write().await;
            let peer = Subject::User("alice".to_string());
            let options = SessionCreateOptions::new()
                .with_parent(trunk_session_id())
                .with_trigger("spawn");
            let handle = mgr.create_session("root", &peer, options).await.unwrap();
            let id = handle.session_id().to_string();
            mgr.set_session_slug(&id, Some(TMP_SLUG.to_string()))
                .await
                .unwrap();
        }

        let created = seed_default_nodes("root", &manager).await.unwrap();
        assert_eq!(created, 1, "only /trash seeds; /tmp is skipped");
        let metas = metas_of(&manager).await;
        assert_eq!(metas.len(), 2);
        let trash = metas
            .iter()
            .find(|m| m.slug.as_deref() == Some(TRASH_SLUG))
            .expect("trash node seeded");
        assert!(trash.standing);
    }
}
