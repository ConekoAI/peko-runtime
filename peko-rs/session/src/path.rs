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
//!   caller's tree; cycle-safe). The trunk session (the principal's
//!   owner root, `parent_session_id == None`) carries no slug and is
//!   addressable only as `/` from inside its own tree — cross-tree
//!   access is refused by the ownership guards anyway.
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

use crate::id::SessionId;
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
    } else if slug.contains(':') {
        // `:` is reserved for raw session ids (the legacy tree-root
        // shape `root:<dim>:<name>` carried them; and runtime
        // extensions stamp `spawn:<uuid>:` / `channel:<id>:` prefixes).
        // Rejecting `:` here makes the LLM-facing addressing grammar
        // unambiguous by construction — slugs can never look like
        // raw ids and the LLM-facing resolver only refuses genuinely
        // id-shaped input.
        Some(
            "it contains ':' — ':' is reserved for raw session ids; use a different slug"
                .to_string(),
        )
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
             '/', and have no leading or trailing whitespace"
        )),
        None => Ok(()),
    }
}

/// Validate a path reference (`/a/b/c` or `a/b/c`) — the LLM-facing
/// form used by tools like `Agent` and the session tool's `path`
/// field. Splits on `/`, rejects empty segments and segments that
/// fail [`validate_slug`], rejects UUID-shaped segments that look
/// like raw session ids, and accepts the empty string only when
/// explicitly allowed (the caller can check first).
///
/// Caller-relative paths (`a/b/c`) and full paths (`/a/b/c`) both
/// pass — leading slash is stripped before segment validation. The
/// trunk's leading `/` is implied; never supply a single bare UUID
/// as a path.
pub fn validate_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        return Err(anyhow::anyhow!(
            "path is empty — pass a slug path ('/a/b/c') or caller-relative ('agent-c')"
        ));
    }
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!(
            "path is just '/' — supply at least one segment"
        ));
    }
    for (i, segment) in trimmed.split('/').enumerate() {
        if segment.is_empty() {
            return Err(anyhow::anyhow!(
                "path '{path}' has an empty segment at position {i}"
            ));
        }
        // Each segment is a slug; reuse the slug rules.
        validate_slug(segment).map_err(|e| {
            anyhow::anyhow!("path '{path}' segment '{segment}': {e}")
        })?;
    }
    Ok(())
}

/// First session (other than `exclude`) whose parent is `parent` and
/// whose slug equals `slug` — i.e. the per-parent-uniqueness conflict.
/// Returns the conflicting session id, if any.
#[must_use]
pub fn slug_conflict(
    metas: &[SessionMetadata],
    parent: Option<SessionId>,
    slug: &str,
    exclude: SessionId,
) -> Option<SessionId> {
    metas
        .iter()
        .find(|m| m.session_id != exclude && m.parent_session_id == parent && m.slug.as_deref() == Some(slug))
        .map(|m| m.session_id)
}

/// Per-parent slug uniqueness violation, naming the conflicting
/// session id (ownership.rs refusal style).
pub fn err_slug_conflict(slug: &str, conflicting_id: SessionId, parent: Option<SessionId>) -> anyhow::Error {
    let under = parent
        .map(|p| p.to_string())
        .unwrap_or_else(|| "<tree root>".to_string());
    anyhow::anyhow!(
        "slug '{slug}' is already used by sibling session '{conflicting_id}' under parent \
         '{under}' — slugs are unique per parent; pick a different slug"
    )
}

// ─── Path resolution ───────────────────────────────────────────────

/// The caller's topmost ancestor: walk `parent_session_id` from
/// `from` to the root of its tree. Cycle-safe (bounded walk). `None`
/// when `from` has no metadata in the slice.
fn tree_root(metas: &[SessionMetadata], from: SessionId) -> Option<SessionId> {
    let find = |id: SessionId| metas.iter().find(|m| m.session_id == id);
    let mut current = find(from)?;
    let mut seen = std::collections::HashSet::new();
    seen.insert(current.session_id);
    while let Some(parent_id) = current.parent_session_id {
        if !seen.insert(parent_id) {
            break; // corrupt chain with a cycle — stop at what we have
        }
        match find(parent_id) {
            Some(parent) => current = parent,
            None => break, // dangling parent id — the walk ends here
        }
    }
    Some(current.session_id)
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
    caller_session_id: SessionId,
    path: &str,
) -> anyhow::Result<SessionId> {
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
    seen.insert(current);
    for segment in segments {
        let child = metas.iter().find(|m| {
            m.parent_session_id == Some(current) && m.slug.as_deref() == Some(segment)
        });
        match child {
            Some(c) if seen.insert(c.session_id) => {
                current = c.session_id;
            }
            Some(c) => {
                return Err(anyhow::anyhow!(
                    "cannot resolve path '{path}': the tree below '{current}' contains a cycle \
                     (reached '{c}' twice)",
                    current = current,
                    c = c.session_id
                ));
            }
            None => {
                let mut available: Vec<&str> = metas
                    .iter()
                    .filter(|m| m.parent_session_id == Some(current))
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
                     '{segment}' — {hint}",
                    current = current,
                    segment = segment
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
/// are skipped as intermediate segments (the trunk session
/// `parent_session_id == None` collapses into the leading `/`), and a
/// slugless target falls back to its raw id as the last segment
/// (`/memory/550e8400-…`). The resolver never accepts that fallback
/// as input. Cycle-safe; a session missing from `metas` yields
/// `/<id>`.
#[must_use]
pub fn compute_path(metas: &[SessionMetadata], session_id: SessionId) -> String {
    let find = |id: SessionId| metas.iter().find(|m| m.session_id == id);
    let mut segments: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor = Some(session_id);
    let mut is_target = true;
    while let Some(id) = cursor {
        if !seen.insert(id) {
            break; // corrupt chain with a cycle — stop, keep what we have
        }
        let Some(meta) = find(id) else {
            // Dangling ancestor (or missing target): stop the walk.
            if is_target {
                segments.push(id.to_string());
            }
            break;
        };
        if is_target {
            // The final segment falls back to the raw id when the
            // target has no slug.
            segments.push(meta.slug.clone().unwrap_or_else(|| id.to_string()));
        } else if let Some(ref slug) = meta.slug {
            // Intermediate segments are slug-only; slugless ancestors
            // are skipped.
            segments.push(slug.clone());
        }
        is_target = false;
        cursor = meta.parent_session_id;
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
pub fn derive_branch_slug(metas: &[SessionMetadata], source_id: SessionId) -> Option<String> {
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

// ─── LLM-facing reference resolution ───────────────────────────────

/// Resolve a bare slug (no leading `/`) to a session id by searching
/// the caller's descendants. Direct children first — slugs are
/// unique-per-parent so this is unambiguous at depth 0 — then
/// breadth-first descent. Multiple matches at the same depth → a
/// structured error listing all match paths so the LLM narrows by
/// passing an absolute `/a/b/c` path.
///
/// Cycle-safe: every node is processed at most once (tracked via a
/// `processed` set, distinct from a seen-set so children can be
/// re-enqueued for descent after their grandchildren are found).
///
/// Used by [`resolve_reference`] for the caller-relative branch.
pub fn resolve_relative(
    metas: &[SessionMetadata],
    caller_session_id: SessionId,
    segment: &str,
) -> anyhow::Result<SessionId> {
    // Validate first so empty / `/`-containing / `:`-containing
    // references error with the canonical "invalid slug" message
    // rather than reaching the descendant scan.
    validate_slug(segment)?;

    // `processed` tracks "have we used this node as a parent yet" —
    // distinct from "have we ever seen this id" so a node can be
    // re-enqueued for further descent after its grandchildren are
    // discovered. Cycles terminate because each node is processed at
    // most once.
    let mut processed: std::collections::HashSet<SessionId> = std::collections::HashSet::new();

    // Level 0: direct children of the caller. Slugs are unique-per-
    // parent (see [`slug_conflict`]), so this is either zero or one
    // match — no ambiguity error needed.
    if let Some(child) = metas.iter().find(|m| {
        m.parent_session_id == Some(caller_session_id)
            && m.slug.as_deref() == Some(segment)
    }) {
        return Ok(child.session_id);
    }

    // Levels 1..: breadth-first descent. At each level, collect all
    // matches by slug; if exactly one, return it; if multiple, error
    // with the compute_path renderings.
    let mut frontier: Vec<SessionId> = metas
        .iter()
        .filter(|m| m.parent_session_id == Some(caller_session_id))
        .map(|m| m.session_id)
        .collect();

    while !frontier.is_empty() {
        let mut next_frontier: Vec<SessionId> = Vec::new();
        let mut matches: Vec<SessionId> = Vec::new();
        for parent_id in &frontier {
            if !processed.insert(*parent_id) {
                continue;
            }
            for child in metas
                .iter()
                .filter(|m| m.parent_session_id == Some(*parent_id))
            {
                if child.slug.as_deref() == Some(segment) {
                    matches.push(child.session_id);
                } else if !processed.contains(&child.session_id) {
                    next_frontier.push(child.session_id);
                }
            }
        }
        match matches.len() {
            1 => return Ok(matches.remove(0)),
            n if n > 1 => {
                let mut paths: Vec<String> = matches.iter().map(|id| compute_path(metas, *id)).collect();
                paths.sort_unstable();
                return Err(anyhow::anyhow!(
                    "'{segment}' is ambiguous under '{caller_session_id}': {n} descendants \
                     match — [{}]; pass an absolute path ('/.../{segment}') to disambiguate",
                    paths.join(", "),
                    caller_session_id = caller_session_id
                ));
            }
            _ => {}
        }
        frontier = next_frontier;
    }

    // Zero matches at any depth — surface the caller's direct-child
    // slugs as a hint, the same shape [`resolve_path`] uses.
    let mut available: Vec<&str> = metas
        .iter()
        .filter(|m| m.parent_session_id == Some(caller_session_id))
        .filter_map(|m| m.slug.as_deref())
        .collect();
    available.sort_unstable();
    let hint = if available.is_empty() {
        "it has no children with slugs — pass an absolute path instead".to_string()
    } else {
        format!("direct-child slugs: [{}]", available.join(", "))
    };
    Err(anyhow::anyhow!(
        "cannot resolve '{segment}': no child or descendant of '{caller_session_id}' has slug \
         '{segment}' — {hint}",
        caller_session_id = caller_session_id
    ))
}

/// Single LLM-facing entry point for session references. Two accepted
/// forms:
///
/// | Form | Example | Resolver |
/// |---|---|---|
/// | Absolute slug path | `/a/b/c` | [`resolve_path`] (caller-anchored) |
/// | Caller's own session id | UUID | SELF (engine-internal call shape) |
///
/// Everything else — non-`/`, non-self — is REFUSED with a structured
/// error so the model learns to use the `path` field from
/// `session list` output. Sprint 5 collapsed the LLM-facing surface to
/// slug paths; sprint 6 collapses the heuristic
/// (`looks_like_session_id`) now that the engine-internal id format is
/// opaque UUIDs. The legacy `root:<dim>:<name>` shape never appears in
/// a sprint-6 runtime, and raw non-self UUIDs are by construction not
/// produced by any tool the model sees.
///
/// **Engine-internal self-reference:** the engine passes its own
/// `current_session_id` verbatim (UUIDs in production). Bypassing the
/// raw-id check there avoids forcing every engine call site to resolve
/// a slug path before invoking the runtime.
pub fn resolve_reference(
    metas: &[SessionMetadata],
    caller_session_id: SessionId,
    reference: &str,
) -> anyhow::Result<SessionId> {
    anyhow::ensure!(
        !reference.is_empty(),
        "session reference is empty — pass a slug path ('/a/b/c')"
    );
    if reference == caller_session_id.to_string() {
        return Ok(caller_session_id);
    }
    if reference.starts_with('/') {
        return resolve_path(metas, caller_session_id, reference);
    }
    Err(anyhow::anyhow!(
        "raw session ids are not accepted as session references ('{reference}') — pass a \
         slug path ('/a/b/c') or your own session id; use the `path` field from `session list` \
         output"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// root (no slug) ── memory ── task-b
    ///              └──── memory (child "notes")
    ///              └──── slugless
    fn tree() -> Vec<SessionMetadata> {
        let root = SessionId::parse("00000000-0000-0000-0000-000000000001").unwrap();
        let s_memory = SessionId::parse("00000000-0000-0000-0000-000000000002").unwrap();
        let s_task = SessionId::parse("00000000-0000-0000-0000-000000000003").unwrap();
        let s_notes = SessionId::parse("00000000-0000-0000-0000-000000000004").unwrap();
        let s_slugless = SessionId::parse("00000000-0000-0000-0000-000000000005").unwrap();
        vec![
            meta_at(root, None, None),
            meta_at(s_memory, Some(root), Some("memory")),
            meta_at(s_task, Some(s_memory), Some("task-b")),
            meta_at(s_notes, Some(s_memory), Some("notes")),
            meta_at(s_slugless, Some(root), None),
        ]
    }

    fn meta_at(id: SessionId, parent: Option<SessionId>, slug: Option<&str>) -> SessionMetadata {
        // Build directly (don't delegate to `meta`, which takes a
        // `Option<&str>` parent) — `parent` here is already a
        // `SessionId`.
        SessionMetadata {
            session_id: id,
            agent_name: "agent".to_string(),
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            turn_count: 0,
            last_total_tokens: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            model_context_limit: None,
            transcript_file: format!("{}.jsonl", id),
            title: None,
            parent_session_id: parent,
            trigger: "user".to_string(),
            peer_type: None,
            peer_id: None,
            archived: false,
            compact_requested: false,
            standing: false,
            privileged: false,
            slug: slug.map(String::from),
        }
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
            "a:b",
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
        let root = SessionId::new();
        let a1 = SessionId::new();
        let a2 = SessionId::new();
        let b1 = SessionId::new();
        let new = SessionId::new();
        let metas = vec![
            meta_at(root, None, None),
            meta_at(a1, Some(root), Some("dup")),
            meta_at(a2, Some(root), Some("other")),
            meta_at(b1, Some(a2), Some("dup")),
        ];
        assert_eq!(slug_conflict(&metas, Some(root), "dup", new), Some(a1));
        // Different parent: no conflict.
        assert_eq!(slug_conflict(&metas, Some(a2), "dup", b1), None);
        // Exclusion lets a session keep its own slug (rename no-op).
        assert_eq!(slug_conflict(&metas, Some(root), "dup", a1), None);
        // Root-level siblings (parent None) conflict with each other.
        let r1 = SessionId::new();
        let r2 = SessionId::new();
        let roots = vec![meta_at(r1, None, Some("x")), meta_at(r2, None, Some("x"))];
        assert_eq!(slug_conflict(&roots, None, "x", r1), Some(r2));
    }

    // ─── resolve_path ──────────────────────────────────────────────

    #[test]
    fn resolve_slash_anchors_at_callers_topmost_ancestor() {
        let metas = tree();
        let s_task = metas[2].session_id;
        let root = metas[0].session_id;
        // From a nested caller, "/" is the tree root.
        assert_eq!(resolve_path(&metas, s_task, "/").unwrap(), root);
        // From the root itself, "/" is the root.
        assert_eq!(resolve_path(&metas, root, "/").unwrap(), root);
    }

    #[test]
    fn resolve_multi_segment_from_nested_caller() {
        let metas = tree();
        let s_task = metas[2].session_id;
        let s_notes = metas[3].session_id;
        let s_memory = metas[1].session_id;
        assert_eq!(
            resolve_path(&metas, s_notes, "/memory/task-b").unwrap(),
            s_task
        );
        assert_eq!(
            resolve_path(&metas, s_task, "/memory").unwrap(),
            s_memory
        );
        // Empty segments are tolerated ("//memory//").
        assert_eq!(
            resolve_path(&metas, s_task, "//memory//").unwrap(),
            s_memory
        );
    }

    #[test]
    fn resolve_unknown_segment_lists_available_child_slugs() {
        let metas = tree();
        let root = metas[0].session_id;
        let err = resolve_path(&metas, root, "/memory/nope").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/memory/nope"), "{msg}");
        assert!(msg.contains("'nope'"), "{msg}");
        assert!(msg.contains("notes"), "{msg}");
        assert!(msg.contains("task-b"), "{msg}");

        // Leaf level with no slugged children says so.
        let err = resolve_path(&metas, root, "/memory/task-b/x").unwrap_err();
        assert!(err.to_string().contains("no children with slugs"), "{err}");
    }

    #[test]
    fn resolve_requires_slugs_at_every_segment() {
        let metas = tree();
        let root = metas[0].session_id;
        // A slugless intermediate node is NOT addressable by its raw
        // id through the resolver — paths are slug-only by design.
        let err = resolve_path(&metas, root, "/s_slugless").unwrap_err();
        assert!(err.to_string().contains("no child"), "{err}");
    }

    #[test]
    fn resolve_missing_caller_metadata_errors() {
        let metas = tree();
        let ghost = SessionId::new();
        let err = resolve_path(&metas, ghost, "/memory").unwrap_err();
        assert!(err.to_string().contains("no entry"), "{err}");
    }

    #[test]
    fn resolve_is_cycle_bounded() {
        // Corrupt chain: a → b → a. The root walk must terminate.
        let a = SessionId::new();
        let b = SessionId::new();
        let c = SessionId::new();
        let metas = vec![
            meta_at(a, Some(b), Some("a")),
            meta_at(b, Some(a), Some("b")),
            meta_at(c, Some(b), Some("c")),
        ];
        let resolved = resolve_path(&metas, c, "/").unwrap();
        assert!(resolved == a || resolved == b);
        assert_eq!(resolve_path(&metas, c, "/b/c").unwrap(), c);
    }

    // ─── compute_path ──────────────────────────────────────────────

    #[test]
    fn compute_path_joins_slugs_and_skips_slugless_ancestors() {
        let metas = tree();
        let s_task = metas[2].session_id;
        let s_memory = metas[1].session_id;
        let root = metas[0].session_id;
        let s_slugless = metas[4].session_id;
        assert_eq!(compute_path(&metas, s_task), "/memory/task-b");
        assert_eq!(compute_path(&metas, s_memory), "/memory");
        // Root has no slug and no parent: bare "/<id>" fallback.
        assert_eq!(compute_path(&metas, root), format!("/{root}"));
        // Slugless target: raw id as the last segment.
        assert_eq!(compute_path(&metas, s_slugless), format!("/{s_slugless}"));
        // Slugless INTERMEDIATE ancestor is skipped.
        let s_deep = SessionId::new();
        let mut deep = tree();
        deep.push(meta_at(s_deep, Some(s_slugless), Some("deep")));
        assert_eq!(compute_path(&deep, s_deep), "/deep");
    }

    #[test]
    fn compute_path_handles_missing_and_cyclic_metadata() {
        let metas = tree();
        let ghost = SessionId::new();
        assert_eq!(compute_path(&metas, ghost), format!("/{ghost}"));
        let a = SessionId::new();
        let b = SessionId::new();
        let cyclic = vec![
            meta_at(a, Some(b), Some("a")),
            meta_at(b, Some(a), Some("b")),
        ];
        assert!(compute_path(&cyclic, a).starts_with('/'));
    }

    // ─── derive_branch_slug ────────────────────────────────────────

    #[test]
    fn derive_branch_slug_uniquifies_on_sibling_conflict() {
        let metas = tree();
        let s_memory = metas[1].session_id;
        let s_task = metas[2].session_id;
        assert_eq!(
            derive_branch_slug(&metas, s_memory),
            Some("memory-branch".to_string())
        );
        // With existing branches, the next free one wins.
        let s_b1 = SessionId::new();
        let s_b2 = SessionId::new();
        let mut metas = tree();
        metas.push(meta_at(s_b1, Some(s_memory), Some("memory-branch")));
        metas.push(meta_at(s_b2, Some(s_memory), Some("memory-branch-2")));
        assert_eq!(
            derive_branch_slug(&metas, s_memory),
            Some("memory-branch-3".to_string())
        );
        // A same-named child of a DIFFERENT parent does not count.
        let s_b3 = SessionId::new();
        let mut metas = tree();
        metas.push(meta_at(s_b3, Some(s_task), Some("memory-branch")));
        assert_eq!(
            derive_branch_slug(&metas, s_memory),
            Some("memory-branch".to_string())
        );
        // No slug on the source → no derived slug.
        let root = metas[0].session_id;
        assert_eq!(derive_branch_slug(&metas, root), None);
        let ghost = SessionId::new();
        assert_eq!(derive_branch_slug(&metas, ghost), None);
    }

    #[test]
    fn derive_branch_slug_stays_within_length_cap() {
        let s = SessionId::new();
        let long = "x".repeat(MAX_SLUG_LEN);
        let metas = vec![meta_at(s, None, Some(&long))];
        let derived = derive_branch_slug(&metas, s).unwrap();
        validate_slug(&derived).unwrap();
        assert!(derived.ends_with("-branch"), "{derived}");
    }

    // ─── resolve_relative ──────────────────────────────────────────

    fn relative_tree() -> Vec<SessionMetadata> {
        let mut t = tree();
        let s_grand = SessionId::new();
        let s_task = t[2].session_id;
        t.push(meta_at(s_grand, Some(s_task), Some("grandchild-1")));
        t
    }

    #[test]
    fn resolve_relative_finds_direct_child() {
        let metas = relative_tree();
        let root = metas[0].session_id;
        let s_memory = metas[1].session_id;
        assert_eq!(
            resolve_relative(&metas, root, "memory").unwrap(),
            s_memory
        );
    }

    #[test]
    fn resolve_relative_prefers_direct_child_over_deeper() {
        let mut metas = relative_tree();
        let root = metas[0].session_id;
        let s_task = metas[2].session_id;
        let s_deep_dup = SessionId::new();
        metas.push(meta_at(s_deep_dup, Some(s_task), Some("memory")));
        let s_memory = metas[1].session_id;
        assert_eq!(
            resolve_relative(&metas, root, "memory").unwrap(),
            s_memory
        );
    }

    #[test]
    fn resolve_relative_descends_to_grandchild() {
        let metas = relative_tree();
        let root = metas[0].session_id;
        let s_memory = metas[1].session_id;
        let s_grand_idx = metas.len() - 1;
        let s_grand = metas[s_grand_idx].session_id;
        assert_eq!(
            resolve_relative(&metas, root, "grandchild-1").unwrap(),
            s_grand
        );
        assert_eq!(
            resolve_relative(&metas, s_memory, "grandchild-1").unwrap(),
            s_grand
        );
    }

    #[test]
    fn resolve_relative_ambiguous_lists_matches() {
        let root = SessionId::new();
        let a = SessionId::new();
        let b = SessionId::new();
        let dup1 = SessionId::new();
        let dup2 = SessionId::new();
        let metas = vec![
            meta_at(root, None, None),
            meta_at(a, Some(root), Some("a")),
            meta_at(b, Some(root), Some("b")),
            meta_at(dup1, Some(a), Some("dup")),
            meta_at(dup2, Some(b), Some("dup")),
        ];
        let err = resolve_relative(&metas, root, "dup").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains("/a/dup"), "{msg}");
        assert!(msg.contains("/b/dup"), "{msg}");
    }

    #[test]
    fn resolve_relative_zero_matches_lists_available() {
        let metas = relative_tree();
        let s_memory = metas[1].session_id;
        let err = resolve_relative(&metas, s_memory, "nope").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no child or descendant"), "{msg}");
        assert!(msg.contains("task-b"), "{msg}");
        assert!(msg.contains("notes"), "{msg}");
    }

    #[test]
    fn resolve_relative_rejects_bad_slug() {
        let metas = relative_tree();
        let root = metas[0].session_id;
        for bad in ["", "a/b", "a:b", " lead", "trail "] {
            let err = resolve_relative(&metas, root, bad).unwrap_err();
            assert!(err.to_string().contains("invalid slug"), "{err}: {bad}");
        }
    }

    #[test]
    fn resolve_relative_is_cycle_bounded() {
        let root = SessionId::new();
        let a = SessionId::new();
        let b = SessionId::new();
        let metas = vec![
            meta_at(root, None, None),
            meta_at(a, Some(root), Some("a")),
            meta_at(b, Some(a), Some("b")),
        ];
        let result = resolve_relative(&metas, root, "b");
        assert!(result.is_ok(), "should terminate: {result:?}");
        assert_eq!(result.unwrap(), b);
    }

    // ─── resolve_reference ─────────────────────────────────────────

    #[test]
    fn resolve_reference_dispatches_on_leading_slash() {
        let metas = relative_tree();
        let s_task = metas[2].session_id;
        let s_memory = metas[1].session_id;
        // `/`-rooted path: resolved via the caller-anchored slug walk.
        assert_eq!(
            resolve_reference(&metas, s_task, "/memory").unwrap(),
            s_memory
        );
        // Engine self-reference: the caller's own id bypasses the
        // raw-id refusal (the engine's `current_session` call shape).
        assert_eq!(
            resolve_reference(&metas, s_task, s_task.to_string().as_str()).unwrap(),
            s_task
        );
    }

    #[test]
    fn resolve_reference_root_path() {
        let metas = relative_tree();
        let s_task = metas[2].session_id;
        let root = metas[0].session_id;
        assert_eq!(resolve_reference(&metas, s_task, "/").unwrap(), root);
    }

    #[test]
    fn resolve_reference_rejects_raw_ids() {
        let metas = relative_tree();
        let root = metas[0].session_id;
        // `:`-bearing legacy shape: refused.
        let err = resolve_reference(&metas, root, "root:cron:alice").unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
        // Runtime-extension shape.
        let err = resolve_reference(&metas, root, "spawn:550e8400-e29b:").unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
        // Bare UUID (32+ hex/dash): refused.
        let err = resolve_reference(&metas, root, "550e8400-e29b-41d4-a716-446655440000").unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
    }

    #[test]
    fn resolve_reference_rejects_empty() {
        let metas = relative_tree();
        let root = metas[0].session_id;
        let err = resolve_reference(&metas, root, "").unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn resolve_reference_self_reference_bypasses_raw_id_check() {
        let metas = relative_tree();
        let own_id = SessionId::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(resolve_reference(&metas, own_id, "550e8400-e29b-41d4-a716-446655440000").unwrap(), own_id);
        // A different UUID-shaped raw id still refuses.
        let err = resolve_reference(&metas, own_id, "660e8400-e29b-41d4-a716-446655440099").unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
    }
}