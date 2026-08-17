//! Ensure-declared for standing named children (agent-session
//! paradigm, Phase 2).
//!
//! A principal may declare STANDING first-level children in
//! `principal.toml`:
//!
//! ```toml
//! [children.memory]
//! subagent_type = "archivist"
//! description = "Long-term memory curator"
//! ```
//!
//! At root-agent run setup (`agent_runner` calls
//! [`ensure_declared_children`]) each declared child is ensured to
//! EXIST as a session:
//!
//! - match: a session whose `slug == name` AND `standing == true` AND
//!   `trigger == "spawn"` whose parent chain roots at the principal's
//!   TRUNK session (`root:self` — re-anchored from the retired owner
//!   root `root:{owner}` in Phase 7) — left untouched (idempotent);
//! - missing: created with metadata + a JSONL created-event only (NO
//!   LLM turn, no run), flagged `trigger = "spawn"` / `standing =
//!   true` / `slug = name`, parented at the trunk, titled with
//!   the declaration's description (falling back to the name). The
//!   declaration's `subagent_type` is recorded as a `System` event in
//!   the child JSONL so a later attach can default to it
//!   (`crate::session::standing`).
//!
//! ## Dangling trunk
//!
//! The trunk session may not exist yet (no self-turn has run).
//! The child's `parent_session_id` then dangles — which the ownership
//! layer tolerates by design: the ancestor walk ends gracefully at the
//! missing entry (the id still lands in `ancestors`), and
//! `descendants_of` adjacency works without a parent entry. The child
//! classifies as a spawned (non-base) caller either way, and once the
//! trunk session is created it manages the whole store including
//! the child. We deliberately do NOT create the trunk entry at
//! init — its lifecycle belongs to the engine.
//!
//! ## Failure tolerance
//!
//! Unreadable/corrupt `principal.toml` and storage failures never
//! crash principal init: the caller warns and continues (the
//! `seen_models.json` tolerated-corruption precedent).

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use peko_auth::Subject;
use peko_session::manager::SessionManager;
use peko_session::{SessionCreateOptions, SessionMetadata};
use tokio::sync::RwLock;

use crate::principal::config::PrincipalConfig;
use crate::principal::routers::root::trunk_session_id;
use crate::session::ownership::caller_context;

/// Ensure every `[children]` declaration in the principal's
/// `principal.toml` exists as a standing session. Returns the number
/// of children created this call (0 when all already exist).
///
/// `agent_name` is stamped on created sessions' metadata (the root
/// agent's prompt name on the production path). Config read/parse
/// failures warn + return `Ok(0)`; storage errors propagate so the
/// caller can warn with the real cause.
pub async fn ensure_declared_children(
    workspace_path: &Path,
    agent_name: &str,
    session_manager: &Arc<RwLock<SessionManager>>,
) -> Result<usize> {
    let config_path = workspace_path.join("principal.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            tracing::warn!(
                "ensure_declared_children: cannot read {}: {e} — skipping",
                config_path.display()
            );
            return Ok(0);
        }
    };
    let config: PrincipalConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "ensure_declared_children: cannot parse {}: {e} — skipping",
                config_path.display()
            );
            return Ok(0);
        }
    };
    if config.children.is_empty() {
        return Ok(0);
    }

    let trunk = trunk_session_id();
    let mut created = 0;
    let mut mgr = session_manager.write().await;
    // Snapshot once: declared names are unique map keys, and a child
    // created for name A can never satisfy the match for name B, so a
    // stale snapshot cannot cause a duplicate create.
    let metas = mgr.list_all_sessions(false).await?;
    for (name, decl) in &config.children {
        if find_declared_child(&metas, &trunk, name).is_some() {
            continue;
        }
        let peer = Subject::Principal(format!("standing_{name}").into());
        let options = SessionCreateOptions::new()
            .with_parent(trunk.clone())
            // `with_parent` presets trigger="branch"; the explicit
            // trigger must be applied after it (spawn semantics — the
            // resume guard stack keys on `trigger == "spawn"`).
            .with_trigger("spawn")
            .with_title(decl.description.clone().unwrap_or_else(|| name.clone()));
        let handle = mgr.create_session(agent_name, &peer, options).await?;
        let child_id = handle.session_id().to_string();
        mgr.set_session_slug(&child_id, Some(name.clone())).await?;
        mgr.set_standing(&child_id, true).await?;
        if let Some(dir) = mgr.sessions_dir().cloned() {
            crate::session::standing::record_declared_child(
                &dir,
                &child_id,
                name,
                &decl.subagent_type,
                decl.description.as_deref(),
            )
            .await?;
        }
        created += 1;
        tracing::info!(
            "ensure_declared_children: created standing child '{name}' as session {child_id} \
             under {trunk}"
        );
    }
    Ok(created)
}

/// Find the standing child session for `name`: slug match + standing
/// + spawn-triggered + parent chain rooted at `owner_root` (the
/// principal's TRUNK `root:self` on the production path — Phase 7
/// re-anchor).
#[must_use]
pub fn find_declared_child<'a>(
    metas: &'a [SessionMetadata],
    owner_root: &str,
    name: &str,
) -> Option<&'a SessionMetadata> {
    metas.iter().find(|m| {
        m.slug.as_deref() == Some(name)
            && m.standing
            && m.trigger == "spawn"
            && tree_root_id(&m.session_id, metas).as_deref() == Some(owner_root)
    })
}

/// The topmost ancestor of `session_id` (or the id itself when it has
/// no parent). The walk tolerates dangling parent pointers: the last
/// known ancestor id is returned even when its metadata is missing.
/// `None` when `session_id` itself has no metadata entry.
fn tree_root_id(session_id: &str, metas: &[SessionMetadata]) -> Option<String> {
    let ctx = caller_context(session_id, metas);
    if ctx.dangling {
        return None;
    }
    Some(
        ctx.ancestors
            .last()
            .cloned()
            .unwrap_or_else(|| session_id.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ownership::{descendants_of, in_subtree};

    /// Build a `SessionManager` over a tempdir and write a
    /// `principal.toml` with the given `[children]` table body into
    /// `<workspace>/principal.toml`. Returns `(workspace, manager)`.
    async fn fixture(children_toml: &str) -> (tempfile::TempDir, Arc<RwLock<SessionManager>>) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("principal_ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let toml =
            format!("name = \"p\"\nowner = {{ kind = \"user\", id = \"alice\" }}\n{children_toml}");
        std::fs::write(workspace.join("principal.toml"), toml).unwrap();

        let sessions_dir = dir.path().join("sessions");
        let manager = SessionManager::new().with_sessions_dir_internal(sessions_dir);
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
    async fn creates_missing_declared_children_with_standing_flags() {
        let (dir, manager) = fixture(
            "\n[children.memory]\nsubagent_type = \"archivist\"\ndescription = \"Memory curator\"\n",
        )
        .await;

        let created =
            ensure_declared_children(dir.path().join("principal_ws").as_path(), "root", &manager)
                .await
                .unwrap();
        assert_eq!(created, 1);

        let metas = metas_of(&manager).await;
        let child = metas
            .iter()
            .find(|m| m.slug.as_deref() == Some("memory"))
            .expect("child metadata exists");
        assert!(child.standing, "child must be standing");
        assert_eq!(child.trigger, "spawn");
        assert_eq!(
            child.parent_session_id.as_deref(),
            Some("root:self"),
            "child must be parented at the trunk session"
        );
        assert_eq!(child.title.as_deref(), Some("Memory curator"));

        // The declaration is recoverable from the child JSONL.
        let sessions_dir = manager.read().await.sessions_dir().cloned().unwrap();
        assert_eq!(
            crate::session::standing::declared_subagent_type(&sessions_dir, &child.session_id)
                .await
                .as_deref(),
            Some("archivist")
        );
    }

    #[tokio::test]
    async fn title_falls_back_to_name_when_no_description() {
        let (dir, manager) =
            fixture("\n[children.about-user]\nsubagent_type = \"profiler\"\n").await;
        ensure_declared_children(dir.path().join("principal_ws").as_path(), "root", &manager)
            .await
            .unwrap();
        let metas = metas_of(&manager).await;
        let child = &metas
            .iter()
            .find(|m| m.slug.as_deref() == Some("about-user"))
            .unwrap();
        assert_eq!(child.title.as_deref(), Some("about-user"));
    }

    #[tokio::test]
    async fn second_init_is_idempotent() {
        let (dir, manager) = fixture("\n[children.memory]\nsubagent_type = \"archivist\"\n").await;
        let ws = dir.path().join("principal_ws");
        assert_eq!(
            ensure_declared_children(&ws, "root", &manager)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            ensure_declared_children(&ws, "root", &manager)
                .await
                .unwrap(),
            0
        );
        assert_eq!(metas_of(&manager).await.len(), 1);
    }

    /// A slug-matching session that is NOT standing (or not
    /// spawn-triggered) must NOT satisfy the declaration — a fresh
    /// standing child would conflict on the per-parent slug, so the
    /// create path errors rather than silently adopting the session.
    /// (The Agent tool surfaces the same collision as a structured
    /// refusal at attach time.)
    #[tokio::test]
    async fn non_standing_slug_collision_does_not_match() {
        let (dir, manager) = fixture("\n[children.memory]\nsubagent_type = \"archivist\"\n").await;
        // Pre-create a NON-standing session under the trunk with
        // the same slug.
        {
            let mut mgr = manager.write().await;
            let peer = Subject::User("alice".to_string());
            let options = SessionCreateOptions::new()
                .with_parent("root:self")
                .with_trigger("spawn");
            let handle = mgr.create_session("root", &peer, options).await.unwrap();
            let id = handle.session_id().to_string();
            mgr.set_session_slug(&id, Some("memory".to_string()))
                .await
                .unwrap();
        }
        // The collision surfaces as the per-parent slug uniqueness
        // error from `set_session_slug` — the declaration neither
        // adopts the non-standing session nor silently passes.
        let err =
            ensure_declared_children(dir.path().join("principal_ws").as_path(), "root", &manager)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("unique per parent"), "{err}");
    }

    #[tokio::test]
    async fn missing_or_corrupt_config_warns_and_continues() {
        // Missing principal.toml → Ok(0), no sessions.
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        let manager = Arc::new(RwLock::new(
            SessionManager::new().with_sessions_dir_internal(sessions_dir),
        ));
        assert_eq!(
            ensure_declared_children(dir.path(), "root", &manager)
                .await
                .unwrap(),
            0
        );

        // Corrupt principal.toml → Ok(0), no sessions.
        std::fs::write(dir.path().join("principal.toml"), "not [valid").unwrap();
        assert_eq!(
            ensure_declared_children(dir.path(), "root", &manager)
                .await
                .unwrap(),
            0
        );
        assert!(metas_of(&manager).await.is_empty());
    }

    /// Dangling-parent ownership behavior (the trunk session does
    /// not exist yet when the child is created):
    ///
    /// (a) once the trunk session exists, its caller is base and
    ///     manages the whole store including the child;
    /// (b) the child classifies as a spawned caller both BEFORE and
    ///     AFTER the trunk exists, and manages only its own subtree.
    #[tokio::test]
    async fn dangling_parent_keeps_ownership_semantics() {
        let (dir, manager) = fixture("\n[children.memory]\nsubagent_type = \"archivist\"\n").await;
        let ws = dir.path().join("principal_ws");
        ensure_declared_children(&ws, "root", &manager)
            .await
            .unwrap();
        let metas = metas_of(&manager).await;
        let child_id = metas
            .iter()
            .find(|m| m.slug.as_deref() == Some("memory"))
            .unwrap()
            .session_id
            .clone();

        // (b) BEFORE the trunk exists: the child is a spawned (non-base)
        // caller; its dangling parent id stays in its ancestor chain.
        let child_caller = caller_context(&child_id, &metas);
        assert!(!child_caller.is_base);
        assert!(!child_caller.dangling);
        assert_eq!(child_caller.ancestors, vec!["root:self".to_string()]);
        // The child cannot manage its (missing) parent — outside its subtree.
        assert!(!in_subtree(&child_caller, "root:self", &metas));
        // Adjacency still works without a parent entry: the dangling
        // trunk's descendants include the child.
        assert_eq!(descendants_of("root:self", &metas), vec![child_id.clone()]);

        // Create the trunk session (as the first self-turn would).
        {
            let mut mgr = manager.write().await;
            let peer = Subject::User("alice".to_string());
            let options = SessionCreateOptions::new().with_session_id("root:self");
            mgr.create_session("root", &peer, options).await.unwrap();
        }
        let metas = metas_of(&manager).await;

        // (a) The trunk caller is base → manages the whole store,
        // including the pre-existing standing child.
        let root_caller = caller_context("root:self", &metas);
        assert!(root_caller.is_base);
        assert!(in_subtree(&root_caller, &child_id, &metas));

        // (b) AFTER: the child is still a spawned caller confined to
        // its own subtree.
        let child_caller = caller_context(&child_id, &metas);
        assert!(!child_caller.is_base);
        assert!(!in_subtree(&child_caller, "root:self", &metas));
        // …but it manages its OWN subtree: a grandchild under the child.
        {
            let mut mgr = manager.write().await;
            let peer = Subject::Principal("grandchild".into());
            let options = SessionCreateOptions::new()
                .with_parent(child_id.clone())
                .with_trigger("spawn");
            mgr.create_session("root", &peer, options).await.unwrap();
        }
        let metas = metas_of(&manager).await;
        let grandchild = metas
            .iter()
            .find(|m| m.parent_session_id.as_deref() == Some(child_id.as_str()))
            .unwrap();
        assert!(in_subtree(&child_caller, &grandchild.session_id, &metas));
        // …and still not the trunk.
        assert!(!in_subtree(&child_caller, "root:self", &metas));
    }

    /// The match requires the parent chain to root at THIS principal's
    /// owner root — a standing session with the same slug in a
    /// different tree does not satisfy the declaration.
    #[test]
    fn find_declared_child_requires_owner_root_tree() {
        let mut foreign = SessionMetadata::new("c1", "agent", "c1.jsonl");
        foreign.slug = Some("memory".to_string());
        foreign.standing = true;
        foreign.trigger = "spawn".to_string();
        foreign.parent_session_id = Some("root:user:bob".to_string());
        let metas = vec![foreign];
        assert!(find_declared_child(&metas, "root:user:alice", "memory").is_none());
        assert!(find_declared_child(&metas, "root:user:bob", "memory").is_some());
        // Wrong flags never match.
        let mut not_standing = SessionMetadata::new("c2", "agent", "c2.jsonl");
        not_standing.slug = Some("memory".to_string());
        not_standing.trigger = "spawn".to_string();
        not_standing.parent_session_id = Some("root:user:alice".to_string());
        assert!(find_declared_child(&[not_standing], "root:user:alice", "memory").is_none());
    }

    /// Standing children survive maintenance pruning even when older
    /// than the cutoff (Phase 0 flag semantics, exercised through the
    /// ensure-declared fixture).
    #[tokio::test]
    async fn standing_child_survives_prune() {
        let (dir, manager) = fixture("\n[children.memory]\nsubagent_type = \"archivist\"\n").await;
        ensure_declared_children(dir.path().join("principal_ws").as_path(), "root", &manager)
            .await
            .unwrap();
        let child_id = metas_of(&manager)
            .await
            .iter()
            .find(|m| m.slug.as_deref() == Some("memory"))
            .unwrap()
            .session_id
            .clone();

        // Age the standing child AND a plain session past the cutoff.
        let plain_id = {
            let mut mgr = manager.write().await;
            let peer = Subject::User("alice".to_string());
            let handle = mgr
                .create_session("root", &peer, SessionCreateOptions::new())
                .await
                .unwrap();
            handle.session_id().to_string()
        };
        {
            let mut mgr = manager.write().await;
            let index = mgr.index_mut().expect("index initialized");
            for id in [&child_id, &plain_id] {
                let mut entry = index.get(id).await.unwrap().unwrap();
                entry.updated_at = 0; // ancient
                index.insert(entry).await.unwrap();
            }
            let config = peko_session::index::MaintenanceConfig {
                prune_after: std::time::Duration::from_millis(1),
                ..Default::default()
            };
            let report = index.maintenance(&config).await.unwrap();
            assert_eq!(report.pruned, 1, "only the plain session prunes");

            // Read through to disk (bypass the 30s index cache) so the
            // assertion can't be fooled by a stale in-memory view; the
            // metadata-controller cache is likewise not consulted here.
            assert!(index.get_uncached(&child_id).await.unwrap().is_some());
            assert!(index.get_uncached(&plain_id).await.unwrap().is_none());
        }
    }
}
