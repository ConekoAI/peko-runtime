//! `DaemonCronAdapter` — implements `peko_cron::CronRuntime` for the daemon.
//!
//! The cron tools in `peko_cron::tools` do not import daemon state.
//! They speak to a runtime port trait ([`peko_cron::CronRuntime`]),
//! and the daemon side implements that trait via this adapter.
//!
//! Construct at daemon startup with the shared `PrincipalManager` and
//! `PathResolver`, then install via
//! [`DaemonCronAdapter::install_as_global`]. Tools read the global
//! via [`peko_cron::global_runtime`] at execute time.
//!
//! 2026-08-25: cron is now an internal principal tool (like Bash,
//! Session). The legacy `CronList`/`CronAdd`/... IPC variants and the
//! `peko cron` CLI were deleted; this adapter is the only cron
//! read/write surface in the daemon. Per-principal `tool:Cron*`
//! grants gate tool access (F37 funnel); this adapter itself does
//! not re-check caps because the funnel already did.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use peko_cron::{set_global_runtime, CronJob, CronRuntime, CronScheduler};
use tracing::warn;

use crate::common::paths::PathResolver;
use crate::principal::manager::PrincipalManager;

/// `CronRuntime` impl that reads/writes the per-principal
/// `<resolver>.cron_schedule(name)` schedule file directly. A single
/// adapter represents the daemon-side impl for all cron tools.
pub struct DaemonCronAdapter {
    path_resolver: PathResolver,
    principal_manager: Arc<PrincipalManager>,
}

impl DaemonCronAdapter {
    /// Build an adapter bound to the daemon's principal manager and
    /// typed resolver. Install once via
    /// [`DaemonCronAdapter::install_as_global`].
    pub fn new(path_resolver: PathResolver, principal_manager: Arc<PrincipalManager>) -> Self {
        Self {
            path_resolver,
            principal_manager,
        }
    }

    /// Convenience: install this adapter as the global runtime.
    /// Idempotent for repeated calls with the same adapter.
    pub fn install_as_global(self: Arc<Self>) {
        set_global_runtime(self.clone());
    }

    /// Enumerate the loaded principals (best-effort).
    async fn all_principal_names(&self) -> Vec<String> {
        let principals = self.principal_manager.list_all().await;
        let mut names = Vec::with_capacity(principals.len());
        for p in principals {
            names.push(p.name().await);
        }
        names
    }

    /// Resolve the loaded principal that owns `job_id`, returning its
    /// name and schedule file path. Falls back to run-history lookup
    /// for one-shot (`delete_after_run=true`) jobs that fire once and
    /// then self-delete — the run record survives the deletion so we
    /// can still resolve owner for `peko cron history` (2026-08-07
    /// field test, Finding 4).
    async fn resolve_owner(&self, job_id: &str) -> Option<(String, PathBuf)> {
        for name in self.all_principal_names().await {
            let path = self.path_resolver.cron_schedule(&name);
            let scheduler = match CronScheduler::new(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if let Ok(Some(_)) = scheduler.get_job(job_id) {
                return Some((name, path));
            }
            if let Ok(runs) = scheduler.get_run_history(job_id, 1) {
                if !runs.is_empty() {
                    return Some((name, path));
                }
            }
        }
        None
    }
}

#[async_trait]
impl CronRuntime for DaemonCronAdapter {
    async fn add_job(&self, job: CronJob) -> Result<String> {
        let job_id = job.id.clone();
        // Resolve the principal that owns this job by wire `PrincipalId`
        // (DID) — the cron runtime is global and the cron tool runs
        // outside the implicit principal context, so we look up by
        // `job.principal_id` and write to the matching per-principal
        // schedule file.
        let principal =
            crate::daemon::cron_engine::resolve_principal(&self.principal_manager, &job.principal_id)
                .await;
        let Some(p) = principal else {
            return Err(anyhow::anyhow!(
                "Principal '{}' is not loaded",
                job.principal_id.0
            ));
        };
        let principal_name = p.name().await;
        let path = self.path_resolver.cron_schedule(&principal_name);
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        scheduler
            .add_job(&job)
            .map_err(|e| anyhow::anyhow!("Failed to add job: {e}"))?;
        Ok(job_id)
    }

    async fn delete_job(&self, job_id: &str) -> Result<()> {
        let (name, path) = self
            .resolve_owner(job_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Job {job_id} not found"))?;
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        let removed = scheduler
            .delete_job(job_id)
            .map_err(|e| anyhow::anyhow!("Failed to remove job: {e}"))?;
        if !removed {
            warn!("cron remove: job {job_id} not found under principal {name}");
        }
        Ok(())
    }

    async fn list_jobs(&self) -> Result<Vec<CronJob>> {
        // `include_disabled=true` so the calling tool can do its own
        // filtering (e.g. by principal). The port trait pushes that
        // policy up to the tool.
        let mut jobs: Vec<CronJob> = Vec::new();
        let mut first_err: Option<String> = None;
        for name in self.all_principal_names().await {
            let path = self.path_resolver.cron_schedule(&name);
            match CronScheduler::new(&path) {
                Ok(scheduler) => match scheduler.list_jobs(true) {
                    Ok(mut j) => jobs.append(&mut j),
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(format!("{name}: {e}"));
                        }
                    }
                },
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(format!("{name}: {e}"));
                    }
                }
            }
        }
        match first_err {
            Some(e) => Err(anyhow::anyhow!("Cron DB error: {e}")),
            None => Ok(jobs),
        }
    }
}