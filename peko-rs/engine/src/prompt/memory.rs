//! Per-principal long-term memory (`kb/MEMORY.md`) and shared
//! directory-scoped context (`AGENTS.md`).
//!
//! Two complementary surfaces:
//!
//! - **MEMORY.md** lives at `<principal_workspace>/kb/MEMORY.md` —
//!   inside the principal's persistent knowledge base (ADR-055), as
//!   the hot, always-rendered member of the pinned hot set. It is
//!   loaded at session start and injected into the system prompt at
//!   the `{{memory}}` placeholder when the template opts in. The
//!   principal owns this file and may update it via `Write`. The
//!   second hot file, `kb/index.md` (the kb map), and the targeted
//!   scope notes (`kb/groups/<channel>.md`, `kb/agents/<name>.md` —
//!   ADR-055 D8) render as `<runtime-context>` tail sections via
//!   [`load_kb_index`] / [`load_binding_note`] / [`load_agent_note`].
//!
//! - **AGENTS.md** lives at arbitrary directories the principal
//!   touches during a session. The framework no longer auto-injects
//!   this file; the helpers `discover_shared_context` and
//!   `directory_from_tool_params` are exposed here so individual
//!   agent extensions can implement their own AGENTS.md handling if
//!   they want it. Missing files simply omit the section.
//!
//! Both are conventions rather than required files.

use std::path::{Path, PathBuf};

/// Directory (relative to the principal workspace) holding the
/// principal's persistent knowledge base (ADR-055). The pinned hot
/// set (`MEMORY.md`, `index.md`) lives here, as do the cold
/// conventions (`people/`, `groups/`, `agents/`) that feed the
/// targeted scope injections (`load_binding_note`, `load_agent_note`).
pub const KB_DIR: &str = "kb";

/// Filename peko uses for per-principal long-term memory. Resolved
/// under [`KB_DIR`] — `<principal_workspace>/kb/MEMORY.md`.
pub const PRINCIPAL_MEMORY_FILE: &str = "MEMORY.md";

/// Filename peko uses for the kb map (ADR-055 D2: the second hot
/// file). Resolved under [`KB_DIR`] —
/// `<principal_workspace>/kb/index.md`.
pub const KB_INDEX_FILE: &str = "index.md";

/// Subdirectory (under [`KB_DIR`]) of per-group notes keyed by
/// channel/group id (ADR-055 D8 binding-note injection).
pub const KB_GROUPS_DIR: &str = "groups";

/// Subdirectory (under [`KB_DIR`]) of per-agent notes keyed by agent
/// name (ADR-055 D8 agent-note injection).
pub const KB_AGENTS_DIR: &str = "agents";

/// Filename peko uses for directory-scoped shared notes.
pub const SHARED_CONTEXT_FILE: &str = "AGENTS.md";

/// Maximum total bytes of MEMORY.md to load. Anything larger is
/// truncated with a notice so a runaway memory file can't blow the
/// context window.
pub const PRINCIPAL_MEMORY_MAX_BYTES: u64 = 256 * 1024; // 256 KiB

/// Maximum total bytes of the kb index (`kb/index.md`) to load
/// (ADR-055 D2 per-section cap — a runaway map cannot starve memory).
pub const KB_INDEX_MAX_BYTES: u64 = 8 * 1024; // 8 KiB

/// Maximum total bytes of a targeted scope note (`kb/groups/<channel>.md`
/// or `kb/agents/<name>.md`, ADR-055 D8).
pub const KB_NOTE_MAX_BYTES: u64 = 8 * 1024; // 8 KiB

/// Maximum total bytes of AGENTS.md to load per directory.
pub const SHARED_CONTEXT_MAX_BYTES: u64 = 64 * 1024; // 64 KiB

/// Maximum total bytes of AGENTS.md content injected as the
/// project-instructions tail section. Matches the 32 KiB cap codex
/// applies to its whole instruction hierarchy.
pub const PROJECT_INSTRUCTIONS_MAX_BYTES: u64 = 32 * 1024; // 32 KiB

/// Load the principal's long-term memory from
/// `<workspace>/kb/MEMORY.md` (ADR-055: memory lives inside the kb).
///
/// Returns `None` if the file does not exist, is empty, or cannot be
/// read. Truncates to `PRINCIPAL_MEMORY_MAX_BYTES` with a notice when
/// oversized.
#[must_use]
pub fn load_principal_memory(workspace: &Path) -> Option<String> {
    let path = workspace.join(KB_DIR).join(PRINCIPAL_MEMORY_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    Some(truncate_with_notice(raw, path, PRINCIPAL_MEMORY_MAX_BYTES))
}

/// Validate a single path segment for the kb scope-note loaders:
/// non-empty, not a dot-segment, no separators. Prevents a channel id
/// or agent name from escaping the kb tree.
fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && !segment.contains(['/', '\\'])
}

/// Load the kb map from `<workspace>/kb/index.md` (ADR-055 D2: the
/// second hot file). Returns `None` if the file does not exist, is
/// empty, or cannot be read. Truncates to [`KB_INDEX_MAX_BYTES`] with
/// a notice when oversized.
#[must_use]
pub fn load_kb_index(workspace: &Path) -> Option<String> {
    let path = workspace.join(KB_DIR).join(KB_INDEX_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    Some(truncate_with_notice(raw, path, KB_INDEX_MAX_BYTES))
}

/// Load the binding note for a run's triggering channel:
/// `<workspace>/kb/groups/<channel>.md` (ADR-055 D8). Channel-bound
/// agents see their room's conventions at turn start; agents with no
/// matching file render nothing. Truncates to [`KB_NOTE_MAX_BYTES`]
/// with a notice when oversized. Returns `None` for unsafe segments
/// or missing/empty/unreadable files.
#[must_use]
pub fn load_binding_note(workspace: &Path, channel: &str) -> Option<String> {
    if !is_safe_segment(channel) {
        return None;
    }
    let path = workspace
        .join(KB_DIR)
        .join(KB_GROUPS_DIR)
        .join(format!("{channel}.md"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    Some(truncate_with_notice(raw, path, KB_NOTE_MAX_BYTES))
}

/// Load the per-agent note for a named agent:
/// `<workspace>/kb/agents/<name>.md` (ADR-055 D8 — the durable
/// per-agent layer ADR-052's T1/T2 lacked). Ephemeral, unnamed spawns
/// get no note. Truncates to [`KB_NOTE_MAX_BYTES`] with a notice when
/// oversized. Returns `None` for unsafe segments or
/// missing/empty/unreadable files.
#[must_use]
pub fn load_agent_note(workspace: &Path, agent_name: &str) -> Option<String> {
    if !is_safe_segment(agent_name) {
        return None;
    }
    let path = workspace
        .join(KB_DIR)
        .join(KB_AGENTS_DIR)
        .join(format!("{agent_name}.md"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    Some(truncate_with_notice(raw, path, KB_NOTE_MAX_BYTES))
}

/// Walk up from `start` looking for `AGENTS.md`. Stops at
/// `principal_workspace_root` (inclusive — we DO check the root
/// itself) so we never escape the principal's authority. Returns the
/// relative label (path from the principal workspace) and the file
/// contents.
///
/// Used by the framework after a tool call lands in a directory.
/// Returns `None` if `start` is not within `principal_workspace_root`,
/// if no `AGENTS.md` is found, or if the file is empty.
#[must_use]
pub fn discover_shared_context(
    start: &Path,
    principal_workspace_root: &Path,
) -> Option<(String, String)> {
    // Refuse to search outside the principal's authority. We compare
    // canonicalized paths so symlinks and `..` components don't trick
    // us into escaping.
    let start_canon = start.canonicalize().ok()?;
    let root_canon = principal_workspace_root.canonicalize().ok()?;
    if !start_canon.starts_with(&root_canon) {
        return None;
    }

    let mut current: PathBuf = start_canon;
    loop {
        let candidate = current.join(SHARED_CONTEXT_FILE);
        if candidate.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&candidate) {
                if !raw.trim().is_empty() {
                    let label = relative_label(&candidate, &root_canon);
                    let content =
                        truncate_with_notice(raw, candidate.clone(), SHARED_CONTEXT_MAX_BYTES);
                    return Some((label, content));
                }
            }
        }

        // We've reached the principal workspace root and didn't find
        // a file above it; stop walking.
        if current == root_canon {
            return None;
        }

        match current.parent() {
            Some(parent) if parent >= root_canon.as_path() => {
                current = parent.to_path_buf();
            }
            _ => return None,
        }
    }
}

/// Extract a directory from a tool-call parameter dict, if a
/// recognisable path-style parameter is present.
///
/// Returns the directory portion of common path-bearing parameters
/// (file_path, path, directory, cwd). Relative paths are resolved
/// against `default_root` (typically the principal's workspace).
#[must_use]
pub fn directory_from_tool_params(
    tool_name: &str,
    params: &serde_json::Value,
    default_root: &Path,
) -> Option<PathBuf> {
    let key = match tool_name {
        "Read" | "Write" | "Edit" => "file_path",
        "Glob" => "directory",
        "Grep" => "path",
        "Bash" => "cwd",
        _ => return None,
    };
    let value = params.get(key)?.as_str()?;
    let raw = PathBuf::from(value);
    let resolved = if raw.is_absolute() {
        raw
    } else {
        default_root.join(raw)
    };
    // If it's a file path (has a filename), strip to its parent.
    let dir = if resolved.is_file() || resolved.extension().is_some() {
        resolved.parent()?.to_path_buf()
    } else {
        resolved
    };
    Some(dir)
}

/// Walk up from `start` looking for the nearest `AGENTS.md`. Stops
/// after checking a directory that contains a `.git` entry (the repo
/// root is checked, but nothing above it), or at the filesystem root.
///
/// Unlike [`discover_shared_context`] there is NO principal-workspace
/// constraint: agents typically work on repos outside the principal
/// workspace, so provenance is communicated via the rendered section's
/// label (the file's own path) and an explicit environment-provided
/// authority note instead of a path cap. Returns the file's path and
/// its contents (non-empty only), truncated at
/// [`PROJECT_INSTRUCTIONS_MAX_BYTES`] with a notice.
#[must_use]
pub fn discover_project_instructions(start: &Path) -> Option<(PathBuf, String)> {
    // Walk the path AS GIVEN — no canonicalize — so the returned label
    // preserves the form the agent actually typed (`/tmp/...` stays
    // `/tmp/...`, not the `/private/tmp/...` macOS resolves it to —
    // found by the tiered-prompt-explore e2e experiment). Symlinks and
    // `..` still work: `is_file` / `read_to_string` follow them, and
    // `parent()` walks upward regardless.
    let mut current: PathBuf = start.to_path_buf();
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }
    loop {
        let candidate = current.join(SHARED_CONTEXT_FILE);
        if candidate.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&candidate) {
                if !raw.trim().is_empty() {
                    let content = truncate_with_notice(
                        raw,
                        candidate.clone(),
                        PROJECT_INSTRUCTIONS_MAX_BYTES,
                    );
                    return Some((candidate, content));
                }
            }
        }

        // Repo boundary: check the directory that owns `.git`, then
        // stop — an AGENTS.md above the repo root does not describe
        // this project.
        if current.join(".git").exists() {
            return None;
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return None,
        }
    }
}

fn truncate_with_notice(raw: String, path: PathBuf, max_bytes: u64) -> String {
    let len = raw.len() as u64;
    if len <= max_bytes {
        return raw;
    }
    let truncated: String = raw.chars().take(max_bytes as usize).collect();
    format!(
        "{truncated}\n\n<!-- truncated: {path:?} was {len} bytes, \
         capped at {max_bytes} bytes by peko-runtime -->\n"
    )
}

fn relative_label(path: &Path, principal_workspace_root: &Path) -> String {
    match path.strip_prefix(principal_workspace_root) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_principal_memory_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_principal_memory(tmp.path()).is_none());
    }

    #[test]
    fn load_principal_memory_returns_contents_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb")).unwrap();
        std::fs::write(tmp.path().join("kb").join("MEMORY.md"), "I prefer tabs.").unwrap();
        let s = load_principal_memory(tmp.path()).unwrap();
        assert_eq!(s, "I prefer tabs.");
    }

    #[test]
    fn load_kb_index_returns_contents_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb")).unwrap();
        std::fs::write(
            tmp.path().join("kb").join("index.md"),
            "- people/ — who I know\n- projects/ — what I build\n",
        )
        .unwrap();
        let s = load_kb_index(tmp.path()).unwrap();
        assert!(s.contains("who I know"));
    }

    #[test]
    fn load_kb_index_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_kb_index(tmp.path()).is_none());
    }

    #[test]
    fn load_kb_index_truncates_oversized_map() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb")).unwrap();
        let big = "x".repeat((KB_INDEX_MAX_BYTES as usize) * 2);
        std::fs::write(tmp.path().join("kb").join("index.md"), big).unwrap();
        let s = load_kb_index(tmp.path()).unwrap();
        assert!(s.len() < KB_INDEX_MAX_BYTES as usize * 2);
        assert!(s.contains("truncated:"));
    }

    #[test]
    fn load_binding_note_reads_matching_channel_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb").join("groups")).unwrap();
        std::fs::write(
            tmp.path().join("kb").join("groups").join("chan_abc.md"),
            "The room's language is German.",
        )
        .unwrap();
        let s = load_binding_note(tmp.path(), "chan_abc").unwrap();
        assert!(s.contains("German"));
    }

    #[test]
    fn load_binding_note_returns_none_when_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb").join("groups")).unwrap();
        assert!(load_binding_note(tmp.path(), "chan_absent").is_none());
    }

    #[test]
    fn load_binding_note_rejects_unsafe_segments() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb").join("groups")).unwrap();
        std::fs::write(tmp.path().join("kb").join("MEMORY.md"), "mem").unwrap();
        for bad in ["", ".", "..", "a/b", "a\\b", "../MEMORY"] {
            assert!(
                load_binding_note(tmp.path(), bad).is_none(),
                "segment: {bad}"
            );
            assert!(load_agent_note(tmp.path(), bad).is_none(), "segment: {bad}");
        }
        // Nothing escaped: no stray file was read or written.
        assert!(!tmp.path().join("groups").exists());
    }

    #[test]
    fn load_agent_note_reads_matching_agent_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb").join("agents")).unwrap();
        std::fs::write(
            tmp.path().join("kb").join("agents").join("channel-comm.md"),
            "You greet new members.",
        )
        .unwrap();
        let s = load_agent_note(tmp.path(), "channel-comm").unwrap();
        assert!(s.contains("greet new members"));
    }

    #[test]
    fn load_agent_note_returns_none_when_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb").join("agents")).unwrap();
        assert!(load_agent_note(tmp.path(), "unnamed").is_none());
    }

    #[test]
    fn load_agent_note_truncates_oversized_note() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("kb").join("agents")).unwrap();
        let big = "y".repeat((KB_NOTE_MAX_BYTES as usize) * 2);
        std::fs::write(tmp.path().join("kb").join("agents").join("big.md"), big).unwrap();
        let s = load_agent_note(tmp.path(), "big").unwrap();
        assert!(s.len() < KB_NOTE_MAX_BYTES as usize * 2);
        assert!(s.contains("truncated:"));
    }

    #[test]
    fn discover_shared_context_finds_file_in_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("AGENTS.md"), "Use make.sh").unwrap();

        let (label, content) = discover_shared_context(&project, tmp.path()).unwrap();
        assert!(content.contains("Use make.sh"));
        assert!(
            label.contains("AGENTS.md"),
            "label should include AGENTS.md, got: {label}"
        );
    }

    #[test]
    fn discover_shared_context_walks_up_to_find_file() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let nested = project.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(project.join("AGENTS.md"), "Don't push to main.").unwrap();

        let (label, content) = discover_shared_context(&nested, tmp.path()).unwrap();
        assert!(content.contains("Don't push to main."));
        assert!(label.contains("project"));
    }

    #[test]
    fn discover_shared_context_does_not_escape_principal_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        // AGENTS.md exists OUTSIDE the principal workspace
        std::fs::write(tmp.path().join("AGENTS.md"), "outside content").unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        // sub is the principal workspace root; AGENTS.md is at the
        // principal workspace root, which is allowed. To verify the
        // cap, we use a path that goes ABOVE the principal workspace.
        let principal_root = sub.clone();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("AGENTS.md"), "should not load").unwrap();

        // Querying from `outside` should not find the file at
        // `principal_root`'s parent because we cap at the principal
        // workspace root.
        let result = discover_shared_context(&outside, &principal_root);
        assert!(
            result.is_none(),
            "discovery should not escape principal workspace: {result:?}"
        );
    }

    #[test]
    fn discover_shared_context_returns_none_when_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(discover_shared_context(&sub, tmp.path()).is_none());
    }

    #[test]
    fn discover_project_instructions_finds_nearest_from_nested_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let nested = project.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(project.join("AGENTS.md"), "repo rules").unwrap();

        let (path, content) = discover_project_instructions(&nested).unwrap();
        assert_eq!(content, "repo rules");
        assert!(path.ends_with("AGENTS.md"), "path was: {path:?}");
        assert!(path.parent().unwrap().ends_with("project"));
    }

    #[test]
    fn discover_project_instructions_prefers_nearest_over_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(outer.join("AGENTS.md"), "outer rules").unwrap();
        std::fs::write(inner.join("AGENTS.md"), "inner rules").unwrap();

        let (path, content) = discover_project_instructions(&inner).unwrap();
        assert_eq!(content, "inner rules");
        assert!(path.parent().unwrap().ends_with("inner"));
    }

    #[test]
    fn discover_project_instructions_does_not_cross_git_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        // AGENTS.md ABOVE the repo root must not be found.
        std::fs::write(tmp.path().join("AGENTS.md"), "above the repo").unwrap();
        let repo = tmp.path().join("repo");
        let nested = repo.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();

        assert!(
            discover_project_instructions(&nested).is_none(),
            "discovery must stop at the .git boundary"
        );
    }

    #[test]
    fn discover_project_instructions_checks_git_root_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        std::fs::write(repo.join("AGENTS.md"), "repo rules").unwrap();

        let (_, content) = discover_project_instructions(&repo).unwrap();
        assert_eq!(content, "repo rules");
    }

    #[test]
    fn discover_project_instructions_ignores_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("AGENTS.md"), "  \n  ").unwrap();
        std::fs::create_dir(project.join(".git")).unwrap();

        assert!(
            discover_project_instructions(&project).is_none(),
            "whitespace-only AGENTS.md must be ignored"
        );
    }

    #[test]
    fn discover_project_instructions_truncates_at_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let big = "x".repeat((PROJECT_INSTRUCTIONS_MAX_BYTES + 4096) as usize);
        std::fs::write(project.join("AGENTS.md"), big).unwrap();

        let (_, content) = discover_project_instructions(&project).unwrap();
        assert!(
            content.len() < (PROJECT_INSTRUCTIONS_MAX_BYTES + 4096) as usize,
            "content was not capped: {} bytes",
            content.len()
        );
        assert!(
            content.contains("truncated:"),
            "truncation notice missing: {}",
            &content[content.len() - 200..]
        );
    }

    #[test]
    fn directory_from_tool_params_resolves_relative_paths() {
        let root = PathBuf::from("/workspaces/agent/personal");
        let dir = directory_from_tool_params(
            "Read",
            &serde_json::json!({"file_path": "src/main.rs"}),
            &root,
        )
        .unwrap();
        assert_eq!(dir, root.join("src"));
    }

    #[test]
    fn directory_from_tool_params_keeps_absolute_paths() {
        let root = PathBuf::from("/workspaces/agent/personal");
        let dir =
            directory_from_tool_params("Bash", &serde_json::json!({"cwd": "/tmp/build"}), &root)
                .unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/build"));
    }

    #[test]
    fn directory_from_tool_params_returns_none_for_unknown_tool() {
        let root = PathBuf::from("/workspaces/agent/personal");
        let dir = directory_from_tool_params("AsyncList", &serde_json::json!({}), &root);
        assert!(dir.is_none());
    }
}
