//! Cron scheduler for periodic task execution
//!
//! Stores cron jobs in `SQLite` and provides scheduling functionality.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::str::FromStr;
use tracing::{debug, info};
use uuid::Uuid;

/// A scheduled cron job
#[derive(Debug, Clone)]
pub struct CronJob {
    pub id: String,
    pub expression: String,
    pub command: String,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
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
                id          TEXT PRIMARY KEY,
                expression  TEXT NOT NULL,
                command     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                next_run    TEXT NOT NULL,
                last_run    TEXT,
                last_status TEXT,
                last_output TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_cron_jobs_next_run ON cron_jobs(next_run);",
        )
        .context("Failed to initialize cron schema")?;

        Ok(())
    }

    /// Add a new cron job
    pub fn add_job(&self, expression: &str, command: &str) -> Result<CronJob> {
        let now = Utc::now();
        let next_run = next_run_for(expression, now)?;
        let id = Uuid::new_v4().to_string();

        let conn = Connection::open(&self.db_path)?;
        conn.execute(
            "INSERT INTO cron_jobs (id, expression, command, created_at, next_run)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &id,
                expression,
                command,
                now.to_rfc3339(),
                next_run.to_rfc3339()
            ],
        )
        .context("Failed to insert cron job")?;

        info!("Added cron job {}: '{}' at {}", id, command, expression);

        Ok(CronJob {
            id,
            expression: expression.to_string(),
            command: command.to_string(),
            next_run,
            last_run: None,
            last_status: None,
        })
    }

    /// List all cron jobs
    pub fn list_jobs(&self) -> Result<Vec<CronJob>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, next_run, last_run, last_status
             FROM cron_jobs ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let next_run_raw: String = row.get(3)?;
            let last_run_raw: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                next_run_raw,
                last_run_raw,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;

        let mut jobs = Vec::new();
        for row in rows {
            let (id, expression, command, next_run_raw, last_run_raw, last_status) = row?;
            jobs.push(CronJob {
                id,
                expression,
                command,
                next_run: parse_rfc3339(&next_run_raw)?,
                last_run: last_run_raw.map(|r| parse_rfc3339(&r)).transpose()?,
                last_status,
            });
        }
        Ok(jobs)
    }

    /// Get jobs that are due to run
    pub fn due_jobs(&self, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, next_run, last_run, last_status
             FROM cron_jobs WHERE next_run <= ?1 ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map(params![now.to_rfc3339()], |row| {
            let next_run_raw: String = row.get(3)?;
            let last_run_raw: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                next_run_raw,
                last_run_raw,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;

        let mut jobs = Vec::new();
        for row in rows {
            let (id, expression, command, next_run_raw, last_run_raw, last_status) = row?;
            jobs.push(CronJob {
                id,
                expression,
                command,
                next_run: parse_rfc3339(&next_run_raw)?,
                last_run: last_run_raw.map(|r| parse_rfc3339(&r)).transpose()?,
                last_status,
            });
        }
        Ok(jobs)
    }

    /// Remove a cron job by ID
    pub fn remove_job(&self, id: &str) -> Result<bool> {
        let conn = Connection::open(&self.db_path)?;
        let changed = conn.execute("DELETE FROM cron_jobs WHERE id = ?1", params![id])?;

        if changed > 0 {
            info!("Removed cron job {}", id);
            Ok(true)
        } else {
            debug!("Cron job {} not found for removal", id);
            Ok(false)
        }
    }

    /// Update job after execution
    pub fn update_after_run(&self, job_id: &str, success: bool, output: &str) -> Result<()> {
        let now = Utc::now();
        let status = if success { "ok" } else { "error" };

        // Get the job to recalculate next run
        let job = self
            .list_jobs()?
            .into_iter()
            .find(|j| j.id == job_id)
            .context("Job not found")?;

        let next_run = next_run_for(&job.expression, now)?;

        let conn = Connection::open(&self.db_path)?;
        conn.execute(
            "UPDATE cron_jobs
             SET next_run = ?1, last_run = ?2, last_status = ?3, last_output = ?4
             WHERE id = ?5",
            params![
                next_run.to_rfc3339(),
                now.to_rfc3339(),
                status,
                output,
                job_id
            ],
        )
        .context("Failed to update cron job run state")?;

        Ok(())
    }
}

/// Calculate the next run time for a cron expression
fn next_run_for(expression: &str, from: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let normalized = normalize_expression(expression)?;
    let schedule = Schedule::from_str(&normalized)
        .with_context(|| format!("Invalid cron expression: {expression}"))?;
    schedule
        .after(&from)
        .next()
        .ok_or_else(|| anyhow::anyhow!("No future occurrence for expression: {expression}"))
}

/// Normalize 5-field cron to 6-field (with seconds)
fn normalize_expression(expression: &str) -> Result<String> {
    let expression = expression.trim();
    let field_count = expression.split_whitespace().count();

    match field_count {
        // Standard crontab syntax: minute hour day month weekday
        5 => Ok(format!("0 {expression}")),
        // Already has seconds (6 fields) or seconds + year (7 fields)
        6 | 7 => Ok(expression.to_string()),
        _ => anyhow::bail!(
            "Invalid cron expression: {expression} (expected 5, 6, or 7 fields, got {field_count})"
        ),
    }
}

/// Parse RFC3339 timestamp
fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("Invalid RFC3339 timestamp: {raw}"))?;
    Ok(parsed.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    fn test_scheduler() -> (CronScheduler, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.db");
        let scheduler = CronScheduler::new(&db_path).unwrap();
        (scheduler, tmp)
    }

    #[test]
    fn test_add_job() {
        let (scheduler, _tmp) = test_scheduler();
        let job = scheduler.add_job("*/5 * * * *", "echo ok").unwrap();
        assert_eq!(job.expression, "*/5 * * * *");
        assert_eq!(job.command, "echo ok");
    }

    #[test]
    fn test_add_job_invalid_expression() {
        let (scheduler, _tmp) = test_scheduler();
        let err = scheduler.add_job("* * * *", "echo bad").unwrap_err();
        assert!(err.to_string().contains("expected 5, 6, or 7 fields"));
    }

    #[test]
    fn test_list_jobs() {
        let (scheduler, _tmp) = test_scheduler();
        scheduler.add_job("*/10 * * * *", "echo test").unwrap();

        let jobs = scheduler.list_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].command, "echo test");
    }

    #[test]
    fn test_due_jobs() {
        let (scheduler, _tmp) = test_scheduler();
        scheduler.add_job("* * * * *", "echo due").unwrap();

        // New job shouldn't be due immediately
        let due_now = scheduler.due_jobs(Utc::now()).unwrap();
        assert!(due_now.is_empty(), "new job should not be due immediately");

        // Should be due in far future
        let far_future = Utc::now() + ChronoDuration::days(365);
        let due_future = scheduler.due_jobs(far_future).unwrap();
        assert_eq!(due_future.len(), 1);
    }

    #[test]
    fn test_remove_job() {
        let (scheduler, _tmp) = test_scheduler();
        let job = scheduler.add_job("*/15 * * * *", "echo remove").unwrap();

        assert!(scheduler.remove_job(&job.id).unwrap());
        assert!(!scheduler.remove_job(&job.id).unwrap());
        assert!(scheduler.list_jobs().unwrap().is_empty());
    }

    #[test]
    fn test_update_after_run() {
        let (scheduler, _tmp) = test_scheduler();
        let job = scheduler.add_job("*/15 * * * *", "echo run").unwrap();

        scheduler
            .update_after_run(&job.id, false, "failed output")
            .unwrap();

        let jobs = scheduler.list_jobs().unwrap();
        let stored = jobs.iter().find(|j| j.id == job.id).unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert!(stored.last_run.is_some());
    }
}
