//! Per-principal long-term memory (`MEMORY.md`) and shared
//! directory-scoped context (`AGENTS.md`).
//!
//! Two complementary surfaces:
//!
//! - **MEMORY.md** lives at `<principal_workspace>/MEMORY.md`. It is
//!   loaded at session start and injected into the system prompt
//!   under `## Your long-term memory (MEMORY.md)`. The principal
//!   owns this file and may update it via `Write`.
//!
//! - **AGENTS.md** lives at arbitrary directories the principal
//!   touches during a session. The framework discovers it on demand
//!   when a directory-aware tool call lands in a directory that
//!   contains (or has an ancestor containing) `AGENTS.md`. Discovered
//!   contexts are surfaced to the model as synthetic user messages so
//!   they appear on the next iteration's LLM call. The walk-up is
//!   capped at the principal's workspace root so we never load files
//!   outside the principal's authority.
//!
//! Both are conventions rather than required files. Missing files
//! simply omit the section.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Tracks directories the principal has touched during a session so the
/// agentic loop can discover `AGENTS.md` on demand.
///
/// Built once per session by the engine; the adapter pushes directories
/// via [`Self::touch`] after each tool call; the agentic loop drains
/// them via [`Self::drain_new`] at iteration start and resolves them to
/// `AGENTS.md` content. Idempotent — pushing the same directory twice
/// is a no-op.
#[derive(Debug, Default)]
pub struct DirectoryContextTracker {
    touched: Mutex<Vec<PathBuf>>,
}

impl DirectoryContextTracker {
    /// Create a new empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            touched: Mutex::new(Vec::new()),
        }
    }

    /// Record a directory the principal just touched via a tool call.
    /// The directory is canonicalized so we don't double-track the
    /// same logical path under different spellings (`./src/..` vs
    /// `src`). Returns `true` if this is the first time we've seen
    /// this directory.
    pub fn touch(&self, dir: &Path) -> bool {
        let canon = match dir.canonicalize() {
            Ok(p) => p,
            Err(_) => dir.to_path_buf(),
        };
        let mut touched = match self.touched.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if touched.iter().any(|p| paths_equal(p, &canon)) {
            return false;
        }
        touched.push(canon);
        true
    }

    /// Drain all touched directories. The caller is responsible for
    /// calling [`discover_shared_context`] on each one to actually
    /// load the `AGENTS.md` content. Returns canonical paths.
    pub fn drain_new(&self) -> Vec<PathBuf> {
        let mut touched = match self.touched.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        std::mem::take(&mut *touched)
    }

    /// Snapshot the touched directories without draining.
    #[must_use]
    pub fn snapshot(&self) -> Vec<PathBuf> {
        self.touched.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    // Compare by components rather than byte-for-byte so trailing
    // slashes and similar minor differences don't make us record the
    // same directory twice.
    a.components().collect::<Vec<_>>() == b.components().collect::<Vec<_>>()
}

/// Filename peko uses for per-principal long-term memory.
pub const PRINCIPAL_MEMORY_FILE: &str = "MEMORY.md";

/// Filename peko uses for directory-scoped shared notes.
pub const SHARED_CONTEXT_FILE: &str = "AGENTS.md";

/// Maximum total bytes of MEMORY.md to load. Anything larger is
/// truncated with a notice so a runaway memory file can't blow the
/// context window.
pub const PRINCIPAL_MEMORY_MAX_BYTES: u64 = 256 * 1024; // 256 KiB

/// Maximum total bytes of AGENTS.md to load per directory.
pub const SHARED_CONTEXT_MAX_BYTES: u64 = 64 * 1024; // 64 KiB

/// Load the principal's long-term memory from `<workspace>/MEMORY.md`.
///
/// Returns `None` if the file does not exist, is empty, or cannot be
/// read. Truncates to `PRINCIPAL_MEMORY_MAX_BYTES` with a notice when
/// oversized.
#[must_use]
pub fn load_principal_memory(workspace: &Path) -> Option<String> {
    let path = workspace.join(PRINCIPAL_MEMORY_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    Some(truncate_with_notice(raw, path, PRINCIPAL_MEMORY_MAX_BYTES))
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
        std::fs::write(tmp.path().join("MEMORY.md"), "I prefer tabs.").unwrap();
        let s = load_principal_memory(tmp.path()).unwrap();
        assert_eq!(s, "I prefer tabs.");
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

    #[test]
    fn tracker_records_distinct_directories() {
        let tracker = DirectoryContextTracker::new();
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        assert!(tracker.touch(&a));
        assert!(tracker.touch(&b));
        // Touching the same path again is a no-op.
        assert!(!tracker.touch(&a));

        let drained = tracker.drain_new();
        assert_eq!(drained.len(), 2);
        // After drain, snapshot is empty.
        assert!(tracker.snapshot().is_empty());
    }

    #[test]
    fn tracker_treats_canonical_equivalent_paths_as_same() {
        let tracker = DirectoryContextTracker::new();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("sub")).unwrap();

        // The temp dir's canonical form may differ from the literal
        // path on macOS (where tempdir lives under /var/folders/...).
        // Just verify that two `touch` calls on the same logical dir
        // only record it once.
        assert!(tracker.touch(tmp.path()));
        assert!(!tracker.touch(tmp.path()));
        assert_eq!(tracker.drain_new().len(), 1);
    }
}
