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
    } else if slug.contains(':') {
        // `:` is reserved for raw session ids (the tree root's parent
        // session id, e.g. `root:user:alice`, carries them; and ad-hoc
        // ids like `spawn:<uuid>:`/`channel:<id>:` are introduced via
        // runtime extensions). Rejecting `:` here makes the LLM-facing
        // addressing grammar unambiguous by construction — slugs can
        // never look like raw ids and `looks_like_session_id` only has
        // to flag genuine id shapes.
        Some("it contains ':' — ':' is reserved for raw session ids; use a different slug".to_string())
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

// ─── LLM-facing reference resolution ───────────────────────────────

/// Quick heuristic: does `reference` look like a raw session id?
///
/// True when the value contains `:` (the canonical tree-root shape is
/// `root:<dimension>:<name>`; runtime extensions stamp `spawn:<uuid>:`
/// or `channel:<id>:` prefixes), or when the value is a bare
/// UUID/hex blob of length ≥ 32. The shape-based dispatch is the
/// primary defense; this is a defense-in-depth nudge so a model
/// that tries to pass a raw id anyway hits a structured refusal
/// rather than getting a confusing "not found" later.
#[must_use]
pub fn looks_like_session_id(reference: &str) -> bool {
    if reference.is_empty() {
        return false;
    }
    if reference.contains(':') {
        return true;
    }
    if reference.len() >= 32 {
        let only_hex_or_dash = reference
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-');
        if only_hex_or_dash {
            return true;
        }
    }
    false
}

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
    caller_session_id: &str,
    segment: &str,
) -> anyhow::Result<String> {
    // Validate first so empty / `/`-containing / `:`-containing
    // references error with the canonical "invalid slug" message
    // rather than reaching the descendant scan.
    validate_slug(segment)?;

    // `processed` tracks "have we used this node as a parent yet" —
    // distinct from "have we ever seen this id" so a node can be
    // re-enqueued for further descent after its grandchildren are
    // discovered. Cycles terminate because each node is processed at
    // most once.
    let mut processed: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Level 0: direct children of the caller. Slugs are unique-per-
    // parent (see [`slug_conflict`]), so this is either zero or one
    // match — no ambiguity error needed.
    if let Some(child) = metas.iter().find(|m| {
        m.parent_session_id.as_deref() == Some(caller_session_id)
            && m.slug.as_deref() == Some(segment)
    }) {
        return Ok(child.session_id.clone());
    }

    // Levels 1..: breadth-first descent. At each level, collect all
    // matches by slug; if exactly one, return it; if multiple, error
    // with the compute_path renderings.
    let mut frontier: Vec<String> = metas
        .iter()
        .filter(|m| m.parent_session_id.as_deref() == Some(caller_session_id))
        .map(|m| m.session_id.clone())
        .collect();

    while !frontier.is_empty() {
        let mut next_frontier: Vec<String> = Vec::new();
        let mut matches: Vec<String> = Vec::new();
        for parent_id in &frontier {
            if !processed.insert(parent_id.clone()) {
                continue;
            }
            for child in metas.iter().filter(|m| {
                m.parent_session_id.as_deref() == Some(parent_id.as_str())
            }) {
                if child.slug.as_deref() == Some(segment) {
                    matches.push(child.session_id.clone());
                } else if !processed.contains(&child.session_id) {
                    next_frontier.push(child.session_id.clone());
                }
            }
        }
        match matches.len() {
            1 => return Ok(matches.remove(0)),
            n if n > 1 => {
                let mut paths: Vec<String> = matches
                    .iter()
                    .map(|id| compute_path(metas, id))
                    .collect();
                paths.sort_unstable();
                return Err(anyhow::anyhow!(
                    "'{segment}' is ambiguous under '{caller_session_id}': {n} descendants \
                     match — [{}]; pass an absolute path ('/.../{segment}') to disambiguate",
                    paths.join(", ")
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
        .filter(|m| m.parent_session_id.as_deref() == Some(caller_session_id))
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
         '{segment}' — {hint}"
    ))
}

/// Single LLM-facing entry point for session references. Three forms:
///
/// | Form | Example | Resolver |
/// |---|---|---|
/// | Absolute slug path | `/a/b/c` | [`resolve_path`] (caller-anchored) |
/// | Caller-relative slug | `agent-c` | [`resolve_relative`] (BFS by depth) |
/// | Raw session id | `root:user:alice`, `550e8400-…` | REFUSED with structured error |
///
/// The raw-id branch exists so the model gets an actionable refusal
/// rather than a confusing descendant-search miss when it tries to
/// pass a raw id it picked up from a prior tool call (the Agent
/// spawn response now emits `path`/`slug` only — see commit 2 — but
/// old examples in training data may still produce ids).
///
/// **Engine-internal self-reference:** if `reference` equals
/// `caller_session_id` exactly, return it directly. This is the
/// shape of the engine's internal `current_session` call (e.g.
/// `session status` with no `session_key`) and bypassing the
/// raw-id check there avoids forcing every engine call site to
/// resolve a slug path before invoking the runtime. The LLM-facing
/// surface still refuses raw ids via the three-form grammar — the
/// `looks_like_session_id` check fires before the self-reference
/// shortcut.
pub fn resolve_reference(
    metas: &[SessionMetadata],
    caller_session_id: &str,
    reference: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        !reference.is_empty(),
        "session reference is empty — pass a slug path ('/a/b/c') or a caller-relative slug \
         ('agent-c')"
    );
    if reference.starts_with('/') {
        return resolve_path(metas, caller_session_id, reference);
    }
    if looks_like_session_id(reference) {
        // Engine-internal self-reference: the engine passes its own
        // current_session_id verbatim (UUIDs in production). Allow
        // it through; refuse all other raw ids.
        if reference == caller_session_id {
            return Ok(reference.to_string());
        }
        return Err(anyhow::anyhow!(
            "raw session ids are not accepted as session references ('{reference}') — pass a \
             slug path ('/a/b/c') or a caller-relative slug ('agent-c'); use the `path` field \
             from `session list` output"
        ));
    }
    resolve_relative(metas, caller_session_id, reference)
}

/// Engine-internal resolver for the three-form grammar.
///
/// Same dispatch as [`resolve_reference`], but raw ids are accepted as-is
/// instead of refused. Used by engine entrypoints
/// (`resume_preflight`, `request_compaction`, `validate_context_parent`)
/// where the id comes from a trusted source (the peer-child session id
/// just minted by `spawn_child`, the caller's own `current_session_key`,
/// a slug path the tool layer already resolved). Existence is validated
/// by the per-call guards.
///
/// **Deliberate divergence from [`resolve_reference`]:** the LLM-facing
/// tool layer uses `resolve_reference` so the model sees a structured
/// refusal when it passes a raw id. Engine-internal code holds canonical
/// session ids it produced itself — applying the refusal heuristic there
/// would force every engine call site to re-shape its input, with no
/// safety gain. Absolute slug paths are still resolved (the tool layer
/// hands back paths it resolved from the LLM's input — they may travel
/// through the engine on the way to a guard).
pub fn resolve_id_or_path(
    metas: &[SessionMetadata],
    caller_session_id: &str,
    reference: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        !reference.is_empty(),
        "session reference is empty — pass a slug path ('/a/b/c') or a raw session id"
    );
    if reference.starts_with('/') {
        return resolve_path(metas, caller_session_id, reference);
    }
    // Engine-internal: trust the input as a raw id. The runtime hands
    // back canonical ids it produced itself; existence is validated by
    // the per-call guards, not by a shape heuristic.
    Ok(reference.to_string())
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

    // ─── looks_like_session_id ─────────────────────────────────────

    #[test]
    fn looks_like_session_id_heuristic() {
        // Contains ':' — the canonical tree-root shape and the
        // extension prefixes.
        assert!(looks_like_session_id("root:user:alice"));
        assert!(looks_like_session_id("spawn:550e8400:"));
        assert!(looks_like_session_id("channel:abc:"));

        // Long all-hex (with dashes) blob.
        assert!(looks_like_session_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(looks_like_session_id(&"a".repeat(32)));

        // Slugs must NOT trigger the heuristic.
        assert!(!looks_like_session_id("memory"));
        assert!(!looks_like_session_id("task-b"));
        assert!(!looks_like_session_id("agent-c"));

        // Short hex-ish values still look like slugs.
        assert!(!looks_like_session_id("deadbeef"));
        assert!(!looks_like_session_id("550e8400"));

        // Empty string falls through.
        assert!(!looks_like_session_id(""));

        // Long but mixed with non-hex/dash — slug-ish.
        assert!(!looks_like_session_id(&format!("{}x", "a".repeat(32))));
    }

    // ─── resolve_relative ──────────────────────────────────────────

    /// Like `tree()`, but adds a grandchild under `s_task` so descent
    /// tests can find it without modifying the canonical tree shape.
    fn relative_tree() -> Vec<SessionMetadata> {
        let mut t = tree();
        t.push(meta("s_grand", Some("s_task"), Some("grandchild-1")));
        t
    }

    #[test]
    fn resolve_relative_finds_direct_child() {
        let metas = relative_tree();
        assert_eq!(
            resolve_relative(&metas, "root:user:alice", "memory").unwrap(),
            "s_memory"
        );
    }

    #[test]
    fn resolve_relative_prefers_direct_child_over_deeper() {
        // Add a deeper node also named "memory" under s_task — the
        // direct child must still win at depth 0.
        let mut metas = relative_tree();
        metas.push(meta("s_deep_dup", Some("s_task"), Some("memory")));
        assert_eq!(
            resolve_relative(&metas, "root:user:alice", "memory").unwrap(),
            "s_memory"
        );
    }

    #[test]
    fn resolve_relative_descends_to_grandchild() {
        let metas = relative_tree();
        assert_eq!(
            resolve_relative(&metas, "root:user:alice", "grandchild-1").unwrap(),
            "s_grand"
        );
        // And from a nested caller.
        assert_eq!(
            resolve_relative(&metas, "s_memory", "grandchild-1").unwrap(),
            "s_grand"
        );
    }

    #[test]
    fn resolve_relative_ambiguous_lists_matches() {
        // Two grandchildren named "dup" under different parents of
        // root:user:alice.
        let metas = vec![
            meta("root:user:alice", None, None),
            meta("a", Some("root:user:alice"), Some("a")),
            meta("b", Some("root:user:alice"), Some("b")),
            meta("dup1", Some("a"), Some("dup")),
            meta("dup2", Some("b"), Some("dup")),
        ];
        let err = resolve_relative(&metas, "root:user:alice", "dup").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains("/a/dup"), "{msg}");
        assert!(msg.contains("/b/dup"), "{msg}");
    }

    #[test]
    fn resolve_relative_zero_matches_lists_available() {
        let metas = relative_tree();
        let err = resolve_relative(&metas, "s_memory", "nope").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no child or descendant"), "{msg}");
        assert!(msg.contains("task-b"), "{msg}"); // direct-child hint
        assert!(msg.contains("notes"), "{msg}");
    }

    #[test]
    fn resolve_relative_rejects_bad_slug() {
        let metas = relative_tree();
        for bad in ["", "a/b", "a:b", " lead", "trail "] {
            let err = resolve_relative(&metas, "root:user:alice", bad).unwrap_err();
            assert!(err.to_string().contains("invalid slug"), "{err}: {bad}");
        }
    }

    #[test]
    fn resolve_relative_is_cycle_bounded() {
        // 2-cycle entirely inside the caller's subtree. Without
        // cycle-bounding the BFS would walk a → b → a → b → …
        // forever.
        let metas = vec![
            meta("root", None, None),
            meta("a", Some("root"), Some("a")),
            // b's parent is a, a's parent is b — 2-cycle.
            meta("b", Some("a"), Some("b")),
        ];
        // Termination check: must return within bounded time and
        // find b's id (it sits at depth 1 and matches directly).
        let result = resolve_relative(&metas, "root", "b");
        assert!(result.is_ok(), "should terminate: {result:?}");
        assert_eq!(result.unwrap(), "b");
    }

    // ─── resolve_reference (the LLM-facing entry point) ────────────

    #[test]
    fn resolve_reference_dispatches_on_leading_slash() {
        let metas = relative_tree();
        // Absolute path → resolve_path.
        assert_eq!(
            resolve_reference(&metas, "s_task", "/memory").unwrap(),
            "s_memory"
        );
        // Bare slug → resolve_relative.
        assert_eq!(
            resolve_reference(&metas, "root:user:alice", "memory").unwrap(),
            "s_memory"
        );
    }

    #[test]
    fn resolve_reference_root_path() {
        let metas = relative_tree();
        // "/" pinned to tree root (matches resolve_path behavior).
        assert_eq!(
            resolve_reference(&metas, "s_task", "/").unwrap(),
            "root:user:alice"
        );
    }

    #[test]
    fn resolve_reference_rejects_raw_ids() {
        let metas = relative_tree();
        // Tree-root shape (NOT the caller's own id, which would
        // trigger the self-reference shortcut).
        let err =
            resolve_reference(&metas, "root:user:alice", "root:cron:alice").unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
        // Runtime-extension shape.
        let err = resolve_reference(&metas, "root:user:alice", "spawn:550e8400-e29b:").unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
        // Bare UUID.
        let err = resolve_reference(
            &metas,
            "root:user:alice",
            "550e8400-e29b-41d4-a716-446655440000",
        )
        .unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
    }

    #[test]
    fn resolve_reference_rejects_empty() {
        let metas = relative_tree();
        let err = resolve_reference(&metas, "root:user:alice", "").unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn resolve_reference_self_reference_bypasses_raw_id_check() {
        // Engine-internal: the engine passes its own current_session
        // id verbatim (a UUID). The self-reference shortcut lets it
        // through; non-self raw ids still refuse.
        let metas = relative_tree();
        let own_id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            resolve_reference(&metas, own_id, own_id).unwrap(),
            own_id
        );
        // But a DIFFERENT raw id (matching `looks_like_session_id`) still
        // refuses. The UUID-shape heuristic catches both 32+ hex/dash
        // and `:`-bearing ids.
        let err = resolve_reference(
            &metas,
            own_id,
            "660e8400-e29b-41d4-a716-446655440099",
        )
        .unwrap_err();
        assert!(err.to_string().contains("raw session ids are not accepted"), "{err}");
    }

    /// Engine-internal passthrough: raw ids are returned verbatim
/// (existence is validated by the calling guards). Absolute slug
/// paths are still resolved — the tool layer hands back paths it
/// resolved from the LLM's input and they may travel through the
/// engine on the way to a guard.
    #[test]
    fn resolve_id_or_path_accepts_raw_ids_and_resolves_paths() {
        let metas = relative_tree();
        // UUID-shaped raw id: returned unchanged.
        let fresh_peer_child = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            resolve_id_or_path(&metas, "any", fresh_peer_child).unwrap(),
            fresh_peer_child
        );
        // `:`-bearing legacy id: returned unchanged.
        let legacy = "root:user:alice";
        assert_eq!(resolve_id_or_path(&metas, "any", legacy).unwrap(), legacy);
        // Short test-fixture id: also returned unchanged.
        let fixture = "branch-a";
        assert_eq!(resolve_id_or_path(&metas, "any", fixture).unwrap(), fixture);
        // Absolute slug path: resolved via the same dispatch as the
        // LLM-facing resolver.
        assert_eq!(
            resolve_id_or_path(&metas, "root:user:alice", "/memory/task-b/grandchild-1").unwrap(),
            "s_grand"
        );
        // Empty: refused.
        let err = resolve_id_or_path(&metas, "any", "").unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }
}
