//! Session path addressing (slugs) — pure functions over metadata
//! slices, in the same style as root's `session::ownership` module:
//! no IO, no locks, structured LLM-actionable errors.
//!
//! Sessions form trees via `SessionMetadata::parent_session_id`. Each
//! session may carry a **slug** — a per-parent-unique path segment —
//! so a session is addressable as `/a/b/c` from anywhere inside its
//! own tree. Ids remain the canonical key everywhere (storage,
//! permits, registries); paths are a computed view only, resolved to
//! ids at the tool-runtime boundary.
//!
//! ## Resolver semantics
//!
//! - `/` alone → the caller's **topmost ancestor** (the walk follows
//!   `parent_session_id` from the caller's session to the root of the
//!   caller's tree; cycle-safe). Root `root:*` sessions carry no slug
//!   and are addressable only as `/` from inside their own tree —
//!   cross-tree access is refused by the ownership guards anyway.
//! - `/a/b/c` → start at that same topmost ancestor; each segment
//!   selects the child of the current node whose slug equals the
//!   segment. Every segment must be a slug — raw ids are NOT accepted
//!   as intermediate segments (a node without a slug is simply not
//!   addressable by path; use its raw id instead).
//! - Unknown segment → a structured error listing the available child
//!   slugs at the failing level.
//!
//! [`compute_path`] is the display-side inverse (used by
//! `session list`): ancestors without a slug are skipped as
//! intermediate segments, and a slugless target falls back to its raw
//! id as the last segment. That fallback is display-only — the
//! resolver never accepts it as input.

use crate::metadata::SessionMetadata;

/// Maximum slug length in characters (keeps paths readable and
/// index entries small).
pub const MAX_SLUG_LEN: usize = 64;

// ─── Slug validation + uniqueness ──────────────────────────────────

/// Validate a slug's format: nonempty, no `/`, no leading/trailing
/// whitespace, at most [`MAX_SLUG_LEN`] chars. Structured error
/// otherwise.
pub fn validate_slug(slug: &str) -> anyhow::Result<()> {
    let reason = if slug.is_empty() {
        Some("it is empty".to_string())
    } else if slug.contains('/') {
        Some("it contains '/' — a slug is a single path segment".to_string())
    } else if slug.trim() != slug {
        Some("it has leading or trailing whitespace".to_string())
    } else if slug.chars().count() > MAX_SLUG_LEN {
        Some(format!("it is longer than {MAX_SLUG_LEN} chars"))
    } else {
        None
    };
    match reason {
        Some(reason) => Err(anyhow::anyhow!(
            "invalid slug '{slug}': {reason} — slugs are 1-{MAX_SLUG_LEN} chars, contain no \
             '/', and have no leading/trailing whitespace"
        )),
        None => Ok(()),
    }
}

/// First session (other than `exclude`) whose parent is `parent` and
/// whose slug equals `slug` — i.e. the per-parent-uniqueness conflict.
/// Returns the conflicting session id, if any.
#[must_use]
pub fn slug_conflict(
    metas: &[SessionMetadata],
    parent: Option<&str>,
    slug: &str,
    exclude: &str,
) -> Option<String> {
    metas
        .iter()
        .find(|m| {
            m.session_id != exclude
                && m.parent_session_id.as_deref() == parent
                && m.slug.as_deref() == Some(slug)
        })
        .map(|m| m.session_id.clone())
}

/// Per-parent slug uniqueness violation, naming the conflicting
/// session id (ownership.rs refusal style).
pub fn err_slug_conflict(slug: &str, conflicting_id: &str, parent: Option<&str>) -> anyhow::Error {
    let under = parent.unwrap_or("<tree root>");
    anyhow::anyhow!(
        "slug '{slug}' is already used by sibling session '{conflicting_id}' under parent \
         '{under}' — slugs are unique per parent; pick a different slug"
    )
}

// ─── Path resolution ───────────────────────────────────────────────

/// The caller's topmost ancestor: walk `parent_session_id` from
/// `from` to the root of its tree. Cycle-safe (bounded walk). `None`
/// when `from` has no metadata in the slice.
fn tree_root(metas: &[SessionMetadata], from: &str) -> Option<String> {
    let find = |id: &str| metas.iter().find(|m| m.session_id == id);
    let mut current = find(from)?;
    let mut seen = std::collections::HashSet::new();
    seen.insert(current.session_id.clone());
    while let Some(parent_id) = current.parent_session_id.clone() {
        if !seen.insert(parent_id.clone()) {
            break; // corrupt chain with a cycle — stop at what we have
        }
        match find(&parent_id) {
            Some(parent) => current = parent,
            None => break, // dangling parent id — the walk ends here
        }
    }
    Some(current.session_id.clone())
}

/// Resolve a `/`-rooted session path to a session id.
///
/// `caller_session_id` anchors the lookup: `/` is the root of the
/// CALLER's tree, never a global root. Values not starting with `/`
/// are not paths — callers treat them as raw session ids and never
/// reach this function.
///
/// Errors (structured, actionable): caller metadata missing; unknown
/// segment (lists the available child slugs at the failing level).
pub fn resolve_path(
    metas: &[SessionMetadata],
    caller_session_id: &str,
    path: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        path.starts_with('/'),
        "session path '{path}' must start with '/' — pass a raw session id to address by id"
    );
    let root = tree_root(metas, caller_session_id).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot resolve path '{path}': your current session '{caller_session_id}' has no \
             entry in the session store"
        )
    })?;

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut current = root;
    let mut seen = std::collections::HashSet::new();
    seen.insert(current.clone());
    for segment in segments {
        let child = metas.iter().find(|m| {
            m.parent_session_id.as_deref() == Some(current.as_str())
                && m.slug.as_deref() == Some(segment)
        });
        match child {
            Some(c) if seen.insert(c.session_id.clone()) => {
                current = c.session_id.clone();
            }
            Some(c) => {
                return Err(anyhow::anyhow!(
                    "cannot resolve path '{path}': the tree below '{}' contains a cycle \
                     (reached '{}' twice)",
                    current,
                    c.session_id
                ));
            }
            None => {
                let mut available: Vec<&str> = metas
                    .iter()
                    .filter(|m| m.parent_session_id.as_deref() == Some(current.as_str()))
                    .filter_map(|m| m.slug.as_deref())
                    .collect();
                available.sort_unstable();
                let hint = if available.is_empty() {
                    "it has no children with slugs — address children by raw session id instead"
                        .to_string()
                } else {
                    format!("available child slugs: [{}]", available.join(", "))
                };
                return Err(anyhow::anyhow!(
                    "cannot resolve path '{path}': no child of '{current}' has slug \
                     '{segment}' — {hint}"
                ));
            }
        }
    }
    Ok(current)
}

// ─── Display-side path computation ─────────────────────────────────

/// Compute the absolute display path of a session, e.g.
/// `/memory/task-b`.
///
/// Display-only inverse of [`resolve_path`]: ancestors without a slug
/// are skipped as intermediate segments (the tree root `root:*`
/// collapses into the leading `/`), and a slugless target falls back
/// to its raw id as the last segment (`/memory/550e8400-…`). The
/// resolver never accepts that fallback as input. Cycle-safe; a
/// session missing from `metas` yields `/<id>`.
#[must_use]
pub fn compute_path(metas: &[SessionMetadata], session_id: &str) -> String {
    let find = |id: &str| metas.iter().find(|m| m.session_id == id);
    let mut segments: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor = Some(session_id.to_string());
    let mut is_target = true;
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            break; // corrupt chain with a cycle — stop, keep what we have
        }
        let Some(meta) = find(&id) else {
            // Dangling ancestor (or missing target): stop the walk.
            if is_target {
                segments.push(id.clone());
            }
            break;
        };
        if is_target {
            // The final segment falls back to the raw id when the
            // target has no slug.
            segments.push(meta.slug.clone().unwrap_or_else(|| id.clone()));
        } else if let Some(ref slug) = meta.slug {
            // Intermediate segments are slug-only; slugless ancestors
            // are skipped.
            segments.push(slug.clone());
        }
        is_target = false;
        cursor = meta.parent_session_id.clone();
    }
    segments.reverse();
    format!("/{}", segments.join("/"))
}

// ─── Branch slug derivation ────────────────────────────────────────

/// Derive the slug for a branch of `source_id`: `<source-slug>-branch`,
/// uniquified among the source's children as `<source-slug>-branch-2`,
/// `-3`, … on conflict. The branch is a CHILD of its source (see
/// `SessionManager::branch_session_by_id`), so siblings are the
/// source's other children. `None` when the source has no slug or is
/// missing from the slice.
///
/// The derived slug is truncated so the result always passes
/// [`validate_slug`] even for a near-cap source slug.
#[must_use]
pub fn derive_branch_slug(metas: &[SessionMetadata], source_id: &str) -> Option<String> {
    let source = metas.iter().find(|m| m.session_id == source_id)?;
    let source_slug = source.slug.as_deref()?;
    // Reserve room for "-branch" plus a "-NN" uniquifier suffix.
    let prefix: String = source_slug.chars().take(MAX_SLUG_LEN - 10).collect();
    let base = format!("{prefix}-branch");
    if slug_conflict(metas, Some(source_id), &base, source_id).is_none() {
        return Some(base);
    }
    for n in 2..100 {
        let candidate = format!("{base}-{n}");
        if slug_conflict(metas, Some(source_id), &candidate, source_id).is_none() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, parent: Option<&str>, slug: Option<&str>) -> SessionMetadata {
        let mut m = SessionMetadata::new(id, "agent", format!("{id}.jsonl"));
        m.parent_session_id = parent.map(String::from);
        m.slug = slug.map(String::from);
        m
    }

    /// root (no slug) ── memory ── task-b
    ///              └──── memory (child "notes")
    ///              └──── slugless
    fn tree() -> Vec<SessionMetadata> {
        vec![
            meta("root:user:alice", None, None),
            meta("s_memory", Some("root:user:alice"), Some("memory")),
            meta("s_task", Some("s_memory"), Some("task-b")),
            meta("s_notes", Some("s_memory"), Some("notes")),
            meta("s_slugless", Some("root:user:alice"), None),
        ]
    }

    // ─── validate_slug ─────────────────────────────────────────────

    #[test]
    fn validate_slug_accepts_simple_segments() {
        for ok in ["a", "task-b", "Memory_2", &"x".repeat(MAX_SLUG_LEN)] {
            validate_slug(ok).unwrap();
        }
    }

    #[test]
    fn validate_slug_rejects_bad_shapes() {
        for bad in [
            "",
            "a/b",
            "/lead",
            "trail/",
            " lead",
            "trail ",
            &"x".repeat(MAX_SLUG_LEN + 1),
        ] {
            let err = validate_slug(bad).unwrap_err();
            assert!(err.to_string().contains("invalid slug"), "{err}");
        }
    }

    // ─── slug_conflict ─────────────────────────────────────────────

    #[test]
    fn slug_conflict_is_per_parent() {
        let metas = vec![
            meta("root", None, None),
            meta("a1", Some("root"), Some("dup")),
            meta("a2", Some("root"), Some("other")),
            meta("b1", Some("a2"), Some("dup")), // same slug, different parent
        ];
        assert_eq!(
            slug_conflict(&metas, Some("root"), "dup", "new"),
            Some("a1".to_string())
        );
        // Different parent: no conflict.
        assert_eq!(slug_conflict(&metas, Some("a2"), "dup", "b1"), None);
        // Exclusion lets a session keep its own slug (rename no-op).
        assert_eq!(slug_conflict(&metas, Some("root"), "dup", "a1"), None);
        // Root-level siblings (parent None) conflict with each other.
        let roots = vec![meta("r1", None, Some("x")), meta("r2", None, Some("x"))];
        assert_eq!(
            slug_conflict(&roots, None, "x", "r1"),
            Some("r2".to_string())
        );
    }

    // ─── resolve_path ──────────────────────────────────────────────

    #[test]
    fn resolve_slash_anchors_at_callers_topmost_ancestor() {
        let metas = tree();
        // From a nested caller, "/" is the tree root…
        assert_eq!(
            resolve_path(&metas, "s_task", "/").unwrap(),
            "root:user:alice"
        );
        // …and from the root itself, "/" is the root.
        assert_eq!(
            resolve_path(&metas, "root:user:alice", "/").unwrap(),
            "root:user:alice"
        );
    }

    #[test]
    fn resolve_multi_segment_from_nested_caller() {
        let metas = tree();
        assert_eq!(
            resolve_path(&metas, "s_notes", "/memory/task-b").unwrap(),
            "s_task"
        );
        assert_eq!(
            resolve_path(&metas, "s_task", "/memory").unwrap(),
            "s_memory"
        );
        // Empty segments are tolerated ("//memory//").
        assert_eq!(
            resolve_path(&metas, "s_task", "//memory//").unwrap(),
            "s_memory"
        );
    }

    #[test]
    fn resolve_unknown_segment_lists_available_child_slugs() {
        let metas = tree();
        let err = resolve_path(&metas, "root:user:alice", "/memory/nope").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/memory/nope"), "{msg}");
        assert!(msg.contains("'nope'"), "{msg}");
        assert!(msg.contains("notes"), "{msg}");
        assert!(msg.contains("task-b"), "{msg}");

        // Leaf level with no slugged children says so.
        let err = resolve_path(&metas, "root:user:alice", "/memory/task-b/x").unwrap_err();
        assert!(err.to_string().contains("no children with slugs"), "{err}");
    }

    #[test]
    fn resolve_requires_slugs_at_every_segment() {
        // A slugless intermediate node is NOT addressable by its raw
        // id through the resolver — paths are slug-only by design.
        let metas = tree();
        let err = resolve_path(&metas, "root:user:alice", "/s_slugless").unwrap_err();
        assert!(err.to_string().contains("no child"), "{err}");
    }

    #[test]
    fn resolve_missing_caller_metadata_errors() {
        let metas = tree();
        let err = resolve_path(&metas, "ghost", "/memory").unwrap_err();
        assert!(err.to_string().contains("no entry"), "{err}");
    }

    #[test]
    fn resolve_is_cycle_bounded() {
        // Corrupt chain: a → b → a. The root walk must terminate.
        let metas = vec![
            meta("a", Some("b"), Some("a")),
            meta("b", Some("a"), Some("b")),
            meta("c", Some("b"), Some("c")),
        ];
        // Terminates (which node "wins" as root is irrelevant for a
        // corrupt tree; termination is the contract).
        let resolved = resolve_path(&metas, "c", "/").unwrap();
        assert!(resolved == "a" || resolved == "b");
        // And a descent from that bounded root still works.
        assert_eq!(resolve_path(&metas, "c", "/b/c").unwrap(), "c");
    }

    // ─── compute_path ──────────────────────────────────────────────

    #[test]
    fn compute_path_joins_slugs_and_skips_slugless_ancestors() {
        let metas = tree();
        assert_eq!(compute_path(&metas, "s_task"), "/memory/task-b");
        assert_eq!(compute_path(&metas, "s_memory"), "/memory");
        // Root has no slug and no parent: bare "/<id>" fallback.
        assert_eq!(compute_path(&metas, "root:user:alice"), "/root:user:alice");
        // Slugless target: raw id as the last segment.
        assert_eq!(compute_path(&metas, "s_slugless"), "/s_slugless");
        // Slugless INTERMEDIATE ancestor is skipped.
        let mut deep = tree();
        deep.push(meta("s_deep", Some("s_slugless"), Some("deep")));
        assert_eq!(compute_path(&deep, "s_deep"), "/deep");
    }

    #[test]
    fn compute_path_handles_missing_and_cyclic_metadata() {
        let metas = tree();
        assert_eq!(compute_path(&metas, "ghost"), "/ghost");
        let cyclic = vec![
            meta("a", Some("b"), Some("a")),
            meta("b", Some("a"), Some("b")),
        ];
        // Terminates; exact shape is irrelevant for corrupt metadata.
        assert!(compute_path(&cyclic, "a").starts_with('/'));
    }

    // ─── derive_branch_slug ────────────────────────────────────────

    #[test]
    fn derive_branch_slug_uniquifies_on_sibling_conflict() {
        let metas = tree();
        assert_eq!(
            derive_branch_slug(&metas, "s_memory"),
            Some("memory-branch".to_string())
        );
        // With an existing "memory-branch" child, the next free one wins.
        let mut metas = tree();
        metas.push(meta("s_b1", Some("s_memory"), Some("memory-branch")));
        metas.push(meta("s_b2", Some("s_memory"), Some("memory-branch-2")));
        assert_eq!(
            derive_branch_slug(&metas, "s_memory"),
            Some("memory-branch-3".to_string())
        );
        // A same-named child of a DIFFERENT parent does not count.
        let mut metas = tree();
        metas.push(meta("s_b1", Some("s_task"), Some("memory-branch")));
        assert_eq!(
            derive_branch_slug(&metas, "s_memory"),
            Some("memory-branch".to_string())
        );
        // No slug on the source → no derived slug.
        assert_eq!(derive_branch_slug(&metas, "root:user:alice"), None);
        assert_eq!(derive_branch_slug(&metas, "ghost"), None);
    }

    #[test]
    fn derive_branch_slug_stays_within_length_cap() {
        let long = "x".repeat(MAX_SLUG_LEN);
        let metas = vec![meta("s", None, Some(&long))];
        let derived = derive_branch_slug(&metas, "s").unwrap();
        validate_slug(&derived).unwrap();
        assert!(derived.ends_with("-branch"), "{derived}");
    }
}
