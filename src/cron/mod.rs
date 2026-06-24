//! Cron scheduler for periodic task execution
//!
//! Stores cron jobs in a JSON file and provides scheduling functionality.
//! Supports both main session (system event) and isolated execution modes.
//!
//! Includes idle detection and event-based triggers.

#![allow(dead_code)]

pub mod event_trigger;
pub mod events;
pub mod idle;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use tracing::info;

/// Normalize a 5-field cron expression to the 7-field format required by the `cron` crate.
///
/// The `cron` crate v0.12 expects: `sec min hour day month weekday year`
/// Standard crontab uses: `min hour day month weekday`
///
/// This helper adds `0` for seconds and `*` for year when a 5-field expression is detected.
/// Expressions with 6 or 7 fields are left unchanged.
pub fn normalize_cron_expr(expr: &str) -> String {
    let trimmed = expr.trim();
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    match parts.len() {
        5 => format!("0 {trimmed} *"),
        _ => trimmed.to_string(),
    }
}

pub use idle::IdleDetector;

/// Schedule kinds for cron jobs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleKind {
    /// One-shot at specific time
    At { at: String },
    /// Recurring interval in milliseconds
    Every { every_ms: u64 },
    /// Cron expression with optional timezone
    Cron { expr: String, tz: Option<String> },
    /// Trigger when agent has been idle for N minutes
    Idle {
        minutes: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
    },
    /// Trigger when specific system event occurs
    Event {
        event_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<serde_json::Value>,
        #[serde(default)]
        once: bool,
    },
}

impl ScheduleKind {
    /// Get display name for the schedule
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            ScheduleKind::At { at } => format!("at {at}"),
            ScheduleKind::Every { every_ms } => {
                let secs = every_ms / 1000;
                if secs < 60 {
                    format!("every {secs}s")
                } else if secs < 3600 {
                    format!("every {}m", secs / 60)
                } else {
                    format!("every {}h", secs / 3600)
                }
            }
            ScheduleKind::Cron { expr, tz } => {
                if let Some(tz) = tz {
                    format!("cron '{expr}' ({tz})")
                } else {
                    format!("cron '{expr}'")
                }
            }
            ScheduleKind::Idle { minutes, agent_id } => {
                if let Some(agent) = agent_id {
                    format!("idle {minutes}m (agent: {agent})")
                } else {
                    format!("idle {minutes}m (any agent)")
                }
            }
            ScheduleKind::Event {
                event_type,
                filter,
                once,
            } => {
                let filter_info = filter.as_ref().map_or("", |_| " [filtered]");
                let once_info = if *once { " (once)" } else { "" };
                format!("event '{event_type}'{filter_info}{once_info}")
            }
        }
    }
}

/// Execution target for cron jobs
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExecutionTarget {
    /// Run in main agent session (system event)
    #[default]
    Main,
    /// Run in isolated session (dedicated agent turn)
    Isolated,
}

/// Delivery configuration for job results
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DeliveryMode {
    /// No delivery, silent execution
    #[default]
    None,
    /// Announce results to channel
    Announce {
        channel: Option<String>,
        to: Option<String>,
        best_effort: bool,
    },
}

/// A scheduled cron job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub schedule: ScheduleKind,
    pub target: ExecutionTarget,
    pub agent_id: Option<String>,
    pub message: String,
    pub delivery: DeliveryMode,
    pub delete_after_run: bool,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub run_count: u32,
}

/// Cron job run record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronRun {
    pub id: String,
    pub job_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// On-disk representation of the cron database
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronDatabase {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    jobs: Vec<CronJob>,
    #[serde(default)]
    runs: Vec<CronRun>,
}

impl Default for CronDatabase {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: Vec::new(),
            runs: Vec::new(),
        }
    }
}

/// Cron scheduler manages scheduled jobs
pub struct CronScheduler {
    db_path: PathBuf,
}

impl CronScheduler {
    /// Create a new cron scheduler with the given database path
    pub fn new(db_path: impl Into<PathBuf>) -> Result<Self> {
        let scheduler = Self {
            db_path: db_path.into(),
        };
        scheduler.init_db()?;
        Ok(scheduler)
    }

    /// Initialize the database file if it does not exist
    fn init_db(&self) -> Result<()> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create cron directory: {}", parent.display())
            })?;
        }

        if !self.db_path.exists() {
            let db = CronDatabase::default();
            self.write_db(&db)?;
        }

        Ok(())
    }

    /// Read the database from disk
    fn read_db(&self) -> Result<CronDatabase> {
        if !self.db_path.exists() {
            return Ok(CronDatabase::default());
        }

        let content = std::fs::read_to_string(&self.db_path)
            .with_context(|| format!("Failed to read cron DB: {}", self.db_path.display()))?;

        if content.trim().is_empty() {
            return Ok(CronDatabase::default());
        }

        let db: CronDatabase = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse cron DB: {}", self.db_path.display()))?;

        Ok(db)
    }

    /// Write the database to disk atomically
    fn write_db(&self, db: &CronDatabase) -> Result<()> {
        let json = serde_json::to_string_pretty(db).context("Failed to serialize cron database")?;

        // Write to a temp file first, then rename for atomicity
        let tmp_path = self.db_path.with_extension("tmp");
        std::fs::write(&tmp_path, json)
            .with_context(|| format!("Failed to write cron temp file: {}", tmp_path.display()))?;

        std::fs::rename(&tmp_path, &self.db_path)
            .with_context(|| format!("Failed to finalize cron DB: {}", self.db_path.display()))?;

        Ok(())
    }

    /// Add a new cron job
    pub fn add_job(&self, job: &CronJob) -> Result<()> {
        let mut db = self.read_db()?;

        if db.jobs.iter().any(|j| j.id == job.id) {
            anyhow::bail!("Cron job with id '{}' already exists", job.id);
        }

        db.jobs.push(job.clone());
        self.write_db(&db)?;

        info!(
            "Added cron job {}: '{}' with schedule {}",
            job.id,
            job.name,
            job.schedule.display()
        );

        Ok(())
    }

    /// Get a job by ID
    pub fn get_job(&self, job_id: &str) -> Result<Option<CronJob>> {
        let db = self.read_db()?;
        Ok(db.jobs.into_iter().find(|j| j.id == job_id))
    }

    /// List all cron jobs
    pub fn list_jobs(&self, include_disabled: bool) -> Result<Vec<CronJob>> {
        let db = self.read_db()?;
        let mut jobs: Vec<CronJob> = if include_disabled {
            db.jobs
        } else {
            db.jobs.into_iter().filter(|j| j.enabled).collect()
        };
        jobs.sort_by(|a, b| a.next_run.cmp(&b.next_run));
        Ok(jobs)
    }

    /// Get jobs that are due to run
    pub fn due_jobs(&self, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
        let db = self.read_db()?;
        let mut jobs: Vec<CronJob> = db
            .jobs
            .into_iter()
            .filter(|j| j.enabled && j.next_run <= now)
            .collect();
        jobs.sort_by(|a, b| a.next_run.cmp(&b.next_run));
        Ok(jobs)
    }

    /// Update job after execution
    pub fn update_job_after_run(
        &self,
        job_id: &str,
        status: &str,
        next_run: DateTime<Utc>,
    ) -> Result<()> {
        let mut db = self.read_db()?;

        if let Some(job) = db.jobs.iter_mut().find(|j| j.id == job_id) {
            job.last_run = Some(Utc::now());
            job.last_status = Some(status.to_string());
            job.next_run = next_run;
            job.run_count += 1;
            self.write_db(&db)?;
        }

        Ok(())
    }

    /// Delete a job
    pub fn delete_job(&self, job_id: &str) -> Result<bool> {
        let mut db = self.read_db()?;
        let before = db.jobs.len();
        db.jobs.retain(|j| j.id != job_id);
        let deleted = db.jobs.len() < before;

        if deleted {
            // Also clean up associated runs
            db.runs.retain(|r| r.job_id != job_id);
            self.write_db(&db)?;
            info!("Deleted cron job {}", job_id);
        }

        Ok(deleted)
    }

    /// Enable/disable a job
    pub fn set_job_enabled(&self, job_id: &str, enabled: bool) -> Result<bool> {
        let mut db = self.read_db()?;

        if let Some(job) = db.jobs.iter_mut().find(|j| j.id == job_id) {
            job.enabled = enabled;
            self.write_db(&db)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Record a job run
    pub fn record_run(&self, run: &CronRun) -> Result<()> {
        let mut db = self.read_db()?;
        db.runs.push(run.clone());
        // Keep only the last 1000 runs to prevent unbounded growth
        const MAX_RUNS: usize = 1000;
        if db.runs.len() > MAX_RUNS {
            db.runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
            db.runs.truncate(MAX_RUNS);
        }
        self.write_db(&db)?;
        Ok(())
    }

    /// Get run history for a job
    pub fn get_run_history(&self, job_id: &str, limit: usize) -> Result<Vec<CronRun>> {
        let db = self.read_db()?;
        let mut runs: Vec<CronRun> = db.runs.into_iter().filter(|r| r.job_id == job_id).collect();
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs.truncate(limit);
        Ok(runs)
    }

    /// Calculate next run time for a schedule
    pub fn calculate_next_run(
        &self,
        schedule: &ScheduleKind,
        after: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        calculate_next_run(schedule, after)
    }

    /// Get idle-triggered jobs
    pub fn idle_jobs(&self, include_disabled: bool) -> Result<Vec<CronJob>> {
        let jobs = self.list_jobs(include_disabled)?;
        Ok(jobs
            .into_iter()
            .filter(|j| matches!(j.schedule, ScheduleKind::Idle { .. }))
            .collect())
    }

    /// Get event-triggered jobs
    pub fn event_jobs(&self, include_disabled: bool) -> Result<Vec<CronJob>> {
        let jobs = self.list_jobs(include_disabled)?;
        Ok(jobs
            .into_iter()
            .filter(|j| matches!(j.schedule, ScheduleKind::Event { .. }))
            .collect())
    }

    /// Find jobs that are due but have never run (missed during downtime)
    pub fn missed_jobs(&self, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
        let db = self.read_db()?;
        let mut jobs: Vec<CronJob> = db
            .jobs
            .into_iter()
            .filter(|j| j.enabled && j.next_run <= now && j.last_run.is_none())
            .collect();
        jobs.sort_by(|a, b| a.next_run.cmp(&b.next_run));
        Ok(jobs)
    }

    /// Recalculate and update next_run for a job based on its schedule
    pub fn recalculate_next_run(
        &self,
        job_id: &str,
        after: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        let job = self
            .get_job(job_id)?
            .ok_or_else(|| anyhow::anyhow!("Job not found: {job_id}"))?;
        let next_run = calculate_next_run(&job.schedule, after)?;
        let mut db = self.read_db()?;
        if let Some(job) = db.jobs.iter_mut().find(|j| j.id == job_id) {
            job.next_run = next_run;
            self.write_db(&db)?;
        }
        Ok(next_run)
    }
}

/// Calculate next run time for a schedule (standalone, no storage needed)
pub fn calculate_next_run(schedule: &ScheduleKind, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    match schedule {
        ScheduleKind::At { at } => {
            let dt = DateTime::parse_from_rfc3339(at)
                .map_err(|e| anyhow::anyhow!("Invalid timestamp: {e}"))?;
            Ok(dt.with_timezone(&Utc))
        }
        ScheduleKind::Every { every_ms } => {
            Ok(after + chrono::Duration::milliseconds(*every_ms as i64))
        }
        ScheduleKind::Cron { expr, tz } => {
            let normalized = normalize_cron_expr(expr);
            let schedule = Schedule::from_str(&normalized)
                .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;

            if let Some(tz_str) = tz {
                let tz: chrono_tz::Tz = tz_str
                    .parse()
                    .map_err(|e| anyhow::anyhow!("Invalid timezone: {e}"))?;
                let local_after = after.with_timezone(&tz);
                if let Some(next) = schedule.after(&local_after).next() {
                    Ok(next.with_timezone(&Utc))
                } else {
                    Err(anyhow::anyhow!("No next occurrence found"))
                }
            } else if let Some(next) = schedule.after(&after).next() {
                Ok(next)
            } else {
                Err(anyhow::anyhow!("No next occurrence found"))
            }
        }
        // Idle and Event triggers don't have a predictable next run time
        // They return a far-future date to avoid being picked up by due_jobs
        ScheduleKind::Idle { .. } => {
            Ok(after + chrono::Duration::days(365 * 100)) // 100 years in the future
        }
        ScheduleKind::Event { .. } => {
            Ok(after + chrono::Duration::days(365 * 100)) // 100 years in the future
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[test]
    fn test_add_and_list_job() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.json");
        let scheduler = CronScheduler::new(&db_path).unwrap();

        let job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Test Job".to_string(),
            schedule: ScheduleKind::Every { every_ms: 60000 },
            target: ExecutionTarget::Main,
            agent_id: None,
            message: "Test message".to_string(),
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        scheduler.add_job(&job).unwrap();
        let jobs = scheduler.list_jobs(false).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "Test Job");
    }

    #[test]
    fn test_due_jobs() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.json");
        let scheduler = CronScheduler::new(&db_path).unwrap();

        let past_job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Past Job".to_string(),
            schedule: ScheduleKind::Every { every_ms: 60000 },
            target: ExecutionTarget::Main,
            agent_id: None,
            message: "Test".to_string(),
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - chrono::Duration::hours(1),
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        let future_job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Future Job".to_string(),
            schedule: ScheduleKind::Every { every_ms: 60000 },
            target: ExecutionTarget::Main,
            agent_id: None,
            message: "Test".to_string(),
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + chrono::Duration::hours(1),
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        scheduler.add_job(&past_job).unwrap();
        scheduler.add_job(&future_job).unwrap();

        let due = scheduler.due_jobs(Utc::now()).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "Past Job");
    }

    #[test]
    fn test_missed_jobs_recovery() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.json");
        let scheduler = CronScheduler::new(&db_path).unwrap();

        // Add a past job (missed)
        let past_job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Missed Job".to_string(),
            schedule: ScheduleKind::At {
                at: (Utc::now() - chrono::Duration::hours(2)).to_rfc3339(),
            },
            target: ExecutionTarget::Main,
            agent_id: None,
            message: "Test".to_string(),
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - chrono::Duration::hours(1),
            last_run: None,
            last_status: None,
            run_count: 0,
        };
        scheduler.add_job(&past_job).unwrap();

        // Add a future job (not missed)
        let future_job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Future Job".to_string(),
            schedule: ScheduleKind::Every {
                every_ms: 3_600_000,
            },
            target: ExecutionTarget::Main,
            agent_id: None,
            message: "Test".to_string(),
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + chrono::Duration::hours(1),
            last_run: None,
            last_status: None,
            run_count: 0,
        };
        scheduler.add_job(&future_job).unwrap();

        let missed = scheduler.missed_jobs(Utc::now()).unwrap();
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].name, "Missed Job");
    }

    #[test]
    fn test_recalculate_next_run() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.json");
        let scheduler = CronScheduler::new(&db_path).unwrap();

        let job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Recurring".to_string(),
            schedule: ScheduleKind::Every { every_ms: 60000 },
            target: ExecutionTarget::Main,
            agent_id: None,
            message: "Test".to_string(),
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
        };
        scheduler.add_job(&job).unwrap();

        let after = Utc::now();
        let next_run = scheduler.recalculate_next_run(&job.id, after).unwrap();

        // Should be about 60 seconds after `after`
        let diff = (next_run - after).num_milliseconds().abs();
        assert!(
            (59000..=61000).contains(&diff),
            "Expected ~60s, got {}ms",
            diff
        );
    }

    #[test]
    fn test_normalize_cron_expr() {
        // 5-field expressions should be normalized to 7-field
        assert_eq!(normalize_cron_expr("0 0 * * *"), "0 0 0 * * * *");
        assert_eq!(normalize_cron_expr("*/5 * * * *"), "0 */5 * * * * *");
        assert_eq!(normalize_cron_expr("30 9 * * 1"), "0 30 9 * * 1 *");

        // 7-field expressions should remain unchanged
        assert_eq!(
            normalize_cron_expr("0 30 9,12,15 1,15 May-Aug Mon,Wed,Fri 2018/2"),
            "0 30 9,12,15 1,15 May-Aug Mon,Wed,Fri 2018/2"
        );

        // Verify normalized expressions parse successfully with the cron crate
        let normalized = normalize_cron_expr("0 0 * * *");
        assert!(Schedule::from_str(&normalized).is_ok());
    }

    #[test]
    fn test_json_persistence() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.json");

        // Create and add a job
        {
            let scheduler = CronScheduler::new(&db_path).unwrap();
            let job = CronJob {
                id: "test-123".to_string(),
                name: "Persisted Job".to_string(),
                schedule: ScheduleKind::Every { every_ms: 60000 },
                target: ExecutionTarget::Main,
                agent_id: None,
                message: "Hello".to_string(),
                delivery: DeliveryMode::None,
                delete_after_run: false,
                enabled: true,
                created_at: Utc::now(),
                next_run: Utc::now(),
                last_run: None,
                last_status: None,
                run_count: 42,
            };
            scheduler.add_job(&job).unwrap();
        }

        // Verify JSON file exists and is readable
        assert!(db_path.exists());
        let content = std::fs::read_to_string(&db_path).unwrap();
        assert!(content.contains("Persisted Job"));
        assert!(content.contains("test-123"));

        // Re-open and verify data is intact
        {
            let scheduler = CronScheduler::new(&db_path).unwrap();
            let job = scheduler
                .get_job("test-123")
                .unwrap()
                .expect("job should exist");
            assert_eq!(job.name, "Persisted Job");
            assert_eq!(job.run_count, 42);
        }
    }
}
