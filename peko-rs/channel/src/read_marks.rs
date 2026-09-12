//! Per-channel session read-marks (runtime-tier persistence).
//!
//! One `read_marks.json` per channel, located at
//! `<runtime_dir>/channels/<channel_id>/read_marks.json`. Tracks, for
//! each *session* (keyed by session-id string), the highest `TaskId`
//! (global log line number) that session has *observed* — either
//! because the `ChannelDigestSessionContextHandler` rendered (or
//! filtered) everything up to it, or because the session's
//! `ChannelRead` returned it. The session-context digest renders
//! "N new messages" from this position.
//!
//! Deliberately a SIBLING of `cursors.json`, not a reuse: the
//! subscriber cursor is per-principal and advances on every event the
//! subscription loop observes (delivery-oriented), so it can never
//! represent "what has this session actually been shown".
//!
//! ## Atomic-write convention
//!
//! Mirror of `peko-rs/plan/src/storage.rs:309-321` (same convention
//! `cursors.rs` already follows) — write to a pid-suffixed tmp file,
//! fsync, rename over the destination.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::port::{ChannelError, Result, TaskId};

// ---------------------------------------------------------------------------
// Read-marks file shape
// ---------------------------------------------------------------------------

/// Per-session read-position map. The outer map's key is the session
/// id string (a `peko_session::id::SessionId` wire form); the value is
/// the highest `TaskId` (global line number across the channel's
/// stitched log) the session has observed. Serialized as flat
/// `HashMap<String, String>` — the same shape `cursors.json` uses.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChannelReadMarks(pub HashMap<String, TaskId>);

impl ChannelReadMarks {
    /// Construct an empty read-mark map.
    pub fn new() -> Self {
        Self(Default::default())
    }

    /// Get the read mark for `session_key` (None if the session has
    /// never observed the channel — first observation).
    pub fn get(&self, session_key: &str) -> Option<&TaskId> {
        self.0.get(session_key)
    }

    /// Advance the mark for `session_key` to `task_id`. Monotonicity
    /// is enforced by the caller (`ChannelStore::advance_read_mark`),
    /// which refuses to rewind numeric line numbers.
    pub fn set(&mut self, session_key: impl Into<String>, task_id: TaskId) {
        self.0.insert(session_key.into(), task_id);
    }

    // -----------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------

    /// Standard location of the read-marks file inside a channel directory.
    pub fn path_in(channel_dir: &Path) -> PathBuf {
        channel_dir.join("read_marks.json")
    }

    /// Load marks from `<channel_dir>/read_marks.json`. Returns an
    /// empty map if the file doesn't exist (fresh channel, never
    /// digested or read).
    pub async fn load(channel_dir: &Path) -> Result<Self> {
        let path = Self::path_in(channel_dir);
        match fs::read(&path).await {
            Ok(bytes) => {
                let marks: Self = serde_json::from_slice(&bytes)
                    .map_err(|e| ChannelError::Cursor(format!("decode {}: {e}", path.display())))?;
                Ok(marks)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(ChannelError::Cursor(format!("read {}: {e}", path.display()))),
        }
    }

    /// Persist marks atomically. Write to a pid-suffixed tmp file in
    /// the same directory, fsync, rename over the destination.
    ///
    /// **Convention mirror:** `peko-rs/plan/src/storage.rs:309-321`.
    pub async fn save(&self, channel_dir: &Path) -> Result<()> {
        let path = Self::path_in(channel_dir);
        fs::create_dir_all(channel_dir).await?;

        let pid = std::process::id();
        let tmp = channel_dir.join(format!(".read_marks.json.{pid}.tmp"));

        let bytes = serde_json::to_vec_pretty(self)?;
        {
            let mut f = fs::File::create(&tmp).await.map_err(|e| {
                ChannelError::Cursor(format!("create {}: {e}", tmp.display()))
            })?;
            f.write_all(&bytes).await.map_err(|e| {
                ChannelError::Cursor(format!("write {}: {e}", tmp.display()))
            })?;
            f.sync_all().await.map_err(|e| {
                ChannelError::Cursor(format!("fsync {}: {e}", tmp.display()))
            })?;
        }
        fs::rename(&tmp, &path).await.map_err(|e| {
            ChannelError::Cursor(format!("rename {} -> {}: {e}", tmp.display(), path.display()))
        })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn get_set_round_trip_in_memory() {
        let mut m = ChannelReadMarks::new();
        assert_eq!(m.get("sess-a"), None);
        m.set("sess-a", "12".to_string());
        m.set("sess-b", "3".to_string());
        assert_eq!(m.get("sess-a"), Some(&"12".to_string()));
        assert_eq!(m.get("sess-b"), Some(&"3".to_string()));
    }

    #[tokio::test]
    async fn round_trip_via_disk() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("chan_xyz");
        std::fs::create_dir_all(&channel_dir).unwrap();

        let mut m = ChannelReadMarks::new();
        m.set("sess-1", "41".to_string());
        m.set("sess-2", "7".to_string());
        m.save(&channel_dir).await.unwrap();

        let loaded = ChannelReadMarks::load(&channel_dir).await.unwrap();
        assert_eq!(loaded.get("sess-1"), Some(&"41".to_string()));
        assert_eq!(loaded.get("sess-2"), Some(&"7".to_string()));
    }

    #[tokio::test]
    async fn load_missing_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("nope");
        std::fs::create_dir_all(&channel_dir).unwrap();
        let m = ChannelReadMarks::load(&channel_dir).await.unwrap();
        assert!(m.0.is_empty());
    }

    #[tokio::test]
    async fn save_atomic_overwrites_existing_without_tmp_leftovers() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("chan_overwrite");
        std::fs::create_dir_all(&channel_dir).unwrap();

        let mut m = ChannelReadMarks::new();
        m.set("sess-v1", "1".to_string());
        m.save(&channel_dir).await.unwrap();

        // Save a different value — should atomically replace.
        let mut m2 = ChannelReadMarks::new();
        m2.set("sess-v2", "2".to_string());
        m2.save(&channel_dir).await.unwrap();

        let loaded = ChannelReadMarks::load(&channel_dir).await.unwrap();
        assert_eq!(loaded.0.len(), 1);
        assert_eq!(loaded.get("sess-v2"), Some(&"2".to_string()));

        // And no tmp files left behind.
        let leftover: Vec<_> = std::fs::read_dir(&channel_dir)
            .unwrap()
            .filter_map(std::io::Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".read_marks.json.")
            })
            .collect();
        assert!(leftover.is_empty(), "found stale tmp files: {leftover:?}");
    }
}
