//! Per-channel cursor map (runtime-tier persistence).
//!
//! One `cursors.json` per channel, located at
//! `<runtime_dir>/channels/<channel_id>/cursors.json`. Tracks, for each
//! member, the highest `TaskId` they've already observed in their
//! subscription poll. Lets `ChannelSubscriber::tick_once` filter out
//! events a principal has already seen — no double-delivery, no
//! spurious `consider_response` calls.
//!
//! PR-1 ships runtime-tier only. PR-3 introduces a Shared-tier cursor
//! if/when channels opt into shared persistence (the RuntimeAuthority
//! seal on the file path distinguishes the two).
//!
//! ## Atomic-write convention
//!
//! Mirror of `peko-rs/plan/src/storage.rs:309-321` — write to a
//! pid-suffixed tmp file, fsync, rename over the destination. This is
//! the same pattern `peko_cron`, `peko_session`, and `peko_plan` use,
//! so channels slot in without inventing a new convention.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use peko_plan::PrincipalId;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::port::{ChannelError, Result, TaskId};

// ---------------------------------------------------------------------------
// Cursors file shape
// ---------------------------------------------------------------------------

/// Per-member cursor map. The outer map's key is the principal id; the
/// value is the highest `TaskId` (a `peko_plan::NodeId` newtype
/// string) the principal has *already observed*. PR-1: serialized as
/// flat `HashMap<String, String>`. PR-3 may move to a CBOR/msgpack
/// shape if the file size grows; JSON is fine for PR-1's fan-out cap
/// of 8 members.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChannelCursors(pub HashMap<PrincipalId, TaskId>);

impl ChannelCursors {
    /// Construct an empty cursor map.
    pub fn new() -> Self {
        Self(Default::default())
    }

    /// Get the cursor for `principal` (None if the principal has never
    /// observed any event — fresh subscriber).
    pub fn get(&self, principal: &PrincipalId) -> Option<&TaskId> {
        self.0.get(principal)
    }

    /// Advance the cursor for `principal` to `task_id`. Strictly
    /// monotonic in PR-1: callers should refuse to set a smaller value
    /// (we let callers do that, since some PR-3 paths may need to
    /// rewind for cross-tier sync).
    pub fn set(&mut self, principal: PrincipalId, task_id: TaskId) {
        self.0.insert(principal, task_id);
    }

    /// True if `principal` has never been recorded (no observation yet).
    pub fn is_fresh(&self, principal: &PrincipalId) -> bool {
        !self.0.contains_key(principal)
    }

    // -----------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------

    /// Standard location of the cursors file inside a channel directory.
    pub fn path_in(channel_dir: &Path) -> PathBuf {
        channel_dir.join("cursors.json")
    }

    /// Load cursors from `<channel_dir>/cursors.json`. Returns an empty
    /// map if the file doesn't exist (fresh channel, never read).
    pub async fn load(channel_dir: &Path) -> Result<Self> {
        let path = Self::path_in(channel_dir);
        match fs::read(&path).await {
            Ok(bytes) => {
                let cursors: Self = serde_json::from_slice(&bytes)
                    .map_err(|e| ChannelError::Cursor(format!("decode {}: {e}", path.display())))?;
                Ok(cursors)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(ChannelError::Cursor(format!("read {}: {e}", path.display()))),
        }
    }

    /// Persist cursors atomically. Write to a pid-suffixed tmp file in
    /// the same directory, fsync, rename over the destination.
    ///
    /// **Convention mirror:** `peko-rs/plan/src/storage.rs:309-321`.
    /// Channels do not invent a new write path.
    pub async fn save(&self, channel_dir: &Path) -> Result<()> {
        let path = Self::path_in(channel_dir);
        fs::create_dir_all(channel_dir).await?;

        let pid = std::process::id();
        let tmp = channel_dir.join(format!(".cursors.json.{pid}.tmp"));

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
    fn get_set_is_fresh() {
        let mut c = ChannelCursors::new();
        let p = PrincipalId::generate();
        assert!(c.is_fresh(&p));
        c.set(p.clone(), "node_aaa".into());
        assert!(!c.is_fresh(&p));
        assert_eq!(c.get(&p), Some(&"node_aaa".to_string()));
    }

    #[tokio::test]
    async fn round_trip_via_disk() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("chan_xyz");
        std::fs::create_dir_all(&channel_dir).unwrap();

        let mut c = ChannelCursors::new();
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();
        c.set(p1.clone(), "node_111".into());
        c.set(p2.clone(), "node_222".into());

        c.save(&channel_dir).await.unwrap();

        let loaded = ChannelCursors::load(&channel_dir).await.unwrap();
        assert_eq!(loaded.get(&p1), Some(&"node_111".to_string()));
        assert_eq!(loaded.get(&p2), Some(&"node_222".to_string()));
    }

    #[tokio::test]
    async fn load_missing_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("nope");
        std::fs::create_dir_all(&channel_dir).unwrap();
        let c = ChannelCursors::load(&channel_dir).await.unwrap();
        assert!(c.0.is_empty());
    }

    #[tokio::test]
    async fn save_atomic_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("chan_overwrite");
        std::fs::create_dir_all(&channel_dir).unwrap();

        let mut c = ChannelCursors::new();
        c.set(PrincipalId::generate(), "node_v1".into());
        c.save(&channel_dir).await.unwrap();

        // Save a different value — should atomically replace.
        let mut c2 = ChannelCursors::new();
        c2.set(PrincipalId::generate(), "node_v2".into());
        c2.save(&channel_dir).await.unwrap();

        let loaded = ChannelCursors::load(&channel_dir).await.unwrap();
        assert_eq!(loaded.0.len(), 1);
        // The "v2" cursor must be the only entry.
        let values: Vec<&String> = loaded.0.values().collect();
        assert_eq!(values, vec![&"node_v2".to_string()]);

        // And no tmp files left behind.
        let leftover: Vec<_> = std::fs::read_dir(&channel_dir)
            .unwrap()
            .filter_map(std::io::Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".cursors.json.")
            })
            .collect();
        assert!(leftover.is_empty(), "found stale tmp files: {leftover:?}");
    }
}