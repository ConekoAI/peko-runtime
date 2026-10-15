//! Per-channel "waiting on you" lists (runtime-tier persistence).
//!
//! One `attention.json` per channel, beside `read_marks.json` at
//! `<runtime_dir>/channels/<channel_id>/attention.json`. For each
//! *session* (keyed by session-id string) it holds the posts by other
//! principals that the session has been shown but has not handled.
//!
//! Only humans and cron start runs (ADR-072 D1), so another
//! principal's post never wakes anyone: the session learns about it
//! from the `{{session_context}}` digest. The read mark records what the
//! digest has *observed*, which is not the same as handled — a post the
//! model glanced past on a keepalive turn would never come back. The
//! digest therefore files such posts here, re-lists them on every run,
//! and drops one only when the session reads it with `ChannelRead` or
//! the principal posts on the channel after it.
//!
//! Writes go through [`crate::ChannelStore::set_attention`], which holds
//! a file lock across load → replace → save so two sessions updating
//! the same channel never drop each other's lists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::port::{ChannelError, Result, TaskId};

/// Most posts kept waiting per (session, channel). Past this the oldest
/// are dropped; they stay in the channel log for `ChannelRead`.
pub const MAX_WAITING_PER_CHANNEL: usize = 50;

/// One post waiting on a session: enough to render a preview without
/// re-reading the channel log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionItem {
    /// The post's line-number id in the channel log.
    pub id: TaskId,
    /// The post's author (wire form).
    pub author: String,
    /// RFC 3339 time the post was written.
    pub at: String,
    /// A short preview of the post body.
    pub preview: String,
}

/// Per-session waiting lists for one channel, keyed by session-id string.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChannelAttention(pub HashMap<String, Vec<AttentionItem>>);

impl ChannelAttention {
    /// The posts waiting on `session_key` (empty when none).
    pub fn get(&self, session_key: &str) -> &[AttentionItem] {
        self.0.get(session_key).map_or(&[], Vec::as_slice)
    }

    /// Replace `session_key`'s list; an empty list removes the entry.
    /// Keeps at most [`MAX_WAITING_PER_CHANNEL`] items, dropping the
    /// oldest.
    pub fn set(&mut self, session_key: impl Into<String>, mut items: Vec<AttentionItem>) {
        let key = session_key.into();
        if items.is_empty() {
            self.0.remove(&key);
            return;
        }
        let excess = items.len().saturating_sub(MAX_WAITING_PER_CHANNEL);
        items.drain(..excess);
        self.0.insert(key, items);
    }

    /// Standard location of the attention file inside a channel directory.
    pub fn path_in(channel_dir: &Path) -> PathBuf {
        channel_dir.join("attention.json")
    }

    /// Load from `<channel_dir>/attention.json`; empty when absent.
    pub async fn load(channel_dir: &Path) -> Result<Self> {
        let path = Self::path_in(channel_dir);
        match fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| ChannelError::Cursor(format!("decode {}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ChannelError::Cursor(format!(
                "read {}: {e}",
                path.display()
            ))),
        }
    }

    /// Persist atomically: pid-suffixed tmp file, fsync, rename (the
    /// `read_marks.rs` convention).
    pub async fn save(&self, channel_dir: &Path) -> Result<()> {
        let path = Self::path_in(channel_dir);
        fs::create_dir_all(channel_dir).await?;
        let tmp = channel_dir.join(format!(".attention.json.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(self)?;
        {
            let mut f = fs::File::create(&tmp)
                .await
                .map_err(|e| ChannelError::Cursor(format!("create {}: {e}", tmp.display())))?;
            f.write_all(&bytes)
                .await
                .map_err(|e| ChannelError::Cursor(format!("write {}: {e}", tmp.display())))?;
            f.sync_all()
                .await
                .map_err(|e| ChannelError::Cursor(format!("fsync {}: {e}", tmp.display())))?;
        }
        fs::rename(&tmp, &path).await.map_err(|e| {
            ChannelError::Cursor(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                path.display()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: usize) -> AttentionItem {
        AttentionItem {
            id: id.to_string(),
            author: "prin_other".into(),
            at: "2026-10-15T00:00:00Z".into(),
            preview: format!("post {id}"),
        }
    }

    #[test]
    fn set_replaces_caps_and_removes_empty_lists() {
        let mut a = ChannelAttention::default();
        assert!(a.get("s").is_empty());
        a.set("s", (0..MAX_WAITING_PER_CHANNEL + 5).map(item).collect());
        let kept = a.get("s");
        assert_eq!(kept.len(), MAX_WAITING_PER_CHANNEL);
        assert_eq!(kept[0].id, "5", "the oldest are dropped");
        a.set("s", Vec::new());
        assert!(!a.0.contains_key("s"));
    }

    #[tokio::test]
    async fn round_trips_via_disk_and_loads_missing_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ChannelAttention::load(dir.path())
            .await
            .unwrap()
            .0
            .is_empty());
        let mut a = ChannelAttention::default();
        a.set("s1", vec![item(3)]);
        a.save(dir.path()).await.unwrap();
        let loaded = ChannelAttention::load(dir.path()).await.unwrap();
        assert_eq!(loaded.get("s1"), &[item(3)]);
    }
}
