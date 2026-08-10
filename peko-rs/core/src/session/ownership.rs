//! Ownership + self-guard primitives for agent-owned session
//! management (plan D4/D5).
//!
//! Sessions form trees via `SessionMetadata::parent_session_id`. A
//! caller running in a **base session** (no parent — the principal's
//! root agent) manages the whole store; a caller in a **spawned
//! session** manages only its own subtree. This module classifies the
//! caller and produces structured, LLM-actionable guard refusals over
//! plain metadata slices — no IO, no locks — so it can be shared by
//! the `SessionManagerRuntime` adapter (session tool) and, in Phase 5,
//! the `SubagentExecutor` / `AgentTool` path (Agent tool).
//!
//! A session whose metadata is missing is treated as a tree root for
//! *classification* (the walk ends there), but a dangling id in the
//! caller's ancestor chain stays in `ancestors` so the delete-ancestor
//! guard still blocks deleting it.

use peko_session::SessionMetadata;

/// Caller classification for ownership guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerContext {
    /// The session the caller is currently running in.
    pub current_session_id: String,
    /// True when the current session has no parent (principal-level
    /// caller; manages the whole store). False ⇒ subtree-level caller
    /// (manages only its own subtree).
    pub is_base: bool,
    /// Ancestor chain of the current session, nearest parent first.
    /// Ids stay in the chain even when their metadata is missing
    /// (dangling) — the delete-ancestor guard depends on that.
    pub ancestors: Vec<String>,
}

/// Walk the `parent_session_id` chain from `current` and classify the
/// caller. The walk ends at the first session whose metadata is
/// missing or has no parent (missing parent = tree root for
/// classification). Cycle-safe.
#[must_use]
pub fn caller_context(current: &str, metas: &[SessionMetadata]) -> CallerContext {
    let find = |id: &str| metas.iter().find(|m| m.session_id == id);

    let first_parent = find(current).and_then(|m| m.parent_session_id.clone());
    let is_base = first_parent.is_none();

    let mut ancestors = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor = first_parent;
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            break; // corrupt chain with a cycle — stop, keep what we have
        }
        ancestors.push(id.clone());
        cursor = find(&id).and_then(|m| m.parent_session_id.clone());
    }

    CallerContext {
        current_session_id: current.to_string(),
        is_base,
        ancestors,
    }
}

/// All ancestors of `id`, nearest first (same walk as
/// [`caller_context`]).
fn ancestors_of(id: &str, metas: &[SessionMetadata]) -> Vec<String> {
    caller_context(id, metas).ancestors
}

/// True when `target` is the caller's current session or sits in the
/// subtree below it (target's ancestor chain contains the caller's
/// current session id).
#[must_use]
pub fn in_subtree(caller: &CallerContext, target: &str, metas: &[SessionMetadata]) -> bool {
    if target == caller.current_session_id {
        return true;
    }
    ancestors_of(target, metas)
        .iter()
        .any(|a| a == &caller.current_session_id)
}

/// All sessions whose ancestor chain contains `target` (the target's
/// descendants, in no particular order).
#[must_use]
pub fn descendants_of(target: &str, metas: &[SessionMetadata]) -> Vec<String> {
    metas
        .iter()
        .filter(|m| {
            m.session_id != target
                && ancestors_of(&m.session_id, metas)
                    .iter()
                    .any(|a| a == target)
        })
        .map(|m| m.session_id.clone())
        .collect()
}

/// Live conversational ids are the deterministic `root:{peer}` /
/// `root:cron:{peer}` slots. Archived chapters keep the same history
/// under a derived `{live}#{ts}` id, so a `root:` id WITHOUT a `#` is
/// a live slot: never directly deletable/archivable — managed via
/// `new` / chapter rotation only.
#[must_use]
pub fn is_live_base_id(id: &str) -> bool {
    id.starts_with("root:") && !id.contains('#')
}

/// The conversation family of an id: the part before `#` (the whole
/// id when it has no `#`). Resume is only legal within one family.
#[must_use]
pub fn chapter_family(id: &str) -> &str {
    id.split_once('#').map_or(id, |(base, _)| base)
}

// ─── Guard refusals ────────────────────────────────────────────────
//
// Every refusal names the offending session id, the reason, and an
// actionable hint so the calling LLM can self-correct.

/// `delete` / `archive` / `rename` on the caller's own live session.
pub fn err_self_mutation(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot modify session '{target}': it is the session you are currently running in — \
         use action 'compact' to summarize it or 'new' to start a fresh chapter instead"
    )
}

/// `delete` on an ancestor of the caller's current session.
pub fn err_delete_ancestor(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot delete session '{target}': it is an ancestor of the session you are running in"
    )
}

/// A subtree (spawned) caller acting outside its subtree.
pub fn err_out_of_tree(target: &str, caller: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "session '{target}' is outside your session subtree (you are running in '{caller}') — \
         spawned agents can only manage sessions they spawned"
    )
}

/// `delete` / `archive` on a live `root:*` conversational slot.
pub fn err_live_base_managed(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "session '{target}' is a live conversational session — it is managed via 'new' / \
         chapter rotation only and cannot be deleted or archived directly"
    )
}

/// A structural operation while a run is in flight for the session.
pub fn err_run_active(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "session '{target}' has an active run in flight — retry after the run completes"
    )
}

/// `compact` on an archived session.
pub fn err_compact_archived(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot compact archived session '{target}': no future run will consume the request — \
         unarchive it first with action 'unarchive'"
    )
}

/// `resume` of an archived chapter.
pub fn err_resume_archived(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot resume archived session '{target}' — unarchive it first with action 'unarchive'"
    )
}

/// `new` / `resume` from a spawned (subtree) caller.
pub fn err_chapters_principal_only() -> anyhow::Error {
    anyhow::anyhow!(
        "chapters are a principal-level concept — spawned agents cannot rotate chapters; \
         use 'branch' to copy a session instead"
    )
}

/// `new` / `resume` when the caller's current session is not a live
/// `root:*` slot (defensive; e.g. a chapter id containing `#`).
pub fn err_not_live_base(cur: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "chapter rotation applies to the live conversational session, but you are running in \
         '{cur}', which is not a live base session"
    )
}

/// `resume` targeting the caller's own current session.
pub fn err_resume_self(target: &str) -> anyhow::Error {
    anyhow::anyhow!("session '{target}' is already your current session")
}

/// `resume` across conversation families (different `root:{peer}` /
/// `root:cron:{peer}` prefix).
pub fn err_resume_cross_family(target: &str, cur: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot resume '{target}': it belongs to a different conversation family than your \
         current session '{cur}'"
    )
}

/// Non-recursive `delete` on a session that has descendants.
pub fn err_descendants_exist(target: &str, descendants: &[String]) -> anyhow::Error {
    anyhow::anyhow!(
        "session '{target}' has descendants [{}] — pass recursive:true to delete the whole subtree",
        descendants.join(", ")
    )
}

/// `Agent` with `resume_session` targeting a non-spawn session
/// (chapter, branch, or live root).
pub fn err_resume_not_spawned(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot re-attach to session '{target}': only spawned subagent sessions (kind \
         'spawned') can be re-attached with Agent's resume_session — chapters, branches, \
         and live root sessions are refused"
    )
}

/// `Agent` with `resume_session` targeting the caller's own session
/// or one of its ancestors (would re-enter the caller's own run).
pub fn err_resume_into_own_run(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot re-attach to session '{target}': it is the session you are running in or one \
         of its ancestors — pick a spawned session from the session tool's list instead"
    )
}

/// `Agent` spawn with an explicit `parent_session_key` outside the
/// caller's subtree (context seeding from foreign sessions).
pub fn err_context_out_of_tree(target: &str, caller: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot seed context from session '{target}': it is outside your session subtree \
         (you are running in '{caller}')"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, parent: Option<&str>) -> SessionMetadata {
        let mut m = SessionMetadata::new(id, "agent", format!("{id}.jsonl"));
        m.parent_session_id = parent.map(String::from);
        m
    }

    fn tree() -> Vec<SessionMetadata> {
        // root ── spawn1 ── child1
        //    └──── spawn2
        vec![
            meta("root:user:alice", None),
            meta("spawn1", Some("root:user:alice")),
            meta("spawn2", Some("root:user:alice")),
            meta("child1", Some("spawn1")),
        ]
    }

    #[test]
    fn caller_context_base_session() {
        let ctx = caller_context("root:user:alice", &tree());
        assert!(ctx.is_base);
        assert!(ctx.ancestors.is_empty());
        assert_eq!(ctx.current_session_id, "root:user:alice");
    }

    #[test]
    fn caller_context_spawned_session() {
        let ctx = caller_context("child1", &tree());
        assert!(!ctx.is_base);
        assert_eq!(
            ctx.ancestors,
            vec!["spawn1".to_string(), "root:user:alice".to_string()]
        );
    }

    #[test]
    fn caller_context_missing_metadata_is_base() {
        // Unknown session: walk ends immediately, classified as base.
        let ctx = caller_context("ghost", &tree());
        assert!(ctx.is_base);
        assert!(ctx.ancestors.is_empty());
    }

    #[test]
    fn dangling_ancestor_stays_in_chain() {
        // spawn1's parent metadata is absent: the id stays in the
        // ancestor chain (delete-ancestor guard) but the walk ends.
        let metas = vec![meta("child1", Some("spawn1"))];
        let ctx = caller_context("child1", &metas);
        assert!(!ctx.is_base);
        assert_eq!(ctx.ancestors, vec!["spawn1".to_string()]);
    }

    #[test]
    fn caller_context_survives_cycles() {
        let mut a = meta("a", Some("b"));
        let b = meta("b", Some("a"));
        a.parent_session_id = Some("b".to_string());
        let metas = vec![a, b];
        let ctx = caller_context("a", &metas);
        assert_eq!(ctx.ancestors, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn in_subtree_covers_self_and_descendants_only() {
        let metas = tree();
        let caller = caller_context("spawn1", &metas);
        assert!(in_subtree(&caller, "spawn1", &metas));
        assert!(in_subtree(&caller, "child1", &metas));
        assert!(!in_subtree(&caller, "spawn2", &metas));
        assert!(!in_subtree(&caller, "root:user:alice", &metas));
    }

    #[test]
    fn descendants_of_collects_transitive_children() {
        let metas = tree();
        let mut d = descendants_of("root:user:alice", &metas);
        d.sort();
        assert_eq!(
            d,
            vec![
                "child1".to_string(),
                "spawn1".to_string(),
                "spawn2".to_string()
            ]
        );
        assert_eq!(descendants_of("spawn2", &metas), Vec::<String>::new());
    }

    #[test]
    fn live_base_id_detection() {
        assert!(is_live_base_id("root:user:alice"));
        assert!(is_live_base_id("root:cron:alice"));
        assert!(!is_live_base_id("root:user:alice#20260809-120000"));
        assert!(!is_live_base_id("spawn1"));
    }

    #[test]
    fn chapter_family_splits_on_hash() {
        assert_eq!(
            chapter_family("root:user:alice#20260809-120000"),
            "root:user:alice"
        );
        assert_eq!(chapter_family("root:user:alice"), "root:user:alice");
        assert_eq!(chapter_family("spawn1"), "spawn1");
    }

    #[test]
    fn refusals_are_actionable() {
        for err in [
            err_self_mutation("s"),
            err_delete_ancestor("s"),
            err_out_of_tree("s", "c"),
            err_live_base_managed("s"),
            err_run_active("s"),
            err_compact_archived("s"),
            err_resume_archived("s"),
            err_chapters_principal_only(),
            err_not_live_base("c"),
            err_resume_self("s"),
            err_resume_cross_family("s", "c"),
            err_descendants_exist("s", &["d1".to_string()]),
        ] {
            let msg = err.to_string();
            assert!(msg.len() > 20, "refusal too terse: {msg}");
        }
        assert!(
            err_descendants_exist("s", &["d1".to_string(), "d2".to_string()])
                .to_string()
                .contains("d1, d2")
        );
    }
}
