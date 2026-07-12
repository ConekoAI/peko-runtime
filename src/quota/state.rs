//! Runtime quota state: counters for the current calendar window,
//! persisted to `quota_state.json` next to `principal.toml`.
//!
//! ## Persistence
//!
//! Mirrors the atomic temp+rename pattern at
//! `src/principal/memory.rs:114-139`. A half-written `.tmp` is
//! recoverable on the next load: `save` only renames over the
//! `.json` after `sync_all`, so a crash mid-write leaves the
//! previous good copy intact and the `.tmp` orphaned. The loader
//! silently discards `.tmp` siblings.
//!
//! ## Field semantics
//!
//! - `window_start` — start of the current window (inclusive).
//! - `window_end` — end of the current window (exclusive). When
//!   `now >= window_end`, the meter rolls forward.
//! - `cycle` — copy of the cycle that produced this state. If the
//!   config's cycle later changes, the meter detects the mismatch
//!   on `advance_if_needed` and resets with the new cycle.
//! - `input_tokens` / `output_tokens` / `request_count` —
//!   cumulative counters within the current window.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::AsyncWriteExt;

use super::config::QuotaCycle;

/// Runtime counters for one principal's current quota window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaState {
    /// Window start (inclusive, UTC).
    pub window_start: DateTime<Utc>,
    /// Window end (exclusive, UTC). When `now >= window_end` the
    /// meter rolls forward.
    pub window_end: DateTime<Utc>,
    /// Cycle that produced this window — copied from the config so
    /// we can detect config drift on reload.
    pub cycle: QuotaCycle,
    /// Cumulative input tokens consumed in the current window.
    pub input_tokens: u64,
    /// Cumulative output tokens consumed in the current window.
    pub output_tokens: u64,
    /// Cumulative LLM requests made in the current window.
    pub request_count: u64,
}

impl QuotaState {
    /// Construct a fresh state for `cycle` at `now`. Used by
    /// `QuotaMeter::reset` and by the initial create-without-disk
    /// path.
    #[must_use]
    pub fn fresh(cycle: QuotaCycle, now: DateTime<Utc>) -> Self {
        let (window_start, window_end) = cycle.window_bounds(now);
        Self {
            window_start,
            window_end,
            cycle,
            input_tokens: 0,
            output_tokens: 0,
            request_count: 0,
        }
    }

    /// Load from `<path>`. Returns `None` when the file does not
    /// exist (a fresh principal). Errors only on I/O or parse
    /// failures; the atomic-rename persistence guarantees we never
    /// see a partial write.
    pub async fn load(path: &Path) -> std::io::Result<Option<Self>> {
        match tokio::fs::read(path).await {
            Ok(bytes) => {
                let state: Self = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Persist atomically. Writes to `<path>.tmp`, flushes, then
    /// `rename`s over `<path>`. A crash before the rename leaves
    /// the previous good copy intact.
    pub async fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = tokio::fs::File::create(&tmp).await?;
            f.write_all(&bytes).await?;
            f.sync_all().await?;
        }
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> DateTime<Utc> {
        chrono::Utc.with_ymd_and_hms(year, month, day, hour, min, sec).single().unwrap()
    }

    #[test]
    fn fresh_daily_window_at_midnight() {
        let now = ts(2026, 7, 12, 0, 0, 0);
        let s = QuotaState::fresh(QuotaCycle::Daily, now);
        assert_eq!(s.cycle, QuotaCycle::Daily);
        assert_eq!(s.window_start, ts(2026, 7, 12, 0, 0, 0));
        assert_eq!(s.window_end, ts(2026, 7, 13, 0, 0, 0));
        assert_eq!(s.input_tokens, 0);
        assert_eq!(s.output_tokens, 0);
        assert_eq!(s.request_count, 0);
    }

    #[test]
    fn fresh_hourly_window_partway_through_hour() {
        let now = ts(2026, 7, 12, 14, 37, 22);
        let s = QuotaState::fresh(QuotaCycle::Hourly, now);
        assert_eq!(s.window_start, ts(2026, 7, 12, 14, 0, 0));
        assert_eq!(s.window_end, ts(2026, 7, 12, 15, 0, 0));
    }

    #[tokio::test]
    async fn load_returns_none_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("quota_state.json");
        let loaded = QuotaState::load(&path).await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("quota_state.json");
        let original = QuotaState {
            window_start: ts(2026, 7, 12, 14, 0, 0),
            window_end: ts(2026, 7, 12, 15, 0, 0),
            cycle: QuotaCycle::Hourly,
            input_tokens: 1234,
            output_tokens: 567,
            request_count: 9,
        };
        original.save(&path).await.unwrap();
        let loaded = QuotaState::load(&path).await.unwrap().unwrap();
        assert_eq!(loaded, original);
        // ts must round-trip too — verifies the DateTime<Utc>
        // serde wire format stays stable.
        assert_eq!(loaded.window_start, original.window_start);
        assert_eq!(loaded.window_end, original.window_end);
    }

    #[tokio::test]
    async fn save_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("deeper").join("quota_state.json");
        let s = QuotaState::fresh(QuotaCycle::Daily, ts(2026, 7, 12, 0, 0, 0));
        s.save(&path).await.unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn save_overwrites_previous_atomic_rename() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("quota_state.json");

        let mut s = QuotaState::fresh(QuotaCycle::Daily, ts(2026, 7, 12, 0, 0, 0));
        s.input_tokens = 100;
        s.save(&path).await.unwrap();

        s.input_tokens = 200;
        s.output_tokens = 50;
        s.save(&path).await.unwrap();

        let loaded = QuotaState::load(&path).await.unwrap().unwrap();
        assert_eq!(loaded.input_tokens, 200);
        assert_eq!(loaded.output_tokens, 50);
        // No orphaned .tmp should remain after a successful save.
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists());
    }
}