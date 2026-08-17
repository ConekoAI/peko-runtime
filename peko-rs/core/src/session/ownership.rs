//! Ownership + self-guard primitives for agent-owned session
//! management.
//!
//! Sessions form trees via `SessionMetadata::parent_session_id`. A
//! caller running in a **base session** (no parent — the principal's
//! root agent) manages the whole store; a caller in a **spawned
//! session** manages only its own subtree. This module classifies the
//! caller and produces structured, LLM-actionable guard refusals over
//! plain metadata slices — no IO, no locks — so it is shared by the
//! `SessionManagerRuntime` adapter (session tool) and the
//! `SubagentExecutor` / `AgentTool` path (Agent tool).
//!
//! The principal's root session (the live `root:*` slot) is
//! **continuous**: the engine owns its lifecycle (paging +
//! compaction), so `delete` / `archive` on it are refused via
//! [`err_live_base_managed`]. Archived state is read directly from
//! `SessionMetadata::archived`.
//!
//! A session whose metadata is missing is treated as a tree root for
//! *classification* (the walk ends there), but a dangling id in the
//! caller's ancestor chain stays in `ancestors` so the delete-ancestor
//! guard still blocks deleting it.
//!
//! ## Privileged callers (sprint 2 peer-child provisioning)
//!
//! A session whose metadata carries `privileged = true` gives its
//! caller **whole-store reach** in the ownership guards — the guard
//! sites read `caller.is_base || caller.privileged` where they used to
//! read `caller.is_base` alone. Privilege affects guard reach ONLY:
//! the session keeps its `parent_session_id` and stays in the trunk's
//! tree, so path addressing, the ancestor guards (`err_delete_ancestor`,
//! `err_move_ancestor`), the self-mutation guard, and the `root:*`
//! family guards all still apply to it, and `descendants_of`
//! supervision is unchanged. Only the principal owner's peer child
//! (`/local-user`) is provisioned privileged; every other peer child
//! stays subtree-scoped.

use peko_session::SessionMetadata;

/// Caller classification for ownership guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerContext {
    /// The session the caller is currently running in.
    pub current_session_id: String,
    /// True when the current session has no parent (principal-level
    /// caller; manages the whole store). False ⇒ subtree-level caller
    /// (manages only its own subtree).
    ///
    /// A session whose metadata is missing from `metas` is NEVER base:
    /// the caller is treated as dangling (see [`Self::dangling`]) and
    /// refused rather than promoted to principal-level. The previous
    /// "missing = base" default was a privilege-escalation hole — see
    /// PR review 2026-08-10.
    pub is_base: bool,
    /// True when the caller's session metadata carries the
    /// `privileged` flag (sprint 2 peer-child provisioning): the
    /// caller gets whole-store reach in the ownership guards, like a
    /// base caller. Unlike `is_base`, this does NOT change tree
    /// membership — the session keeps its parent pointer, so the
    /// ancestor / self-mutation / `root:*` guards still apply.
    /// Populated from the caller's own metadata, so a dangling caller
    /// is never privileged.
    pub privileged: bool,
    /// True when the caller's own session metadata is missing from
    /// `metas`. Guards treat dangling callers like subtree callers
    /// with an empty ancestor chain — every ownership check fails,
    /// producing a `dangling` refusal rather than a silent
    /// privilege grant.
    pub dangling: bool,
    /// Ancestor chain of the current session, nearest parent first.
    /// Ids stay in the chain even when their metadata is missing
    /// (dangling) — the delete-ancestor guard depends on that.
    pub ancestors: Vec<String>,
}

/// Walk the `parent_session_id` chain from `current` and classify the
/// caller. The walk ends at the first session whose metadata is
/// missing or has no parent. Cycle-safe.
#[must_use]
pub fn caller_context(current: &str, metas: &[SessionMetadata]) -> CallerContext {
    let find = |id: &str| metas.iter().find(|m| m.session_id == id);

    let current_meta = find(current);
    let dangling = current_meta.is_none();
    let first_parent = current_meta.and_then(|m| m.parent_session_id.clone());
    // is_base is true ONLY when the caller's metadata is present AND
    // has no parent recorded. A missing-metadata caller is dangling,
    // not base.
    let is_base = first_parent.is_none() && !dangling;
    // Privilege comes from the caller's own metadata; a dangling
    // caller (no metadata) is never privileged.
    let privileged = current_meta.is_some_and(|m| m.privileged);

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
        privileged,
        dangling,
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
///
/// Implemented as a single BFS over a parent-to-children adjacency
/// map built once, instead of `metas.len()` calls to `ancestors_of`
/// (each an O(depth) walk). Total cost: O(N + E) per call, where
/// E <= N.
#[must_use]
pub fn descendants_of(target: &str, metas: &[SessionMetadata]) -> Vec<String> {
    // Build parent -> direct-children adjacency map in one pass.
    let mut children: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::with_capacity(metas.len());
    for m in metas {
        if let Some(parent) = m.parent_session_id.as_deref() {
            children.entry(parent).or_default().push(&m.session_id);
        }
    }
    // BFS down from `target`, collecting every reachable node.
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    // Seed `seen` with the target so a cycle back to it doesn't push
    // it into `out` and so the walk terminates.
    seen.insert(target);
    let mut stack: Vec<&str> = match children.get(target) {
        Some(kids) => kids.iter().copied().collect(),
        None => Vec::new(),
    };
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        out.push(id.to_string());
        if let Some(kids) = children.get(id) {
            for k in kids {
                if !seen.contains(*k) {
                    stack.push(k);
                }
            }
        }
    }
    out
}

// ─── Guard refusals ────────────────────────────────────────────────
//
// Every refusal names the offending session id, the reason, and an
// actionable hint so the calling LLM can self-correct.

/// `delete` / `archive` / `rename` on the caller's own live session.
pub fn err_self_mutation(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot modify session '{target}': it is the session you are currently running in. \
         The engine compacts and pages it automatically; to manage a different session, \
         pass its session_id from `session list`."
    )
}

/// `delete` on an ancestor of the caller's current session.
pub fn err_delete_ancestor(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot delete session '{target}': it is an ancestor of the session you are running in"
    )
}

/// `move` on an ancestor of the caller's current session.
pub fn err_move_ancestor(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot move session '{target}': it is an ancestor of the session you are running in"
    )
}

/// `move` that would create a parent↔child cycle (`new_parent` is the
/// target itself or one of its descendants). Cycles silently truncate
/// ancestry walks, so they are refused at move time.
pub fn err_move_cycle(target: &str, new_parent: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot move session '{target}' under '{new_parent}': the destination is the session \
         itself or one of its descendants — the move would create a cycle"
    )
}

/// A subtree (spawned) caller acting outside its subtree.
pub fn err_out_of_tree(target: &str, caller: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "session '{target}' is outside your session subtree (you are running in '{caller}') — \
         spawned agents can only manage sessions they spawned"
    )
}

/// `delete` / `archive` / `move` on the principal's live `root:*` session.
pub fn err_live_base_managed(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "session '{target}' is the principal's root session: it is continuous and managed by \
         the engine — you cannot delete, archive, or move it. To manage a different session, \
         pass its session_id from `session list`."
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

/// `resume` of an archived session.
pub fn err_resume_archived(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot resume archived session '{target}' — unarchive it first with action 'unarchive'"
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

/// `Agent` with `action = "resume"` targeting a non-spawn session
/// (branch or live root).
pub fn err_resume_not_spawned(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot re-attach to session '{target}': only spawned subagent sessions (kind \
         'spawned') can be re-attached with Agent's action \"resume\" — branches and live \
         root sessions are refused"
    )
}

/// `Agent` with `action = "resume"` targeting the caller's own session
/// or one of its ancestors (would re-enter the caller's own run).
pub fn err_resume_into_own_run(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot re-attach to session '{target}': it is the session you are running in or one \
         of its ancestors — pick a spawned session from the session tool's list instead"
    )
}

/// `compact` targeting the caller's own session or one of its
/// ancestors (the engine compacts those automatically when needed).
pub fn err_compact_ancestor(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot compact session '{target}': it is the session you are running in or an \
         ancestor — the engine compacts it automatically when needed"
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

/// Caller's own session metadata is missing from the index (dangling).
/// Privilege grant is refused rather than silently promoting to base —
/// the caller cannot prove they belong to any subtree, so the safe
/// default is to deny.
pub fn err_dangling(cur: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot act on sessions: your current session '{cur}' has no entry in the session \
         store — refusing until it is recovered or re-attached"
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
    fn caller_context_missing_metadata_is_dangling() {
        // Unknown session: classified as dangling, NOT base. The old
        // "missing = base" default was a privilege-escalation hole;
        // dangling callers must be refused at the guard site.
        let ctx = caller_context("ghost", &tree());
        assert!(!ctx.is_base);
        assert!(ctx.dangling);
        assert!(ctx.ancestors.is_empty());
    }

    #[test]
    fn caller_context_privileged_from_metadata() {
        // A spawned session flagged `privileged` keeps its parent
        // pointer (is_base stays false, ancestors intact) — privilege
        // affects guard reach only, not tree membership.
        let mut metas = tree();
        metas
            .iter_mut()
            .find(|m| m.session_id == "spawn1")
            .unwrap()
            .privileged = true;
        let ctx = caller_context("spawn1", &metas);
        assert!(!ctx.is_base);
        assert!(ctx.privileged);
        assert!(!ctx.dangling);
        assert_eq!(ctx.ancestors, vec!["root:user:alice".to_string()]);
        // in_subtree is unchanged: the root is still not in the
        // privileged caller's subtree.
        assert!(!in_subtree(&ctx, "root:user:alice", &metas));

        // Defaults to false for unflagged sessions, and a dangling
        // caller is never privileged.
        assert!(!caller_context("spawn2", &metas).privileged);
        assert!(!caller_context("ghost", &metas).privileged);
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
    fn descendants_of_handles_deep_and_dangling_chains() {
        // Reference implementation: the old O(N²) form, kept here as a
        // parity oracle for the BFS rewrite.
        fn naive_descendants(target: &str, metas: &[SessionMetadata]) -> Vec<String> {
            fn ancestors_of(id: &str, metas: &[SessionMetadata]) -> Vec<String> {
                let find = |id: &str| metas.iter().find(|m| m.session_id == id);
                let mut chain = Vec::new();
                let mut seen = std::collections::HashSet::new();
                let mut cursor = find(id).and_then(|m| m.parent_session_id.clone());
                while let Some(id) = cursor {
                    if !seen.insert(id.clone()) {
                        break;
                    }
                    chain.push(id.clone());
                    cursor = find(&id).and_then(|m| m.parent_session_id.clone());
                }
                chain
            }
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

        // 4-level tree under one base + an unrelated branch + a
        // dangling chain (parent metadata absent).
        let mut metas = vec![
            meta("base", None),
            meta("a", Some("base")),
            meta("b", Some("a")),
            meta("c", Some("b")),
            meta("d", Some("c")),
            meta("sibling1", Some("base")),
            meta("sibling2", Some("base")),
            // dangling: `ghost` is referenced as parent but never exists
            // in the metadata; the BFS must terminate without panicking.
            meta("orphan", Some("ghost")),
        ];

        for target in ["base", "a", "b", "c", "d", "sibling1", "orphan", "ghost"] {
            let mut naive = naive_descendants(target, &metas);
            naive.sort();
            let mut bfs = descendants_of(target, &metas);
            bfs.sort();
            assert_eq!(
                naive, bfs,
                "target={target}: BFS diverged from naive O(N²)"
            );
        }

        // Cycle: x → y → x. Both must be returned when querying x OR y.
        let mut x = meta("x", Some("y"));
        let y = meta("y", Some("x"));
        x.parent_session_id = Some("y".to_string());
        metas.push(x);
        metas.push(y);
        let mut x_desc = descendants_of("x", &metas);
        x_desc.sort();
        assert_eq!(x_desc, vec!["y".to_string()]);
    }

    #[test]
    fn refusals_are_actionable() {
        for err in [
            err_self_mutation("s"),
            err_delete_ancestor("s"),
            err_move_ancestor("s"),
            err_move_cycle("s", "d"),
            err_out_of_tree("s", "c"),
            err_live_base_managed("s"),
            err_run_active("s"),
            err_compact_archived("s"),
            err_compact_ancestor("s"),
            err_resume_archived("s"),
            err_resume_self("s"),
            err_resume_cross_family("s", "c"),
            err_descendants_exist("s", &["d1".to_string()]),
            err_dangling("c"),
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
