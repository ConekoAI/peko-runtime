//! Cron scheduler for periodic task execution
//!
//! Stores cron jobs in SQLite and provides scheduling functionality.
//! Supports both main session (system event) and isolated execution modes.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use tracing::info;

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
}

impl ScheduleKind {
    /// Get display name for the schedule
    pub fn display(&self) -> String {
        match self {
            ScheduleKind::At { at } => format!("at {}", at),
            ScheduleKind::Every { every_ms } => {
                let secs = every_ms / 1000;
                if secs < 60 {
                    format!("every {}s", secs)
                } else if secs < 3600 {
                    format!("every {}m", secs / 60)
                } else {
                    format!("every {}h", secs / 3600)
                }
            }
            ScheduleKind::Cron { expr, tz } => {
                if let Some(tz) = tz {
                    format!("cron '{}' ({})", expr, tz)
                } else {
                    format!("cron '{}'", expr)
                }
            }
        }
    }
}

/// Execution target for cron jobs
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    /// Run in main agent session (system event)
    Main,
    /// Run in isolated session (dedicated agent turn)
    Isolated,
}

impl Default for ExecutionTarget {
    fn default() -> Self {
        ExecutionTarget::Main
    }
}

/// Delivery configuration for job results
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// No delivery, silent execution
    None,
    /// Announce results to channel
    Announce {
        channel: Option<String>,
        to: Option<String>,
        best_effort: bool,
    },
}

impl Default for DeliveryMode {
    fn default() -> Self {
        DeliveryMode::None
    }
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

    /// Initialize the database schema
    fn init_db(&self) -> Result<()> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create cron directory: {}", parent.display())
            })?;
        }

        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("Failed to open cron DB: {}", self.db_path.display()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cron_jobs (
                id               TEXT PRIMARY KEY,
                name             TEXT NOT NULL,
                schedule_kind    TEXT NOT NULL,
                schedule_data    TEXT NOT NULL,
                target           TEXT NOT NULL DEFAULT 'main',
                agent_id         TEXT,
                message          TEXT NOT NULL,
                delivery_mode    TEXT NOT NULL DEFAULT 'none',
                delivery_data    TEXT,
                delete_after_run INTEGER NOT NULL DEFAULT 0,
                enabled          INTEGER NOT NULL DEFAULT 1,
                created_at       TEXT NOT NULL,
                next_run         TEXT NOT NULL,
                last_run         TEXT,
                last_status      TEXT,
                run_count        INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_cron_jobs_next_run ON cron_jobs(next_run);
            CREATE INDEX IF NOT EXISTS idx_cron_jobs_enabled ON cron_jobs(enabled);

            CREATE TABLE IF NOT EXISTS cron_runs (
                id           TEXT PRIMARY KEY,
                job_id       TEXT NOT NULL,
                started_at   TEXT NOT NULL,
                finished_at  TEXT,
                status       TEXT NOT NULL,
                output       TEXT,
                error        TEXT,
                FOREIGN KEY (job_id) REFERENCES cron_jobs(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_cron_runs_job_id ON cron_runs(job_id);",
        )
        .context("Failed to initialize cron schema")?;

        Ok(())
    }

    /// Add a new cron job
    pub fn add_job(&self, job: &CronJob) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        
        let schedule_data = serde_json::to_string(&job.schedule)?;
        let delivery_data = match &job.delivery {
            DeliveryMode::None => None,
            DeliveryMode::Announce { .. } => Some(serde_json::to_string(&job.delivery)?),
        };

        conn.execute(
            "INSERT INTO cron_jobs (
                id, name, schedule_kind, schedule_data, target, agent_id,
                message, delivery_mode, delivery_data, delete_after_run,
                enabled, created_at, next_run, run_count
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                &job.id,
                &job.name,
                schedule_kind_str(&job.schedule),
                schedule_data,
                target_str(&job.target),
                &job.agent_id,
                &job.message,
                delivery_mode_str(&job.delivery),
                delivery_data,
                job.delete_after_run as i32,
                job.enabled as i32,
                job.created_at.to_rfc3339(),
                job.next_run.to_rfc3339(),
                job.run_count as i32,
            ],
        )
        .context("Failed to insert cron job")?;

        info!("Added cron job {}: '{}' with schedule {}", 
            job.id, job.name, job.schedule.display());

        Ok(())
    }

    /// Get a job by ID
    pub fn get_job(&self, job_id: &str) -> Result<Option<CronJob>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, name, schedule_kind, schedule_data, target, agent_id,
                    message, delivery_mode, delivery_data, delete_after_run,
                    enabled, created_at, next_run, last_run, last_status, run_count
             FROM cron_jobs WHERE id = ?1"
        )?;

        let job = stmt.query_row(params![job_id], |row| {
            parse_job_from_row(row)
        }).optional()?;

        Ok(job)
    }

    /// List all cron jobs
    pub fn list_jobs(&self, include_disabled: bool) -> Result<Vec<CronJob>> {
        let conn = Connection::open(&self.db_path)?;
        let sql = if include_disabled {
            "SELECT id, name, schedule_kind, schedule_data, target, agent_id,
                    message, delivery_mode, delivery_data, delete_after_run,
                    enabled, created_at, next_run, last_run, last_status, run_count
             FROM cron_jobs ORDER BY next_run ASC"
        } else {
            "SELECT id, name, schedule_kind, schedule_data, target, agent_id,
                    message, delivery_mode, delivery_data, delete_after_run,
                    enabled, created_at, next_run, last_run, last_status, run_count
             FROM cron_jobs WHERE enabled = 1 ORDER BY next_run ASC"
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| parse_job_from_row(row))?;

        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row?);
        }
        Ok(jobs)
    }

    /// Get jobs that are due to run
    pub fn due_jobs(&self, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, name, schedule_kind, schedule_data, target, agent_id,
                    message, delivery_mode, delivery_data, delete_after_run,
                    enabled, created_at, next_run, last_run, last_status, run_count
             FROM cron_jobs 
             WHERE enabled = 1 AND next_run <= ?1 
             ORDER BY next_run ASC"
        )?;

        let rows = stmt.query_map(params![now.to_rfc3339()], |row| parse_job_from_row(row))?;

        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row?);
        }
        Ok(jobs)
    }

    /// Update job after execution
    pub fn update_job_after_run(&self, job_id: &str, status: &str, next_run: DateTime<Utc>) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        conn.execute(
            "UPDATE cron_jobs 
             SET last_run = ?1, last_status = ?2, next_run = ?3, run_count = run_count + 1
             WHERE id = ?4",
            params![Utc::now().to_rfc3339(), status, next_run.to_rfc3339(), job_id],
        )?;
        Ok(())
    }

    /// Delete a job
    pub fn delete_job(&self, job_id: &str) -> Result<bool> {
        let conn = Connection::open(&self.db_path)?;
        let changed = conn.execute("DELETE FROM cron_jobs WHERE id = ?1", params![job_id])?;
        if changed > 0 {
            info!("Deleted cron job {}", job_id);
        }
        Ok(changed > 0)
    }

    /// Enable/disable a job
    pub fn set_job_enabled(&self, job_id: &str, enabled: bool) -> Result<bool> {
        let conn = Connection::open(&self.db_path)?;
        let changed = conn.execute(
            "UPDATE cron_jobs SET enabled = ?1 WHERE id = ?2",
            params![enabled as i32, job_id],
        )?;
        Ok(changed > 0)
    }

    /// Record a job run
    pub fn record_run(&self, run: &CronRun) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        conn.execute(
            "INSERT INTO cron_runs (id, job_id, started_at, finished_at, status, output, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &run.id,
                &run.job_id,
                run.started_at.to_rfc3339(),
                run.finished_at.map(|t| t.to_rfc3339()),
                &run.status,
                &run.output,
                &run.error,
            ],
        )?;
        Ok(())
    }

    /// Get run history for a job
    pub fn get_run_history(&self, job_id: &str, limit: usize) -> Result<Vec<CronRun>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, job_id, started_at, finished_at, status, output, error
             FROM cron_runs 
             WHERE job_id = ?1 
             ORDER BY started_at DESC 
             LIMIT ?2"
        )?;

        let rows = stmt.query_map(params![job_id, limit as i64], |row| {
            let started_at_raw: String = row.get(2)?;
            let finished_at_raw: Option<String> = row.get(3)?;
            
            let started_at = DateTime::parse_from_rfc3339(&started_at_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    2, rusqlite::types::Type::Text, Box::new(e)
                ))?;
            
            let finished_at = match finished_at_raw {
                Some(s) => Some(DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                        3, rusqlite::types::Type::Text, Box::new(e)
                    ))?),
                None => None,
            };
            
            Ok(CronRun {
                id: row.get(0)?,
                job_id: row.get(1)?,
                started_at,
                finished_at,
                status: row.get(4)?,
                output: row.get(5)?,
                error: row.get(6)?,
            })
        })?;

        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    /// Calculate next run time for a schedule
    pub fn calculate_next_run(&self, schedule: &ScheduleKind, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
        match schedule {
            ScheduleKind::At { at } => {
                let dt = DateTime::parse_from_rfc3339(at)
                    .map_err(|e| anyhow::anyhow!("Invalid timestamp: {}", e))?;
                Ok(dt.with_timezone(&Utc))
            }
            ScheduleKind::Every { every_ms } => {
                Ok(after + chrono::Duration::milliseconds(*every_ms as i64))
            }
            ScheduleKind::Cron { expr, tz } => {
                let schedule = Schedule::from_str(expr)
                    .map_err(|e| anyhow::anyhow!("Invalid cron expression: {}", e))?;
                
                if let Some(tz_str) = tz {
                    let tz: chrono_tz::Tz = tz_str.parse()
                        .map_err(|e| anyhow::anyhow!("Invalid timezone: {}", e))?;
                    let local_after = after.with_timezone(&tz);
                    if let Some(next) = schedule.after(&local_after).next() {
                        Ok(next.with_timezone(&Utc))
                    } else {
                        Err(anyhow::anyhow!("No next occurrence found"))
                    }
                } else {
                    if let Some(next) = schedule.after(&after).next() {
                        Ok(next)
                    } else {
                        Err(anyhow::anyhow!("No next occurrence found"))
                    }
                }
            }
        }
    }
}

// Helper functions

fn parse_job_from_row(row: &rusqlite::Row) -> rusqlite::Result<CronJob> {
    let schedule_kind: String = row.get(2)?;
    let schedule_data: String = row.get(3)?;
    let delivery_mode: String = row.get(7)?;
    let delivery_data: Option<String> = row.get(8)?;

    let schedule = match schedule_kind.as_str() {
        "at" => serde_json::from_str(&schedule_data).unwrap_or(ScheduleKind::Every { every_ms: 3600000 }),
        "every" => serde_json::from_str(&schedule_data).unwrap_or(ScheduleKind::Every { every_ms: 3600000 }),
        "cron" => serde_json::from_str(&schedule_data).unwrap_or(ScheduleKind::Every { every_ms: 3600000 }),
        _ => ScheduleKind::Every { every_ms: 3600000 },
    };

    let delivery = if delivery_mode == "announce" {
        delivery_data.and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or(DeliveryMode::None)
    } else {
        DeliveryMode::None
    };

    Ok(CronJob {
        id: row.get(0)?,
        name: row.get(1)?,
        schedule,
        target: match row.get::<_, String>(4)?.as_str() {
            "isolated" => ExecutionTarget::Isolated,
            _ => ExecutionTarget::Main,
        },
        agent_id: row.get(5)?,
        message: row.get(6)?,
        delivery,
        delete_after_run: row.get::<_, i32>(9)? != 0,
        enabled: row.get::<_, i32>(10)? != 0,
        created_at: {
            let s: String = row.get(11)?;
            DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    11, rusqlite::types::Type::Text, Box::new(e)
                ))?
        },
        next_run: {
            let s: String = row.get(12)?;
            DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    12, rusqlite::types::Type::Text, Box::new(e)
                ))?
        },
        last_run: {
            let opt: Option<String> = row.get(13)?;
            match opt {
                Some(s) => Some(DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                        13, rusqlite::types::Type::Text, Box::new(e)
                    ))?),
                None => None,
            }
        },
        last_status: row.get(14)?,
        run_count: row.get::<_, i32>(15)? as u32,
    })
}

fn schedule_kind_str(schedule: &ScheduleKind) -> &'static str {
    match schedule {
        ScheduleKind::At { .. } => "at",
        ScheduleKind::Every { .. } => "every",
        ScheduleKind::Cron { .. } => "cron",
    }
}

fn target_str(target: &ExecutionTarget) -> &'static str {
    match target {
        ExecutionTarget::Main => "main",
        ExecutionTarget::Isolated => "isolated",
    }
}

fn delivery_mode_str(delivery: &DeliveryMode) -> &'static str {
    match delivery {
        DeliveryMode::None => "none",
        DeliveryMode::Announce { .. } => "announce",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_add_and_list_job() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.db");
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
        let db_path = tmp.path().join("cron.db");
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
}
