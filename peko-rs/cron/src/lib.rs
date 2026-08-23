//! Cron domain — scheduler + tools + port + DTOs.
//!
//! Phase 0.Z-E (2026-07-25): this crate owns the entire cron surface
//! natively. Previously the DTOs + `CronRuntime` port + 3 cron tools
//! lived in `peko-tools-builtin` (Phase 10b), and this crate re-exported
//! them. After F4 (2026-07-25) lifted the bulk of built-in tools into
//! root, `peko-tools-builtin` survived only as a cron-port sat; 0.Z-E
//! deletes that sat and consolidates cron here.
//!
//! ## Layout
//!
//! - [`tools`] — the cron DTOs (`CronJob`, `CronJobAction`, `ScheduleKind`),
//!   the `CronRuntime` port trait + global registry, the helper functions,
//!   and the 3 tool impls (`CronCreateTool`, `CronDeleteTool`,
//!   `CronListTool`).
//! - This file — the `CronScheduler` (engine + on-disk persistence),
//!   `CronRun` records, `CronDatabase` schema. Daemon-internal state.
//! - [`idle`] — scheduler-side submodule for idle detection.
//!
//! ## Port (`CronRuntime`)
//!
//! [`tools::CronRuntime`] is the port the cron tools use to talk to the
//! daemon. The concrete implementation in root is
//! `peko_core::daemon::cron_runtime::DaemonCronAdapter`, which wraps
//! `peko_core::ipc::DaemonClient::cron_add/cron_remove/cron_list`. That
//! adapter stays in root because it depends on `DaemonClient`; it
//! implements the `CronRuntime` trait via the orphan rule (the trait
//! is foreign to root, but the adapter type is local).
//!
//! The trait deliberately does not touch IPC — the trait surface is
//! pure data (`CronJob` / `&str` / `Vec<CronJob>`). The adapter wires
//! the trait methods through to `DaemonClient` calls.

// Crate-wide `dead_code` allow is intentionally narrow: it covers
// the builder helpers (`build_send_job`/`build_spawn_tool_job`/
// `resolve_prompt`) that lint flags as dead from the crate's root
// because nothing in this crate's `lib.rs` symbols references them
// directly — they are consumed only via `pub use tools::{...}`
// re-exports by `peko_core::daemon::cron_runtime` and adjacent
// callers. Keep the allow narrow here; tighten at each helper's site
// if a future cleanup proves a subset unreachable.
#![allow(dead_code)]

pub mod idle;
pub mod tools;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
#[allow(unused_imports)]
use cron::Schedule;
#[allow(unused_imports)]
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[allow(unused_imports)]
use std::str::FromStr;
use tracing::info;

// ── Cron domain surface (canonical home — no re-export dance) ──
//
// Single source of truth for the cron DTOs + port trait + tools + helpers.
// All cron-related code (root, daemon, integration tests, CLI commands,
// IPC handlers) imports from `peko_cron::*` directly.
#[allow(unused_imports)]
pub use tools::{
    build_spawn_tool_job, calculate_next_interval_anchored, calculate_next_run,
    global_runtime, normalize_cron_expr, render_job_list, resolve_delete_after_run, resolve_label,
    resolve_schedule_kind, set_global_runtime, CronCreateTool, CronDeleteTool,
    CronJob, CronJobAction, CronListTool, CronRuntime, ScheduleKind,
    DEFAULT_MAX_RETRIES,
};

pub use idle::IdleDetector;

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
            // v2 introduces `CronJob.action` (Send | SpawnTool) in place
            // of the legacy top-level `message` field. Pre-launch: legacy
            // records simply fail to deserialize — operators should clear
            // `cron.json` rather than rely on a migration.
            version: 2,
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

        // Validate the action shape. Send/Notify require a non-empty
        // message; SpawnTool requires a non-empty tool name; a Send
        // `target` must be a known value (serde rejects unknown targets
        // at JSON load, but in-process struct-literal construction
        // bypasses that). A trunk-targeted Send additionally respects
        // the keepalive interval floor (`TRUNK_MIN_INTERVAL_MS`) — a
        // sub-minute self-poke loop is a runaway token-burn
        // anti-pattern (Phase 3b). Validation happens here so a
        // malformed job never reaches the on-disk DB.
        match &job.action {
            CronJobAction::Send { message, target } => {
                if message.trim().is_empty() {
                    anyhow::bail!(
                        "CronJob {} action requires a non-empty 'message'",
                        job.action.kind_label()
                    );
                }
                crate::tools::validate_send_target(target)?;
                crate::tools::validate_trunk_send_interval(&job.schedule, target)?;
            }
            CronJobAction::SpawnTool { tool_name, .. } => {
                if tool_name.trim().is_empty() {
                    anyhow::bail!("CronJob SpawnTool action requires a non-empty 'tool_name'");
                }
            }
        }

        // Reject one-shot jobs scheduled in the past. Without this they
        // are accepted and then parked on the `now + 100 years` sentinel
        // by `calculate_next_run`, showing "active" but never firing —
        // a silent zombie (2026-08-07 field test, N2b). All creation
        // surfaces (CronCreate tool, CLI `cron at`, IPC `CronAdd`)
        // funnel through this function. Note this also changes the
        // legacy CLI behavior where a past `--at` fired immediately on
        // the next poll — that immediacy was accidental.
        if let ScheduleKind::At { at } = &job.schedule {
            let dt = chrono::DateTime::parse_from_rfc3339(at)
                .map_err(|e| anyhow::anyhow!("Invalid 'at' timestamp (use RFC3339): {e}"))?;
            if dt.with_timezone(&Utc) <= Utc::now() {
                anyhow::bail!(
                    "'at' time {at} is in the past; use a future timestamp or interval_ms"
                );
            }
        }

        db.jobs.push(job.clone());
        self.write_db(&db)?;

        info!(
            "Added cron job {}: '{}' (action={}) with schedule {}",
            job.id,
            job.name,
            job.action.kind_label(),
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
        jobs.sort_by_key(|a| a.next_run);
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
        jobs.sort_by_key(|a| a.next_run);
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
            // Retry-budget accounting: a success resets the counter;
            // any non-success (failed, errored, …) bumps it. The engine
            // consults `consecutive_failures` after this call to decide
            // whether to disable the job.
            if status == "success" {
                job.consecutive_failures = 0;
            } else {
                job.consecutive_failures += 1;
            }
            self.write_db(&db)?;
        }

        Ok(())
    }

    /// Update only `last_status` (and `last_run`) on a job, leaving
    /// `next_run` and `run_count` untouched. Used by the cron
    /// reconciler when an `AsyncTask` finishes long after the original
    /// fire — the schedule is already advanced and we must not bump
    /// `run_count` again.
    ///
    /// `consecutive_failures` is still bumped/reset here so the
    /// `SpawnTool` reconcile path also respects the retry budget. If
    /// we skipped this, a `SpawnTool` job that fails would never
    /// disable — bypassing the budget that the `Send` path enforces.
    pub fn set_job_last_status(&self, job_id: &str, status: &str) -> Result<bool> {
        let mut db = self.read_db()?;
        let Some(job) = db.jobs.iter_mut().find(|j| j.id == job_id) else {
            return Ok(false);
        };
        job.last_run = Some(Utc::now());
        job.last_status = Some(status.to_string());
        if status == "success" {
            job.consecutive_failures = 0;
        } else {
            job.consecutive_failures += 1;
        }
        self.write_db(&db)?;
        Ok(true)
    }

    /// Recompute the cron job's `next_run` based on its stored
    /// schedule. Returns `None` for schedules that never re-fire
    /// (e.g. `At`) or when the job id is unknown.
    /// Delete a job. Run history is intentionally preserved: one-shot

    /// Delete a job. Run history is intentionally preserved: one-shot
    /// (`delete_after_run`) jobs delete themselves after firing, and
    /// purging their runs made `cron history <id>` unanswerable for a
    /// job that had just successfully run (2026-08-07 field test,
    /// Finding 4). Growth is bounded by `record_run`'s 1000-run cap.
    pub fn delete_job(&self, job_id: &str) -> Result<bool> {
        let mut db = self.read_db()?;
        let before = db.jobs.len();
        db.jobs.retain(|j| j.id != job_id);
        let deleted = db.jobs.len() < before;

        if deleted {
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
            db.runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
            db.runs.truncate(MAX_RUNS);
        }
        self.write_db(&db)?;
        Ok(())
    }

    /// Attach the async `output` (e.g. a spawned task id) and/or `error`
    /// to an OPEN run row without closing it. Used by the SpawnTool fire
    /// path: the task id is only known after the fire returns, and the
    /// run must stay `running` (unfinished) until the janitor reconciles
    /// the task's terminal state via [`Self::finalize_run`]. Keeps one
    /// row per run — the fire path must not `record_run` twice with the
    /// same id (2026-08-07 field test, F3).
    pub fn attach_run_output(
        &self,
        run_id: &str,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<bool> {
        let mut db = self.read_db()?;
        let Some(run) = db.runs.iter_mut().find(|r| r.id == run_id) else {
            return Ok(false);
        };
        if run.finished_at.is_some() {
            return Ok(false);
        }
        run.output = output;
        run.error = error;
        self.write_db(&db)?;
        Ok(true)
    }

    /// Get run history for a job
    pub fn get_run_history(&self, job_id: &str, limit: usize) -> Result<Vec<CronRun>> {        let db = self.read_db()?;
        let mut runs: Vec<CronRun> = db.runs.into_iter().filter(|r| r.job_id == job_id).collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        runs.truncate(limit);
        Ok(runs)
    }

    /// List all still-running runs. Used by the cron engine to reconcile
    /// `SpawnTool` fires whose underlying `AsyncTask` has since
    /// completed in the background.
    pub fn list_running_runs(&self) -> Result<Vec<CronRun>> {
        let db = self.read_db()?;
        Ok(db
            .runs
            .into_iter()
            .filter(|r| r.status == "running" && r.finished_at.is_none())
            .collect())
    }

    /// Finalize a still-running run row with the executor's terminal
    /// outcome. Returns `true` when a row was updated, `false` when
    /// the id no longer exists or the row is already finalized.
    pub fn finalize_run(
        &self,
        run_id: &str,
        status: &str,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<bool> {
        let mut db = self.read_db()?;
        let Some(run) = db.runs.iter_mut().find(|r| r.id == run_id) else {
            return Ok(false);
        };
        if run.finished_at.is_some() {
            return Ok(false);
        }
        run.status = status.to_string();
        run.output = output;
        run.error = error;
        run.finished_at = Some(Utc::now());
        self.write_db(&db)?;
        Ok(true)
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
            principal_id: PrincipalId("prin_test_principal".to_string()),
            action: CronJobAction::Send {
                message: "Test message".to_string(),
                target: None,
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };

        scheduler.add_job(&job).unwrap();
        let jobs = scheduler.list_jobs(false).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "Test Job");
        assert_eq!(jobs[0].action.kind_label(), "send");
    }

    /// One-shot (`delete_after_run`) jobs delete themselves after a
    /// successful fire, but their run history must survive so
    /// `cron history <id>` stays answerable (2026-08-07 field test,
    /// Finding 4).
    #[test]
    fn test_delete_job_preserves_run_history() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.json");
        let scheduler = CronScheduler::new(&db_path).unwrap();

        let job_id = Uuid::new_v4().to_string();
        let job = CronJob {
            id: job_id.clone(),
            name: "One Shot".to_string(),
            schedule: ScheduleKind::At {
                at: (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            },
            principal_id: PrincipalId("prin_test_principal".to_string()),
            action: CronJobAction::Send {
                message: "Test".to_string(),
                target: None,
            },
                        delete_after_run: true,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 1,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();
        scheduler
            .record_run(&CronRun {
                id: Uuid::new_v4().to_string(),
                job_id: job_id.clone(),
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                status: "success".to_string(),
                output: None,
                error: None,
            })
            .unwrap();

        assert!(scheduler.delete_job(&job_id).unwrap());
        assert!(scheduler.get_job(&job_id).unwrap().is_none());
        let history = scheduler.get_run_history(&job_id, 10).unwrap();
        assert_eq!(history.len(), 1, "run history must survive delete_job");
        assert_eq!(history[0].status, "success");
    }

    /// The SpawnTool fire path keeps ONE row per run: `record_run`
    /// opens it, `attach_run_output` hangs the task id on it while it
    /// stays `running`, and `finalize_run` closes it. A second
    /// `record_run` with the same id used to append a duplicate row
    /// stuck on "running" forever (2026-08-07 field test, F3).
    #[test]
    fn test_run_row_attach_then_finalize_single_row() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.json");
        let scheduler = CronScheduler::new(&db_path).unwrap();

        let job_id = Uuid::new_v4().to_string();
        let job = CronJob {
            id: job_id.clone(),
            name: "Spawn One Shot".to_string(),
            schedule: ScheduleKind::Every { every_ms: 60000 },
            principal_id: PrincipalId("prin_test_principal".to_string()),
            action: CronJobAction::SpawnTool {
                tool_name: "Bash".to_string(),
                tool_params: serde_json::json!({"command": "true"}),
                wake_on_completion: Some(false),
                timeout_secs: Some(60),
            },
                        delete_after_run: true,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        let run_id = Uuid::new_v4().to_string();
        scheduler
            .record_run(&CronRun {
                id: run_id.clone(),
                job_id: job_id.clone(),
                started_at: Utc::now(),
                finished_at: None,
                status: "running".to_string(),
                output: None,
                error: None,
            })
            .unwrap();

        // Attach the async task id: row stays open, still one row.
        assert!(scheduler
            .attach_run_output(&run_id, Some("task:abc".to_string()), None)
            .unwrap());
        let runs = scheduler.get_run_history(&job_id, 10).unwrap();
        assert_eq!(runs.len(), 1, "attach must not duplicate the row");
        assert_eq!(runs[0].status, "running");
        assert!(runs[0].finished_at.is_none());
        assert_eq!(runs[0].output.as_deref(), Some("task:abc"));

        // A second attach on the still-open row is allowed (id update);
        // after finalize the row is closed and further attaches refuse.
        assert!(scheduler
            .finalize_run(&run_id, "success", Some("done".to_string()), None)
            .unwrap());
        assert!(!scheduler
            .attach_run_output(&run_id, Some("task:xyz".to_string()), None)
            .unwrap());
        assert!(!scheduler
            .finalize_run(&run_id, "success", None, None)
            .unwrap());

        let runs = scheduler.get_run_history(&job_id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert!(runs[0].finished_at.is_some());
        assert_eq!(runs[0].output.as_deref(), Some("done"));
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
            principal_id: PrincipalId("prin_test_principal".to_string()),
            action: CronJobAction::Send {
                message: "Test".to_string(),
                target: None,
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - chrono::Duration::hours(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };

        let future_job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Future Job".to_string(),
            schedule: ScheduleKind::Every { every_ms: 60000 },
            principal_id: PrincipalId("prin_test_principal".to_string()),
            action: CronJobAction::Send {
                message: "Test".to_string(),
                target: None,
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + chrono::Duration::hours(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };

        scheduler.add_job(&past_job).unwrap();
        scheduler.add_job(&future_job).unwrap();

        let due = scheduler.due_jobs(Utc::now()).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "Past Job");
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

    // ------------------------------------------------------------------
    // Retry-budget accounting
    // ------------------------------------------------------------------

    /// Build a minimal `CronJob` for the retry-budget tests. The fields
    /// that don't matter for the assertions are filled with the same
    /// defaults `peko cron add` uses.
    fn make_job(id: &str, max_retries: Option<u32>) -> CronJob {
        CronJob {
            id: id.to_string(),
            name: format!("job-{id}"),
            principal_id: PrincipalId("prin_retry".to_string()),
            schedule: ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::Send {
                message: "x".to_string(),
                target: None,
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries,
        }
    }

    #[test]
    fn test_consecutive_failures_increment() {
        let tmp = TempDir::new().unwrap();
        let scheduler = CronScheduler::new(tmp.path().join("cron.json")).unwrap();
        scheduler.add_job(&make_job("a", None)).unwrap();

        // Two failed runs → consecutive_failures should reach 2.
        scheduler
            .update_job_after_run("a", "failed", Utc::now())
            .unwrap();
        scheduler
            .update_job_after_run("a", "failed", Utc::now())
            .unwrap();
        let job = scheduler.get_job("a").unwrap().unwrap();
        assert_eq!(job.consecutive_failures, 2);
        assert_eq!(job.run_count, 2);
    }

    #[test]
    fn test_consecutive_failures_reset_on_success() {
        let tmp = TempDir::new().unwrap();
        let scheduler = CronScheduler::new(tmp.path().join("cron.json")).unwrap();
        scheduler.add_job(&make_job("b", None)).unwrap();

        // Fail twice → 2; then succeed → 0.
        scheduler
            .update_job_after_run("b", "failed", Utc::now())
            .unwrap();
        scheduler
            .update_job_after_run("b", "failed", Utc::now())
            .unwrap();
        scheduler
            .update_job_after_run("b", "success", Utc::now())
            .unwrap();
        let job = scheduler.get_job("b").unwrap().unwrap();
        assert_eq!(job.consecutive_failures, 0);
    }

    #[test]
    fn test_max_retries_disables_job() {
        let tmp = TempDir::new().unwrap();
        let scheduler = CronScheduler::new(tmp.path().join("cron.json")).unwrap();
        scheduler.add_job(&make_job("c", Some(2))).unwrap();

        // Fail twice → budget exhausted. The engine does the disabling,
        // but we mirror the policy here directly to validate the field
        // shapes survive the round-trip.
        scheduler
            .update_job_after_run("c", "failed", Utc::now())
            .unwrap();
        scheduler
            .update_job_after_run("c", "failed", Utc::now())
            .unwrap();
        let job = scheduler.get_job("c").unwrap().unwrap();
        assert_eq!(job.consecutive_failures, 2);
        assert_eq!(job.max_retries, Some(2));
        // We don't call set_job_enabled here — the engine owns that
        // decision (see `execute_job` in the daemon). This test just
        // covers the field plumbing.
    }

    #[test]
    fn test_set_job_last_status_increments_on_failure() {
        // The `SpawnTool` reconcile path goes through `set_job_last_status`
        // (not `update_job_after_run`). Without that path also bumping
        // `consecutive_failures`, a `SpawnTool` failure would skip the
        // retry budget entirely.
        let tmp = TempDir::new().unwrap();
        let scheduler = CronScheduler::new(tmp.path().join("cron.json")).unwrap();
        scheduler.add_job(&make_job("d", Some(3))).unwrap();

        assert!(scheduler.set_job_last_status("d", "failed").unwrap());
        assert!(scheduler.set_job_last_status("d", "failed").unwrap());
        assert!(scheduler.set_job_last_status("d", "success").unwrap());

        let job = scheduler.get_job("d").unwrap().unwrap();
        assert_eq!(job.consecutive_failures, 0);
        assert_eq!(job.run_count, 0); // set_job_last_status never bumps run_count
    }

    /// Phase 3b (2026-08-15): trunk-targeted Send jobs respect the
    /// keepalive interval floor at the `add_job` funnel (the path the
    /// CLI `--target trunk` and the `CronCreate` tool both flow
    /// through). Sub-minute `Every` intervals are refused; explicit
    /// (`At`, `Cron`) and idle-triggered schedules are unchanged.
    /// Phase 7: the trunk is the DEFAULT Send target, so
    /// `target: None` jobs are floored too.
    #[test]
    fn test_add_job_trunk_interval_floor() {
        let tmp = TempDir::new().unwrap();
        let scheduler = CronScheduler::new(tmp.path().join("cron.json")).unwrap();

        let mut job = make_job("trunk-fast", None);
        job.action = CronJobAction::Send {
            message: "keepalive".to_string(),
            target: Some("trunk".to_string()),
        };

        // Every{30s} + trunk → refused with the floor in the message.
        job.schedule = ScheduleKind::Every { every_ms: 30_000 };
        let err = scheduler.add_job(&job).unwrap_err();
        assert!(err.to_string().contains("every_ms >= 60000"), "got: {err}");
        assert!(scheduler.get_job("trunk-fast").unwrap().is_none());

        // Every{300s} + trunk → accepted.
        job.schedule = ScheduleKind::Every { every_ms: 300_000 };
        scheduler.add_job(&job).unwrap();

        // Default-target (None) Send + Every{30s} → refused: Phase 7
        // made the trunk the default destination, so the floor holds.
        let mut fast = make_job("conv-fast", None);
        fast.schedule = ScheduleKind::Every { every_ms: 30_000 };
        let err = scheduler.add_job(&fast).unwrap_err();
        assert!(err.to_string().contains("every_ms >= 60000"), "got: {err}");

        // trunk + At (future) and trunk + Cron → exempt, accepted.
        let mut at = make_job("trunk-at", None);
        at.action = CronJobAction::Send {
            message: "keepalive".to_string(),
            target: Some("trunk".to_string()),
        };
        at.schedule = ScheduleKind::At {
            at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
        };
        scheduler.add_job(&at).unwrap();

        let mut cron = make_job("trunk-cron", None);
        cron.action = CronJobAction::Send {
            message: "keepalive".to_string(),
            target: Some("trunk".to_string()),
        };
        cron.schedule = ScheduleKind::Cron {
            expr: "* * * * *".to_string(),
            tz: None,
        };
        scheduler.add_job(&cron).unwrap();
    }

    /// A past `at` must be rejected at creation, not silently parked on
    /// the 100-year sentinel (2026-08-07 field test, N2b).
    #[test]
    fn test_add_job_rejects_past_at() {
        let tmp = TempDir::new().unwrap();
        let scheduler = CronScheduler::new(tmp.path().join("cron.json")).unwrap();
        let mut job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: "Past One Shot".to_string(),
            schedule: ScheduleKind::At {
                at: (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339(),
            },
            principal_id: PrincipalId("prin_test_principal".to_string()),
            action: CronJobAction::Send {
                message: "Test".to_string(),
                target: None,
            },
                        delete_after_run: true,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        let err = scheduler.add_job(&job).unwrap_err();
        assert!(err.to_string().contains("in the past"), "got: {err}");

        // Future timestamps still accepted.
        job.schedule = ScheduleKind::At {
            at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
        };
        scheduler.add_job(&job).unwrap();
    }

    #[test]
    fn test_at_job_single_fire_terminates() {
        // An `At` job whose due time is in the past should produce a
        // far-future next_run so the engine doesn't re-fire it on
        // every poll. This is the structural fix for the retry-forever
        // bug — the retry budget is the second line of defense.
        let past = Utc::now() - chrono::Duration::hours(1);
        let past_rfc = past.to_rfc3339();
        let schedule = ScheduleKind::At { at: past_rfc };
        let next = calculate_next_run(&schedule, Utc::now()).unwrap();
        let since_now = next - Utc::now();
        // Far-future sentinel: at least 50 years out.
        assert!(since_now > chrono::Duration::days(365 * 50));
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
                principal_id: PrincipalId("prin_test_principal".to_string()),
                schedule: ScheduleKind::Every { every_ms: 60000 },
                action: CronJobAction::Send {
                    message: "Hello".to_string(),
                    target: None,
                },
                                delete_after_run: false,
                enabled: true,
                created_at: Utc::now(),
                next_run: Utc::now(),
                last_run: None,
                last_status: None,
                run_count: 42,
                consecutive_failures: 0,
                max_retries: None,
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
