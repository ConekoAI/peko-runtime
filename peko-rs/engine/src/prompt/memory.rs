//! The principal's hot knowledge-base set and directory-scoped project
//! instructions (`AGENTS.md`).
//!
//! - **Hot kb set** (ADR-055 D2/D8): `kb/MEMORY.md`, `kb/CONVENTIONS.md`,
//!   `kb/index.md`, plus the two targeted scope notes
//!   (`kb/groups/<channel>.md`, `kb/roles/<name>.md`). [`HotKb::load`]
//!   reads them under one shared byte budget and the renderer injects
//!   them as `<runtime-context>` tail sections, on change.
//! - **Project instructions** (ADR-052 D5): the nearest `AGENTS.md`
//!   above the run's focus directory, via
//!   [`discover_project_instructions`].
//!
//! Both are conventions rather than required files: a missing or blank
//! file renders absent.

use std::path::{Path, PathBuf};

/// Directory (relative to the principal workspace) holding the
/// principal's persistent knowledge base (ADR-055). The hot set
/// (`MEMORY.md`, `index.md`, `CONVENTIONS.md`) lives here, as do the
/// cold conventions (`people/`, `groups/`, `roles/`) that feed the
/// targeted scope notes.
pub const KB_DIR: &str = "kb";

/// Filename peko uses for per-principal long-term memory. Resolved
/// under [`KB_DIR`] — `<principal_workspace>/kb/MEMORY.md`.
pub const PRINCIPAL_MEMORY_FILE: &str = "MEMORY.md";

/// Filename peko uses for the shared behavioral rulebook rendered to
/// every agent in the principal — `<principal_workspace>/kb/CONVENTIONS.md`.
pub const PRINCIPAL_CONVENTIONS_FILE: &str = "CONVENTIONS.md";

/// Filename peko uses for the kb map (ADR-055 D2: the second hot
/// file). Resolved under [`KB_DIR`] —
/// `<principal_workspace>/kb/index.md`.
pub const KB_INDEX_FILE: &str = "index.md";

/// Subdirectory (under [`KB_DIR`]) of per-group notes keyed by
/// channel/group id (ADR-055 D8 binding-note injection).
pub const KB_GROUPS_DIR: &str = "groups";

/// Subdirectory (under [`KB_DIR`]) of per-role notes keyed by role
/// name (ADR-055 D8 role-note injection). Pairs with the role file
/// `roles/<name>/ROLE.md` (ADR-064).
pub const KB_ROLES_DIR: &str = "roles";

/// Filename peko uses for directory-scoped project instructions.
pub const SHARED_CONTEXT_FILE: &str = "AGENTS.md";

/// Shared byte budget for the whole hot kb set (ADR-055 D2): every
/// agent pays for these bytes on the turn they (re)enter the tail, so
/// the set is capped as one unit, codex-style. Truncation notices count
/// against the budget.
pub const HOT_KB_BUDGET_BYTES: usize = 32 * 1024;

/// Per-file cap for each non-memory member of the hot set
/// (`CONVENTIONS.md`, `index.md`, the binding note, the role note).
/// `MEMORY.md` gets whatever these leave of [`HOT_KB_BUDGET_BYTES`], so
/// a runaway map or note can never starve memory below
/// [`HOT_KB_MEMORY_FLOOR_BYTES`].
pub const HOT_KB_SECTION_MAX_BYTES: usize = 4 * 1024;

/// The least of the budget `MEMORY.md` can be left with: the budget
/// minus four fully-used [`HOT_KB_SECTION_MAX_BYTES`] sections.
pub const HOT_KB_MEMORY_FLOOR_BYTES: usize = HOT_KB_BUDGET_BYTES - 4 * HOT_KB_SECTION_MAX_BYTES;

/// Maximum bytes of `AGENTS.md` content injected as the
/// project-instructions tail section. Matches the 32 KiB cap codex
/// applies to its whole instruction hierarchy.
pub const PROJECT_INSTRUCTIONS_MAX_BYTES: usize = 32 * 1024;

/// The principal's hot kb set for one run, loaded under
/// [`HOT_KB_BUDGET_BYTES`]. Each member is `None` when its file is
/// missing, blank, or unreadable (absence renders absent).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotKb {
    /// `kb/MEMORY.md` — curated long-term memory.
    pub memory: Option<String>,
    /// `kb/CONVENTIONS.md` — the shared behavioral rulebook.
    pub conventions: Option<String>,
    /// `kb/index.md` — the map of the kb tree.
    pub index: Option<String>,
    /// `kb/groups/<channel>.md` — the run's binding note.
    pub binding_note: Option<String>,
    /// `kb/roles/<role_name>.md` — the named role's standing note.
    pub role_note: Option<String>,
}

impl HotKb {
    /// Load the hot set for a run in `workspace`, bound to `channel`
    /// and named `role_name`.
    ///
    /// The four small members load first, each capped at
    /// [`HOT_KB_SECTION_MAX_BYTES`]; `MEMORY.md` then gets the rest of
    /// [`HOT_KB_BUDGET_BYTES`]. An oversized file is cut at a line
    /// boundary and ends with a notice telling the agent to curate it.
    /// Channel and role names that are not a single safe path segment
    /// load no note.
    #[must_use]
    pub fn load(workspace: &Path, channel: &str, role_name: &str) -> Self {
        let kb = workspace.join(KB_DIR);
        let note = |dir: &str, name: &str| {
            is_safe_segment(name)
                .then(|| {
                    read_capped(
                        &kb.join(dir).join(format!("{name}.md")),
                        HOT_KB_SECTION_MAX_BYTES,
                    )
                })
                .flatten()
        };
        let conventions = read_capped(
            &kb.join(PRINCIPAL_CONVENTIONS_FILE),
            HOT_KB_SECTION_MAX_BYTES,
        );
        let index = read_capped(&kb.join(KB_INDEX_FILE), HOT_KB_SECTION_MAX_BYTES);
        let binding_note = note(KB_GROUPS_DIR, channel);
        let role_note = note(KB_ROLES_DIR, role_name);

        let used: usize = [&conventions, &index, &binding_note, &role_note]
            .into_iter()
            .flatten()
            .map(String::len)
            .sum();
        let memory = read_capped(
            &kb.join(PRINCIPAL_MEMORY_FILE),
            HOT_KB_BUDGET_BYTES.saturating_sub(used),
        );

        Self {
            memory,
            conventions,
            index,
            binding_note,
            role_note,
        }
    }

    /// Total bytes of the loaded hot set (never above
    /// [`HOT_KB_BUDGET_BYTES`]).
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        [
            &self.memory,
            &self.conventions,
            &self.index,
            &self.binding_note,
            &self.role_note,
        ]
        .into_iter()
        .flatten()
        .map(String::len)
        .sum()
    }
}

/// Validate a single path segment for the kb scope-note loaders:
/// non-empty, not a dot-segment, no separators. Prevents a channel id
/// or role name from escaping the kb tree.
fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && !segment.contains(['/', '\\'])
}

/// Read `path` and cap it at `max_bytes` (notice included). `None` for
/// a missing, unreadable, or whitespace-only file.
fn read_capped(path: &Path, max_bytes: usize) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(truncate_with_notice(raw, path, max_bytes))
}

/// Walk up from `start` looking for the nearest `AGENTS.md`. Stops
/// after checking a directory that contains a `.git` entry (the repo
/// root is checked, but nothing above it), or at the filesystem root.
///
/// There is NO principal-workspace constraint: agents typically work
/// on repos outside the principal workspace, so provenance is
/// communicated via the rendered section's label (the file's own path)
/// and an explicit environment-provided authority note instead of a
/// path cap. Returns the file's path and its contents (non-empty
/// only), truncated at [`PROJECT_INSTRUCTIONS_MAX_BYTES`] with a
/// notice.
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
            if let Some(content) = read_capped(&candidate, PROJECT_INSTRUCTIONS_MAX_BYTES) {
                return Some((candidate, content));
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

/// Cap `raw` at `max_bytes` UTF-8 bytes, notice included. The kept
/// text ends at the last line break that fits (or at a char boundary
/// when a single line is longer than the room left).
fn truncate_with_notice(raw: String, path: &Path, max_bytes: usize) -> String {
    let len = raw.len();
    if len <= max_bytes {
        return raw;
    }
    let notice = format!(
        "\n\n<!-- truncated: {} is {len} bytes and only the part above fits \
         its prompt budget; curate the file — the rest is not in your prompt -->\n",
        path.display()
    );
    let room = max_bytes.saturating_sub(notice.len());
    let mut end = room.min(len);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(newline) = raw[..end].rfind('\n') {
        end = newline;
    }
    format!("{}{notice}", &raw[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (relative, body) in files {
            let path = tmp.path().join(KB_DIR).join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        tmp
    }

    #[test]
    fn hot_kb_is_empty_without_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            HotKb::load(tmp.path(), "chan_a", "worker"),
            HotKb::default()
        );
    }

    #[test]
    fn hot_kb_loads_every_member() {
        let tmp = kb(&[
            ("MEMORY.md", "I prefer tabs."),
            ("CONVENTIONS.md", "Journal daily."),
            ("index.md", "- people/ — who I know"),
            ("groups/chan_abc.md", "The room's language is German."),
            ("roles/channel-comm.md", "You greet new members."),
        ]);
        let hot = HotKb::load(tmp.path(), "chan_abc", "channel-comm");
        assert_eq!(hot.memory.as_deref(), Some("I prefer tabs."));
        assert_eq!(hot.conventions.as_deref(), Some("Journal daily."));
        assert!(hot.index.unwrap().contains("who I know"));
        assert!(hot.binding_note.unwrap().contains("German"));
        assert!(hot.role_note.unwrap().contains("greet new members"));
    }

    #[test]
    fn hot_kb_skips_blank_files_and_unmatched_notes() {
        let tmp = kb(&[("MEMORY.md", "  \n"), ("groups/chan_other.md", "x")]);
        let hot = HotKb::load(tmp.path(), "chan_absent", "unnamed");
        assert_eq!(hot, HotKb::default());
    }

    #[test]
    fn hot_kb_rejects_unsafe_note_segments() {
        let tmp = kb(&[("MEMORY.md", "mem"), ("groups/x.md", "note")]);
        for bad in ["", ".", "..", "a/b", "a\\b", "../MEMORY"] {
            let hot = HotKb::load(tmp.path(), bad, bad);
            assert!(hot.binding_note.is_none(), "segment: {bad}");
            assert!(hot.role_note.is_none(), "segment: {bad}");
        }
    }

    #[test]
    fn hot_kb_caps_small_members_per_section() {
        let big = "y\n".repeat(HOT_KB_SECTION_MAX_BYTES);
        let tmp = kb(&[
            ("CONVENTIONS.md", &big),
            ("index.md", &big),
            ("groups/chan_a.md", &big),
            ("roles/worker.md", &big),
        ]);
        let hot = HotKb::load(tmp.path(), "chan_a", "worker");
        for member in [
            &hot.conventions,
            &hot.index,
            &hot.binding_note,
            &hot.role_note,
        ] {
            let text = member.as_deref().unwrap();
            assert!(
                text.len() <= HOT_KB_SECTION_MAX_BYTES,
                "{} bytes",
                text.len()
            );
            assert!(text.contains("truncated:"));
        }
    }

    #[test]
    fn memory_gets_the_whole_budget_when_alone() {
        let big = "m\n".repeat(HOT_KB_BUDGET_BYTES);
        let tmp = kb(&[("MEMORY.md", &big)]);
        let memory = HotKb::load(tmp.path(), "cli", "root").memory.unwrap();
        assert!(memory.len() <= HOT_KB_BUDGET_BYTES);
        assert!(
            memory.len() > HOT_KB_BUDGET_BYTES - 512,
            "{} bytes",
            memory.len()
        );
        assert!(memory.contains("truncated:"));
    }

    #[test]
    fn memory_keeps_its_floor_when_every_other_member_is_full() {
        let big = "z\n".repeat(HOT_KB_BUDGET_BYTES);
        let tmp = kb(&[
            ("MEMORY.md", &big),
            ("CONVENTIONS.md", &big),
            ("index.md", &big),
            ("groups/chan_a.md", &big),
            ("roles/worker.md", &big),
        ]);
        let hot = HotKb::load(tmp.path(), "chan_a", "worker");
        assert!(
            hot.total_bytes() <= HOT_KB_BUDGET_BYTES,
            "{}",
            hot.total_bytes()
        );
        assert!(hot.memory.unwrap().len() >= HOT_KB_MEMORY_FLOOR_BYTES - 512);
    }

    #[test]
    fn small_hot_set_is_untouched() {
        let tmp = kb(&[("MEMORY.md", "a\nb\n"), ("index.md", "map")]);
        let hot = HotKb::load(tmp.path(), "cli", "root");
        assert_eq!(hot.memory.as_deref(), Some("a\nb\n"));
        assert_eq!(hot.total_bytes(), 7);
    }

    #[test]
    fn truncation_counts_bytes_not_chars() {
        // 3-byte chars: a char-count cap would keep three times the
        // byte budget.
        let raw = "記".repeat(HOT_KB_SECTION_MAX_BYTES);
        let out = truncate_with_notice(raw, Path::new("kb/index.md"), HOT_KB_SECTION_MAX_BYTES);
        assert!(out.len() <= HOT_KB_SECTION_MAX_BYTES, "{} bytes", out.len());
        assert!(out.starts_with('記'));
    }

    #[test]
    fn truncation_cuts_at_a_line_boundary() {
        let raw = format!("{}\n{}", "a".repeat(100), "b".repeat(400));
        let out = truncate_with_notice(raw, Path::new("kb/MEMORY.md"), 400);
        assert!(out.starts_with(&"a".repeat(100)));
        assert!(!out.contains("bb"), "kept a partial line: {out}");
        assert!(out.contains("truncated: kb/MEMORY.md is 501 bytes"));
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
        let big = "x".repeat(PROJECT_INSTRUCTIONS_MAX_BYTES + 4096);
        std::fs::write(project.join("AGENTS.md"), big).unwrap();

        let (_, content) = discover_project_instructions(&project).unwrap();
        assert!(
            content.len() < PROJECT_INSTRUCTIONS_MAX_BYTES + 4096,
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
        let dir = directory_from_tool_params("Async", &serde_json::json!({"action":"list"}), &root);
        assert!(dir.is_none());
    }
}
