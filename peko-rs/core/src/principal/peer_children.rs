//! Peer-child provisioning (agent-session paradigm, sprint 2 Phase 5).
//!
//! Every external ingress peer gets a STANDING first-level child of
//! the trunk session (`root:self`) that acts as that peer's permanent
//! conversation with the principal:
//!
//! - the CLI owner (`user:local`) → `/local-user`;
//! - any other user (`user:{id}`) → `/user-{sanitized}`;
//! - an A2A principal (`principal:{did}`) →
//!   `/principal-{did-fragment}`.
//!
//! [`ensure_peer_child`] find-or-creates the child (metadata + a JSONL
//! created-event only — NO LLM turn, no run), flagged
//! `trigger = "spawn"` / `standing = true` / `slug =
//! peer_child_slug(peer)`, parented at the trunk, titled with the
//! peer's display string, and stamped with the REAL peer (not the
//! `standing_*` placeholder [`crate::principal::children`] uses for
//! declared children). The owner's child is additionally flagged
//! `privileged = true`, giving its caller whole-store reach in the
//! ownership guards (`crate::session::ownership`); every other peer
//! child stays subtree-scoped.
//!
//! ## Dangling trunk
//!
//! The trunk session may not exist yet (no self-turn has run). The
//! child's `parent_session_id` then dangles — which the ownership
//! layer tolerates by design (same as the dangling owner root in
//! `crate::principal::children`): the ancestor walk ends gracefully at
//! the missing entry, and `descendants_of` adjacency works without a
//! parent entry. We deliberately do NOT create the trunk entry here —
//! its lifecycle belongs to the engine.
//!
//! ## Concurrency
//!
//! Creation is serialized by holding the shared per-principal
//! `SessionManager` write lock across the whole find-or-create (the
//! `ensure_declared_children` pattern): every ingress path for a
//! principal shares one manager, so concurrent first-contact for the
//! same peer cannot double-create.

use std::sync::Arc;

use anyhow::Result;
use peko_auth::Subject;
use peko_session::manager::SessionManager;
use peko_session::path::MAX_SLUG_LEN;
use peko_session::{SessionCreateOptions, SessionMetadata};
use tokio::sync::RwLock;

use crate::principal::children::find_declared_child;
use crate::principal::routers::root::trunk_session_id;

/// Upper bound on `-N` suffix attempts when a peer's derived slug
/// collides with a different session under the trunk (sanitized
/// collisions like `user:Foo Bar` vs `user:foo-bar`). Ten spare slots
/// per peer is far past any realistic collision chain.
const MAX_SLUG_ATTEMPTS: usize = 10;

/// The stable per-parent slug for a peer's standing child of the
/// trunk.
///
/// - `user:local` → `local-user` (the CLI owner's privileged child);
/// - `user:{id}` → `user-{sanitized}` where sanitization keeps
///   lowercase ascii alphanumerics, maps everything else to `-`,
///   collapses repeats, and trims leading/trailing `-`;
/// - `principal:{did}` → `principal-{fragment}` where the fragment is
///   the first 16 lowercase ascii alphanumerics of the DID.
///
/// The result is capped at [`MAX_SLUG_LEN`] chars and validated
/// against `peko_session::path::validate_slug`. `Subject::Public` is
/// not a session peer (ADR-039) and is a structured error.
pub fn peer_child_slug(peer: &Subject) -> Result<String> {
    let slug = match peer {
        Subject::User(id) if id == "local" => "local-user".to_string(),
        Subject::User(id) => {
            let sanitized = sanitize_slug_segment(id);
            let slug = if sanitized.is_empty() {
                "user".to_string()
            } else {
                format!("user-{sanitized}")
            };
            cap_slug(&slug)
        }
        Subject::Principal(did) => {
            let fragment: String = did
                .as_str()
                .chars()
                .map(|c| c.to_ascii_lowercase())
                .filter(|c| c.is_ascii_alphanumeric())
                .take(16)
                .collect();
            format!("principal-{fragment}")
        }
        Subject::Public => {
            anyhow::bail!("public access is not a session peer — no peer child can be provisioned")
        }
    };
    peko_session::path::validate_slug(&slug)?;
    Ok(slug)
}

/// Find the peer's EXISTING standing child of the trunk (find-only —
/// never creates). Returns the child session id, or `None` when the
/// peer has no child yet.
///
/// Used by the peer messenger's note delivery
/// ([`crate::principal::messenger`]): a note must attach to the peer's
/// conversational session but must never provision one — sessions are
/// created by ingress turns, not by note delivery.
///
/// Same match rule as [`ensure_peer_child`]: walk `base`, `base-2`,
/// …; a standing spawn child of the trunk whose stamped peer IS this
/// peer matches; a slug claimed by a DIFFERENT peer's child keeps the
/// walk going; the first unclaimed candidate ends it (`None`).
pub fn find_peer_child(metas: &[SessionMetadata], peer: &Subject) -> Option<String> {
    let base_slug = peer_child_slug(peer).ok()?;
    let trunk = trunk_session_id();
    for attempt in 0..MAX_SLUG_ATTEMPTS {
        let candidate = suffixed_slug(&base_slug, attempt);
        match find_declared_child(metas, &trunk, &candidate) {
            Some(m) if peer_matches(m, peer) => return Some(m.session_id.clone()),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

/// Find-or-create the peer's standing child of the trunk; returns the
/// child session id. Idempotent: a second call returns the same id.
///
/// `owner` is the principal's configured owner subject
/// (`principal.toml`); the child is flagged `privileged` iff `peer ==
/// owner` (the `/local-user` case). `agent_name` is stamped on a
/// created session's metadata (the root agent's prompt name on the
/// production path), matching [`crate::principal::children`].
///
/// Match: a session whose `slug == peer_child_slug(peer)` AND
/// `standing == true` AND `trigger == "spawn"` whose parent chain
/// roots at the trunk (`root:self`) AND whose stamped peer IS this
/// peer. A slug held by a DIFFERENT peer's child (sanitized collision)
/// does not match — the peer's child is created with a `-2`, `-3`, …
/// suffix instead.
pub async fn ensure_peer_child(
    agent_name: &str,
    owner: &Subject,
    peer: &Subject,
    session_manager: &Arc<RwLock<SessionManager>>,
) -> Result<String> {
    let base_slug = peer_child_slug(peer)?;
    let trunk = trunk_session_id();
    let privileged = peer == owner;

    // Hold the write lock across find-or-create so concurrent ingress
    // for the same peer serializes here (module docs, "Concurrency").
    let mut mgr = session_manager.write().await;
    let metas = mgr.list_all_sessions(false).await?;

    // Find: an existing standing child stamped with THIS peer is
    // reused (idempotent). Otherwise walk `base`, `base-2`, … for the
    // first candidate not claimed by a DIFFERENT peer's child
    // (sanitized collisions like `user:Foo Bar` vs `user:foo-bar`).
    if let Some(existing) = find_peer_child(&metas, peer) {
        return Ok(existing);
    }
    let mut create_slug = None;
    for attempt in 0..MAX_SLUG_ATTEMPTS {
        let candidate = suffixed_slug(&base_slug, attempt);
        match find_declared_child(&metas, &trunk, &candidate) {
            Some(_) => continue,
            None => {
                create_slug = Some(candidate);
                break;
            }
        }
    }
    let mut slug = create_slug.ok_or_else(|| {
        anyhow::anyhow!(
            "peer child slug space exhausted for peer {peer}: every suffix of '{base_slug}' is \
             claimed by another peer's child"
        )
    })?;

    // Create with the REAL peer stamped (not the `standing_*`
    // placeholder `ensure_declared_children` uses).
    let options = SessionCreateOptions::new()
        .with_parent(trunk.clone())
        // `with_parent` presets trigger="branch"; the explicit trigger
        // must be applied after it (spawn semantics — the resume guard
        // stack keys on `trigger == "spawn"`).
        .with_trigger("spawn")
        .with_title(peer.to_string());
    let handle = mgr.create_session(agent_name, peer, options).await?;
    let child_id = handle.session_id().to_string();

    // The find scan only sees standing+spawn matches; a PLAIN session
    // can still hold the candidate slug under the trunk, so retry the
    // assignment with further suffixes on a uniqueness conflict.
    let mut assigned = false;
    for attempt in 0..MAX_SLUG_ATTEMPTS {
        if attempt > 0 {
            slug = suffixed_slug(&base_slug, attempt);
        }
        match mgr.set_session_slug(&child_id, Some(slug.clone())).await {
            Ok(()) => {
                assigned = true;
                break;
            }
            Err(e) if e.to_string().contains("unique per parent") => continue,
            Err(e) => return Err(e),
        }
    }
    if !assigned {
        anyhow::bail!(
            "peer child slug space exhausted for peer {peer}: every suffix of '{base_slug}' \
             collides under the trunk"
        );
    }
    mgr.set_standing(&child_id, true).await?;
    mgr.set_privileged(&child_id, privileged).await?;
    tracing::info!(
        "ensure_peer_child: provisioned peer child '/{slug}' for {peer} as session {child_id} \
         under {trunk} (privileged={privileged})"
    );
    Ok(child_id)
}

/// Sanitize a user id into a slug segment: lowercase ascii
/// alphanumerics kept, everything else mapped to `-`, repeats
/// collapsed, leading/trailing `-` trimmed.
fn sanitize_slug_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Truncate to the slug length cap (char-safe).
fn cap_slug(slug: &str) -> String {
    slug.chars().take(MAX_SLUG_LEN).collect()
}

/// Attempt 0 is the base slug; attempts 1.. append `-2`, `-3`, …,
/// truncating the base first so the result stays within the cap.
fn suffixed_slug(base: &str, attempt: usize) -> String {
    if attempt == 0 {
        return base.to_string();
    }
    let suffix = format!("-{}", attempt + 1);
    let head: String = base.chars().take(MAX_SLUG_LEN - suffix.len()).collect();
    format!("{head}{suffix}")
}

/// True when the session was stamped with this peer at creation
/// (`SessionManager::create_session` records `peer.kind()` /
/// `peer.subject_id()`).
fn peer_matches(m: &SessionMetadata, peer: &Subject) -> bool {
    let kind = peer.kind().to_string();
    m.peer_type.as_deref() == Some(kind.as_str()) && m.peer_id.as_deref() == Some(peer.subject_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ownership::{caller_context, in_subtree};

    fn principal_peer(did: &str) -> Subject {
        Subject::Principal(did.to_string().into())
    }

    // ─── peer_child_slug ────────────────────────────────────────────

    #[test]
    fn slug_for_local_user_is_local_user() {
        assert_eq!(
            peer_child_slug(&Subject::User("local".to_string())).unwrap(),
            "local-user"
        );
    }

    #[test]
    fn slug_for_users_sanitizes() {
        assert_eq!(
            peer_child_slug(&Subject::User("alice".to_string())).unwrap(),
            "user-alice"
        );
        // Uppercase + spaces + punctuation collapse to single dashes.
        assert_eq!(
            peer_child_slug(&Subject::User("Foo Bar__Baz!".to_string())).unwrap(),
            "user-foo-bar-baz"
        );
        // Unicode maps to dashes; leading/trailing dashes trim.
        assert_eq!(
            peer_child_slug(&Subject::User("héllo wörld".to_string())).unwrap(),
            "user-h-llo-w-rld"
        );
        // Nothing slug-safe survives → bare prefix (still valid).
        assert_eq!(
            peer_child_slug(&Subject::User("!!!".to_string())).unwrap(),
            "user"
        );
    }

    #[test]
    fn slug_for_principals_uses_did_fragment() {
        let slug = peer_child_slug(&principal_peer("did:key:z6MkTestABCDEF1234567890")).unwrap();
        assert_eq!(slug, "principal-didkeyz6mktestab");
        assert!(slug.len() <= MAX_SLUG_LEN);
    }

    #[test]
    fn slug_is_capped_at_max_len() {
        let long_id = "a".repeat(200);
        let slug = peer_child_slug(&Subject::User(long_id)).unwrap();
        assert_eq!(slug.chars().count(), MAX_SLUG_LEN);
        assert!(slug.starts_with("user-"));
        peko_session::path::validate_slug(&slug).unwrap();
    }

    #[test]
    fn slug_for_public_is_an_error() {
        let err = peer_child_slug(&Subject::Public).unwrap_err();
        assert!(err.to_string().contains("not a session peer"), "{err}");
    }

    // ─── ensure_peer_child ──────────────────────────────────────────

    async fn fixture() -> (tempfile::TempDir, Arc<RwLock<SessionManager>>) {
        let dir = tempfile::tempdir().unwrap();
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
    async fn creates_owner_peer_child_privileged_and_standing() {
        let (_dir, manager) = fixture().await;
        let owner = Subject::User("local".to_string());

        let child_id = ensure_peer_child("root", &owner, &owner, &manager)
            .await
            .unwrap();

        let metas = metas_of(&manager).await;
        let child = metas
            .iter()
            .find(|m| m.session_id == child_id)
            .expect("child metadata exists");
        assert_eq!(child.slug.as_deref(), Some("local-user"));
        assert!(child.standing, "peer child must be standing");
        assert!(child.privileged, "owner peer child must be privileged");
        assert_eq!(child.trigger, "spawn");
        assert_eq!(
            child.parent_session_id.as_deref(),
            Some("root:self"),
            "peer child must be parented at the trunk"
        );
        assert_eq!(child.title.as_deref(), Some("user:local"));
        // The REAL peer is stamped (not a standing_* placeholder).
        assert_eq!(child.peer_type.as_deref(), Some("user"));
        assert_eq!(child.peer_id.as_deref(), Some("local"));
    }

    #[tokio::test]
    async fn stranger_peer_child_is_not_privileged() {
        let (_dir, manager) = fixture().await;
        let owner = Subject::User("local".to_string());
        let stranger = Subject::User("mallory".to_string());
        let a2a = principal_peer("did:key:z6MkStranger");

        let stranger_id = ensure_peer_child("root", &owner, &stranger, &manager)
            .await
            .unwrap();
        let a2a_id = ensure_peer_child("root", &owner, &a2a, &manager)
            .await
            .unwrap();

        let metas = metas_of(&manager).await;
        let s = metas.iter().find(|m| m.session_id == stranger_id).unwrap();
        assert_eq!(s.slug.as_deref(), Some("user-mallory"));
        assert!(s.standing);
        assert!(!s.privileged);
        assert_eq!(s.peer_id.as_deref(), Some("mallory"));

        let p = metas.iter().find(|m| m.session_id == a2a_id).unwrap();
        assert_eq!(p.slug.as_deref(), Some("principal-didkeyz6mkstrang"));
        assert!(p.standing);
        assert!(!p.privileged);
        assert_eq!(p.peer_type.as_deref(), Some("principal"));
        assert_eq!(p.peer_id.as_deref(), Some("did:key:z6MkStranger"));
    }

    #[tokio::test]
    async fn second_call_is_idempotent() {
        let (_dir, manager) = fixture().await;
        let owner = Subject::User("local".to_string());
        let peer = Subject::User("alice".to_string());

        let first = ensure_peer_child("root", &owner, &peer, &manager)
            .await
            .unwrap();
        let second = ensure_peer_child("root", &owner, &peer, &manager)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(metas_of(&manager).await.len(), 1);
    }

    /// Two peers whose ids sanitize to the same slug get distinct
    /// children via the `-2` suffix, and each subsequent call still
    /// resolves to its OWN child.
    #[tokio::test]
    async fn sanitized_collision_gets_suffix_and_stays_idempotent() {
        let (_dir, manager) = fixture().await;
        let owner = Subject::User("local".to_string());
        let peer_a = Subject::User("foo-bar".to_string());
        let peer_b = Subject::User("foo bar".to_string());

        let a_id = ensure_peer_child("root", &owner, &peer_a, &manager)
            .await
            .unwrap();
        let b_id = ensure_peer_child("root", &owner, &peer_b, &manager)
            .await
            .unwrap();
        assert_ne!(a_id, b_id);

        let metas = metas_of(&manager).await;
        let a = metas.iter().find(|m| m.session_id == a_id).unwrap();
        let b = metas.iter().find(|m| m.session_id == b_id).unwrap();
        assert_eq!(a.slug.as_deref(), Some("user-foo-bar"));
        assert_eq!(b.slug.as_deref(), Some("user-foo-bar-2"));

        // Idempotent per peer: each resolves to its own child again.
        assert_eq!(
            ensure_peer_child("root", &owner, &peer_a, &manager)
                .await
                .unwrap(),
            a_id
        );
        assert_eq!(
            ensure_peer_child("root", &owner, &peer_b, &manager)
                .await
                .unwrap(),
            b_id
        );
        assert_eq!(metas_of(&manager).await.len(), 2);
    }

    /// A PLAIN (non-standing) session holding the derived slug under
    /// the trunk does not satisfy the match; provisioning retries with
    /// the `-2` suffix (the per-parent uniqueness error path).
    #[tokio::test]
    async fn plain_session_slug_collision_retries_with_suffix() {
        let (_dir, manager) = fixture().await;
        let owner = Subject::User("local".to_string());
        let peer = Subject::User("alice".to_string());

        // Pre-create a non-standing session under the trunk holding
        // the would-be slug.
        {
            let mut mgr = manager.write().await;
            let options = SessionCreateOptions::new()
                .with_parent("root:self")
                .with_trigger("spawn");
            let handle = mgr.create_session("root", &owner, options).await.unwrap();
            let id = handle.session_id().to_string();
            mgr.set_session_slug(&id, Some("user-alice".to_string()))
                .await
                .unwrap();
        }

        let child_id = ensure_peer_child("root", &owner, &peer, &manager)
            .await
            .unwrap();
        let metas = metas_of(&manager).await;
        let child = metas.iter().find(|m| m.session_id == child_id).unwrap();
        assert_eq!(child.slug.as_deref(), Some("user-alice-2"));
        assert!(child.standing);
        assert_eq!(metas.len(), 2, "the plain session is left untouched");
    }

    /// The trunk session may not exist yet: the child's parent pointer
    /// dangles, which the ownership layer tolerates (the ancestor walk
    /// ends gracefully; the child classifies as a spawned caller). Once
    /// the trunk exists it manages the whole store including the child.
    #[tokio::test]
    async fn dangling_trunk_is_tolerated() {
        let (_dir, manager) = fixture().await;
        let owner = Subject::User("local".to_string());

        let child_id = ensure_peer_child("root", &owner, &owner, &manager)
            .await
            .unwrap();
        let metas = metas_of(&manager).await;

        // BEFORE the trunk exists: the child is a spawned caller whose
        // dangling parent stays in its ancestor chain.
        let child_caller = caller_context(&child_id, &metas);
        assert!(!child_caller.is_base);
        assert!(child_caller.privileged);
        assert!(!child_caller.dangling);
        assert_eq!(child_caller.ancestors, vec!["root:self".to_string()]);
        assert!(!in_subtree(&child_caller, "root:self", &metas));

        // Create the trunk (as the first self-turn would).
        {
            let mut mgr = manager.write().await;
            let options = SessionCreateOptions::new().with_session_id("root:self");
            mgr.create_session("root", &owner, options).await.unwrap();
        }
        let metas = metas_of(&manager).await;

        // AFTER: the trunk caller is base and manages the child.
        let trunk_caller = caller_context("root:self", &metas);
        assert!(trunk_caller.is_base);
        assert!(in_subtree(&trunk_caller, &child_id, &metas));
    }

    #[tokio::test]
    async fn public_peer_is_refused_without_creating() {
        let (_dir, manager) = fixture().await;
        let owner = Subject::User("local".to_string());
        let err = ensure_peer_child("root", &owner, &Subject::Public, &manager)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a session peer"), "{err}");
        assert!(metas_of(&manager).await.is_empty());
    }
}
