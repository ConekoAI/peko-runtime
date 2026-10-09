//! Test support: a [`CronRuntime`] backed by real [`CronScheduler`] files.
//!
//! Mirrors `DaemonCronAdapter`'s storage layout — one schedule file per
//! principal, job ids resolved across every principal's file — without a
//! `PrincipalManager` or cron engine. Because it uses the production
//! scheduler, retention rules (run history outliving a reaped one-shot job,
//! re-enable resetting failures) behave exactly as they do in the daemon.
//!
//! Enabled for this crate's tests and, via the `test-support` feature, for
//! downstream test builds.

use crate::tools::{CronJob, CronRuntime};
use crate::{CronRun, CronScheduler};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use peko_subject::PrincipalId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// File-backed cron runtime rooted in a caller-owned directory.
pub struct FileCronRuntime {
    root: PathBuf,
    /// Principal → schedule file, populated as jobs are added so lookups
    /// scan the same set of files the daemon would.
    schedules: Mutex<BTreeMap<String, PathBuf>>,
}

impl FileCronRuntime {
    /// Store schedule files under `root` (typically a tempdir).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            schedules: Mutex::new(BTreeMap::new()),
        }
    }

    /// The scheduler holding `principal`'s jobs and runs.
    pub fn scheduler(&self, principal: &PrincipalId) -> CronScheduler {
        let path = self.schedule_path(principal);
        CronScheduler::new(path).expect("open test cron schedule")
    }

    /// Every live job across all principals.
    pub fn jobs(&self) -> Vec<CronJob> {
        let schedules = self.schedules.lock().unwrap().clone();
        schedules
            .values()
            .flat_map(|path| {
                CronScheduler::new(path)
                    .and_then(|s| s.list_jobs(true))
                    .expect("read test cron schedule")
            })
            .collect()
    }

    /// Simulate a one-shot fire: record a finished run, then reap the job
    /// the way the engine does for `delete_after_run` jobs.
    pub fn fire_and_reap(&self, job_id: &str, status: &str) -> Result<String> {
        let (_, path) = self
            .owner(job_id)?
            .ok_or_else(|| anyhow!("Job {job_id} not found"))?;
        let scheduler = CronScheduler::new(&path)?;
        let run_id = Self::record(&scheduler, job_id, status)?;
        scheduler.delete_job(job_id)?;
        Ok(run_id)
    }

    fn schedule_path(&self, principal: &PrincipalId) -> PathBuf {
        let mut schedules = self.schedules.lock().unwrap();
        schedules
            .entry(principal.0.clone())
            .or_insert_with(|| schedule_file(&self.root, &principal.0))
            .clone()
    }

    /// The principal whose schedule holds `job_id` as a live job or in its
    /// run history, matching `DaemonCronAdapter::resolve_owner`.
    fn owner(&self, job_id: &str) -> Result<Option<(String, PathBuf)>> {
        let schedules = self.schedules.lock().unwrap().clone();
        for (principal, path) in schedules {
            let scheduler = CronScheduler::new(&path)?;
            if scheduler.get_job(job_id)?.is_some()
                || !scheduler.get_run_history(job_id, 1)?.is_empty()
            {
                return Ok(Some((principal, path)));
            }
        }
        Ok(None)
    }

    fn record(scheduler: &CronScheduler, job_id: &str, status: &str) -> Result<String> {
        let now = Utc::now();
        let run = CronRun {
            id: format!("run_{}", uuid::Uuid::new_v4().simple()),
            job_id: job_id.to_string(),
            started_at: now,
            finished_at: (status != "running").then_some(now),
            status: status.to_string(),
            output: None,
            error: None,
        };
        scheduler.record_run(&run)?;
        Ok(run.id)
    }
}

fn schedule_file(root: &Path, principal: &str) -> PathBuf {
    let name: String = principal
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    root.join(format!("{name}.cron.json"))
}

#[async_trait]
impl CronRuntime for FileCronRuntime {
    async fn add_job(&self, job: CronJob) -> Result<String> {
        self.scheduler(&job.principal_id).add_job(&job)?;
        Ok(job.id)
    }

    async fn delete_job(&self, job_id: &str) -> Result<()> {
        let (_, path) = self
            .owner(job_id)?
            .ok_or_else(|| anyhow!("Job {job_id} not found"))?;
        CronScheduler::new(&path)?.delete_job(job_id)?;
        Ok(())
    }

    async fn list_jobs(&self) -> Result<Vec<CronJob>> {
        Ok(self.jobs())
    }

    async fn update_job(
        &self,
        job_id: &str,
        enabled: Option<bool>,
        wake_on_completion: Option<bool>,
    ) -> Result<()> {
        let (_, path) = self
            .owner(job_id)?
            .ok_or_else(|| anyhow!("Job {job_id} not found"))?;
        if !CronScheduler::new(&path)?.update_job_fields(job_id, enabled, wake_on_completion)? {
            return Err(anyhow!("Job {job_id} not found"));
        }
        Ok(())
    }

    /// Records a `running` run, coalescing with one already in flight. No
    /// engine fires the job; tests finish runs through the scheduler.
    async fn trigger_job(&self, job_id: &str) -> Result<String> {
        let (_, path) = self
            .owner(job_id)?
            .ok_or_else(|| anyhow!("Job {job_id} not found"))?;
        let scheduler = CronScheduler::new(&path)?;
        if scheduler.get_job(job_id)?.is_none() {
            return Err(anyhow!("Job {job_id} not found"));
        }
        if let Some(run) = scheduler
            .list_running_runs()?
            .into_iter()
            .find(|run| run.job_id == job_id)
        {
            return Ok(run.id);
        }
        Self::record(&scheduler, job_id, "running")
    }

    async fn run_history(&self, job_id: &str, limit: usize) -> Result<Vec<CronRun>> {
        let (_, path) = self
            .owner(job_id)?
            .ok_or_else(|| anyhow!("Job {job_id} not found"))?;
        CronScheduler::new(&path)?.get_run_history(job_id, limit)
    }

    async fn owns_job_history(&self, principal_id: &PrincipalId, job_id: &str) -> Result<bool> {
        let scheduler = self.scheduler(principal_id);
        Ok(scheduler.get_job(job_id)?.is_some()
            || !scheduler.get_run_history(job_id, 1)?.is_empty())
    }
}
