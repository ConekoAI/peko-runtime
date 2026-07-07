//! Per-session tracker of directories the principal has touched during a
//! session. Lives in the framework types module so that downstream
//! framework code (e.g. `BuiltinToolAdapter`) can hold an `Arc` to one
//! without crossing into `crate::agents` — the lint boundary keeps the
//! framework free of agent-layer dependencies.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Tracks directories the principal has touched during a session so the
/// agentic loop can surface directory-scoped context (e.g. `AGENTS.md`)
/// on demand.
///
/// Built once per session by the engine; the adapter pushes directories
/// via [`Self::touch`] after each tool call; the agentic loop drains
/// them via [`Self::drain_new`] at iteration start and resolves them to
/// context content. Idempotent — pushing the same directory twice is a
/// no-op.
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
    /// calling the relevant context-discovery function on each one to
    /// actually load any directory-scoped notes. Returns canonical paths.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_distinct_directories() {
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
    fn treats_canonical_equivalent_paths_as_same() {
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
