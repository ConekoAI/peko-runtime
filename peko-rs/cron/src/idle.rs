//! Idle detection for `Idle` schedules.
//!
//! A principal is *active* when people talk to it: a human-started
//! (ingress) turn starts or ends, or a human message is queued for a
//! busy session. Cron turns, completion wakes and other principals'
//! posts are not activity — otherwise a 10-minute keepalive would keep
//! every principal permanently "active".
//!
//! Activity is wall-clock time keyed by `PrincipalId`, persisted to one
//! JSON file when the detector is [`IdleDetector::persistent`], so a
//! daemon restart neither forgets it nor makes every principal look
//! idle. A principal with no record counts as active from the moment
//! the detector started, so nothing fires at boot.
//!
//! An `Idle { minutes }` job is due once per idle stretch: when the
//! principal has been idle for `minutes` and the job has not run since
//! the last activity ([`IdleDetector::idle_job_due`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tokio::sync::Mutex;
use tracing::{trace, warn};

/// Tracks when each principal was last active.
#[derive(Debug, Clone)]
pub struct IdleDetector {
    last_activity: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
    /// Where activity persists; `None` keeps it in memory (tests).
    path: Option<PathBuf>,
    /// Stand-in activity time for principals with no record.
    started_at: DateTime<Utc>,
}

impl IdleDetector {
    /// An in-memory detector (nothing survives the process).
    pub fn new() -> Self {
        Self {
            last_activity: Arc::default(),
            path: None,
            started_at: Utc::now(),
        }
    }

    /// A detector persisted at `path` (a JSON map of principal id to
    /// RFC 3339 time), loading what is already there. An unreadable
    /// file is logged and treated as empty.
    pub fn persistent(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let loaded = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                warn!(
                    "idle activity file {} unreadable ({e}); starting empty",
                    path.display()
                );
                HashMap::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                warn!(
                    "idle activity file {} unreadable ({e}); starting empty",
                    path.display()
                );
                HashMap::new()
            }
        };
        Self {
            last_activity: Arc::new(Mutex::new(loaded)),
            path: Some(path),
            started_at: Utc::now(),
        }
    }

    /// Record activity for `principal_id` now.
    pub async fn record_activity(&self, principal_id: &str) {
        self.record_activity_at(principal_id, Utc::now()).await;
    }

    /// Record activity for `principal_id` at `at` (never moves a record
    /// backwards). Persistence failures are logged, not returned:
    /// activity tracking must never fail a turn.
    pub async fn record_activity_at(&self, principal_id: &str, at: DateTime<Utc>) {
        let mut activity = self.last_activity.lock().await;
        let entry = activity.entry(principal_id.to_string()).or_insert(at);
        if *entry < at {
            *entry = at;
        }
        trace!("Recorded activity for principal {principal_id}");
        if let Some(path) = &self.path {
            if let Err(e) = save(path, &activity).await {
                warn!("idle activity: persisting {} failed: {e}", path.display());
            }
        }
    }

    /// When `principal_id` was last active (the detector's start time
    /// when there is no record).
    pub async fn last_activity(&self, principal_id: &str) -> DateTime<Utc> {
        self.last_activity
            .lock()
            .await
            .get(principal_id)
            .copied()
            .unwrap_or(self.started_at)
    }

    /// Whether `principal_id` has been idle for at least `minutes` at `now`.
    pub async fn is_idle(&self, principal_id: &str, minutes: u64, now: DateTime<Utc>) -> bool {
        now - self.last_activity(principal_id).await >= idle_window(minutes)
    }

    /// Whether an `Idle { minutes }` job last run at `last_run` is due at
    /// `now`: idle long enough, and not yet run in this idle stretch.
    pub async fn idle_job_due(
        &self,
        principal_id: &str,
        minutes: u64,
        last_run: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        let last_activity = self.last_activity(principal_id).await;
        now - last_activity >= idle_window(minutes)
            && last_run.is_none_or(|run| run < last_activity)
    }
}

impl Default for IdleDetector {
    fn default() -> Self {
        Self::new()
    }
}

fn idle_window(minutes: u64) -> Duration {
    Duration::minutes(i64::try_from(minutes).unwrap_or(i64::MAX / 60_000))
}

/// Atomic write: tmp file beside the target, then rename.
async fn save(path: &Path, activity: &HashMap<String, DateTime<Utc>>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(activity)?).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_record_counts_as_active_since_start() {
        let detector = IdleDetector::new();
        let start = detector.last_activity("p").await;
        assert!(!detector.is_idle("p", 1, start).await);
        assert!(detector.is_idle("p", 1, start + Duration::minutes(1)).await);
    }

    #[tokio::test]
    async fn recorded_activity_resets_the_window_and_never_rewinds() {
        let detector = IdleDetector::new();
        let t0 = Utc::now() + Duration::hours(1);
        detector.record_activity_at("p", t0).await;
        assert!(!detector.is_idle("p", 10, t0 + Duration::minutes(9)).await);
        assert!(detector.is_idle("p", 10, t0 + Duration::minutes(10)).await);
        detector
            .record_activity_at("p", t0 - Duration::minutes(5))
            .await;
        assert_eq!(detector.last_activity("p").await, t0);
        // Other principals are tracked separately.
        assert_eq!(detector.last_activity("q").await, detector.started_at);
    }

    /// Due once per idle stretch: after the job runs, it waits for new
    /// activity and another full idle window.
    #[tokio::test]
    async fn idle_job_is_due_once_per_idle_stretch() {
        let detector = IdleDetector::new();
        let t0 = Utc::now() + Duration::hours(1);
        detector.record_activity_at("p", t0).await;
        let later = t0 + Duration::minutes(30);
        assert!(
            !detector
                .idle_job_due("p", 30, None, later - Duration::seconds(1))
                .await
        );
        assert!(detector.idle_job_due("p", 30, None, later).await);
        // It ran: not due again in the same stretch, however long.
        let ran = Some(later);
        assert!(
            !detector
                .idle_job_due("p", 30, ran, later + Duration::days(3))
                .await
        );
        // New activity starts a new stretch.
        let t1 = later + Duration::hours(1);
        detector.record_activity_at("p", t1).await;
        assert!(
            !detector
                .idle_job_due("p", 30, ran, t1 + Duration::minutes(29))
                .await
        );
        assert!(
            detector
                .idle_job_due("p", 30, ran, t1 + Duration::minutes(30))
                .await
        );
    }

    #[tokio::test]
    async fn persistent_detector_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime").join("idle_activity.json");
        let t0 = Utc::now() - Duration::hours(2);
        IdleDetector::persistent(&path)
            .record_activity_at("p", t0)
            .await;

        let reborn = IdleDetector::persistent(&path);
        assert_eq!(reborn.last_activity("p").await, t0);
        assert!(
            reborn.is_idle("p", 60, Utc::now()).await,
            "two hours idle, across the restart"
        );

        std::fs::write(&path, b"not json").unwrap();
        let fresh = IdleDetector::persistent(&path);
        assert_eq!(fresh.last_activity("p").await, fresh.started_at);
    }
}
