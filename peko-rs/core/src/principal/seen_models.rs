//! `seen_models.json` — first-use-per-model tracking for the
//! audit-warning UX.
//!
//! Phase 4 of `feature/multi-model-subagents` (plan:
//! `/Users/rlsn/.claude/plans/goofy-humming-wall.md`). Persists a
//! per-principal set of model ids the principal has already
//! called. Powers `mark_model_seen(model_id) -> bool` which
//! returns `true` on first use so the caller can pick
//! `Warning` severity for the audit row instead of `Info`.
//!
//! ## Atomic-write pattern
//!
//! Mirrors `peko-rs/core/src/daemon/config_drift.rs:17-21, 274-298`:
//!
//! 1. Serialize the in-memory set to pretty JSON.
//! 2. Write to `<path>.json.tmp`.
//! 3. `sync_all()` to flush.
//! 4. `rename` `.tmp` over the real file — POSIX-atomic.
//!
//! A crash before step 4 leaves the previous good copy intact; a
//! crash after leaves the new file in place. The loader silently
//! discards an orphaned `.tmp` (same as
//! `peko-rs/quota/src/state.rs:80-93`).
//!
//! ## On-disk shape
//!
//! ```json
//! {
//!   "version": 1,
//!   "models": ["claude-3-5-sonnet-...", "claude-haiku-..."]
//! }
//! ```
//!
//! `version` is reserved for future migrations; we read it
//! tolerantly (default to `1` when absent) so an empty / older
//! file loads cleanly.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SeenModels {
    /// Schema version. Reserved for migrations; readers default
    /// to `1` when absent so an empty / legacy file still loads.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Model ids the principal has called. `BTreeSet` so the
    /// serialized form is stable (sorted) — easier to diff in
    /// `peko audit tail` follow-ups and stable across restarts.
    #[serde(default)]
    pub models: BTreeSet<String>,
}

fn default_version() -> u32 {
    1
}

impl SeenModels {
    /// Empty set — used when no `seen_models.json` exists on
    /// first principal boot.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: 1,
            models: BTreeSet::new(),
        }
    }

    /// Load from disk. Returns `Ok(SeenModels::empty())` when
    /// the file is missing (fresh principal). Errors only on
    /// parse / I/O failures; the atomic-rename persistence
    /// means we never see a partial write under the real path.
    /// Orphaned `.tmp` siblings are silently discarded.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let seen: Self = serde_json::from_slice(&bytes).with_context(|| {
                    format!("parse seen_models JSON at {}", path.display())
                })?;
                Ok(seen)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(anyhow::Error::from(e).context(format!(
                "read seen_models.json at {}",
                path.display()
            ))),
        }
    }

    /// Atomic save: serialize → `.tmp` → fsync → rename over
    /// the real file. Creates parent dirs if missing.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create parent dir {}", parent.display())
            })?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).context("serialize seen_models")?;
        {
            let mut file = std::fs::File::create(&tmp)
                .with_context(|| format!("create {}", tmp.display()))?;
            file.write_all(&json)
                .with_context(|| format!("write {}", tmp.display()))?;
            file.sync_all()
                .with_context(|| format!("fsync {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, path).with_context(|| {
            format!("rename {} → {}", tmp.display(), path.display())
        })?;
        Ok(())
    }

    /// `true` if the model id is already in the set, `false`
    /// otherwise. Pure — does not mutate. Pair with
    /// [`Self::add_then_save`] (or `mark_seen_inplace`) when
    /// the caller wants to record the first use.
    #[must_use]
    pub fn contains(&self, model_id: &str) -> bool {
        self.models.contains(model_id)
    }

    /// Insert + persist. Returns `true` if this was a fresh
    /// insertion (first use), `false` if already present. The
    /// caller uses the boolean to pick `Warning` vs `Info`
    /// severity in the audit row.
    pub fn add_then_save(&mut self, model_id: &str, path: &Path) -> Result<bool> {
        let fresh = self.models.insert(model_id.to_string());
        if fresh {
            self.save(path)?;
        }
        Ok(fresh)
    }
}

/// Resolve the canonical `seen_models.json` path for a
/// principal. Pattern: `<workspace_path>/seen_models.json`. The
/// principal's `workspace_path` is `{config_dir}/principals/<name>`
/// (see `PrincipalContext::workspace_path`).
#[must_use]
pub fn seen_models_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join("seen_models.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_returns_empty_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let path = seen_models_path(dir.path());
        let seen = SeenModels::load(&path).unwrap();
        assert!(seen.models.is_empty());
        assert_eq!(seen.version, 1);
    }

    #[test]
    fn save_then_load_round_trips_models() {
        let dir = TempDir::new().unwrap();
        let path = seen_models_path(dir.path());
        let mut seen = SeenModels::empty();
        seen.models.insert("claude-sonnet-4-6".to_string());
        seen.models.insert("claude-haiku-4-5".to_string());
        seen.save(&path).unwrap();

        let loaded = SeenModels::load(&path).unwrap();
        assert_eq!(loaded, seen);
    }

    #[test]
    fn add_then_save_records_first_use_and_persists() {
        let dir = TempDir::new().unwrap();
        let path = seen_models_path(dir.path());
        let mut seen = SeenModels::empty();
        // First call: fresh → persists, returns `true`.
        assert!(seen.add_then_save("claude-sonnet-4-6", &path).unwrap());
        // Second call on the same id: already present → no save,
        // returns `false`.
        assert!(!seen.add_then_save("claude-sonnet-4-6", &path).unwrap());
        // A different model: fresh → persists.
        assert!(seen.add_then_save("claude-haiku-4-5", &path).unwrap());
        let loaded = SeenModels::load(&path).unwrap();
        assert_eq!(loaded.models.len(), 2);
        assert!(loaded.contains("claude-sonnet-4-6"));
        assert!(loaded.contains("claude-haiku-4-5"));
    }

    #[test]
    fn add_then_save_no_op_for_duplicate_does_not_rewrite() {
        // The "no save on duplicate" optimization means a
        // hot-path caller (every LLM call) doesn't trigger a
        // disk write after the first time. Verify by setting
        // mtime on the existing file and checking the mtime
        // doesn't change on a duplicate add.
        let dir = TempDir::new().unwrap();
        let path = seen_models_path(dir.path());
        let mut seen = SeenModels::empty();
        seen.add_then_save("claude-sonnet-4-6", &path).unwrap();
        let metadata_before = std::fs::metadata(&path).unwrap();
        let mtime_before = metadata_before.modified().unwrap();
        // Sleep just long enough that mtime ticks (1s on most
        // filesystems). 1.2s leaves headroom for slow CI.
        std::thread::sleep(std::time::Duration::from_millis(1200));
        seen.add_then_save("claude-sonnet-4-6", &path).unwrap();
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "duplicate add must not rewrite the file"
        );
    }

    #[test]
    fn load_tolerates_legacy_file_without_version() {
        // A pre-Phase-4 file might lack the `version` field.
        // `#[serde(default)]` on `version` keeps it loadable.
        let dir = TempDir::new().unwrap();
        let path = seen_models_path(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            &path,
            r#"{ "models": ["claude-sonnet-4-6"] }"#,
        )
        .unwrap();
        let loaded = SeenModels::load(&path).unwrap();
        assert_eq!(loaded.version, 1);
        assert!(loaded.contains("claude-sonnet-4-6"));
    }

    #[test]
    fn save_creates_parent_directories() {
        // `workspace_path` may not exist yet on a fresh
        // principal; `save` should `mkdir -p` the parent.
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested").join("deeper");
        let path = seen_models_path(&nested);
        let mut seen = SeenModels::empty();
        seen.models.insert("claude-sonnet-4-6".to_string());
        seen.save(&path).unwrap();
        assert!(path.exists());
    }
}