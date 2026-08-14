//! In-process cron operations — the single implementation shared by the
//! IPC cron handler (`crate::ipc::handlers::cron`, thin packet wrappers)
//! and the daemon-local `CronRuntime` adapter
//! (`crate::daemon::cron_runtime::DaemonCronAdapter`).
//!
//! 2026-08-07 field test, F1b: the adapter used to loop back over the
//! daemon's own Unix socket via `DaemonClient` — an entire failure
//! class (auth envelope, datagram routing, receiver lifecycle) for what
//! is a function call in the same process. A latent receiver bug
//! (receiver task exited after 60s idle) made every conversational
//! `CronCreate`/`CronList` hang for exactly 60s and then fail, while
//! the job itself was silently added — duplicate jobs, bricked UX.
//! The adapter now calls these ops directly; the IPC handler keeps the
//! same gates by calling them too, so the capability check stays
//! single-sourced.

use std::path::PathBuf;
use std::sync::Arc;

use peko_cron::{CronJob, CronScheduler};
use peko_extension_api::Capabilities;
use tracing::warn;

use crate::common::authority::RuntimeAuthority;
use crate::common::paths::PathResolver;
use crate::principal::manager::PrincipalManager;

/// Discriminator for [`CronOps::authorize`] — picks between the
/// schedule-write gate (Add/Remove/Run) and the history gate (History).
///
/// **Read-via-write-cap invariant:** `CronOp::History` is gated by
/// `principal:write_cron_history` because there is no separate read cap
/// for cron history. The capability string is reused deliberately
/// (see PR #339); the gate key is `local_cron_history_gate_for_name`.
/// If a separate read cap is ever introduced, split the gate AND the
/// starter-bundle grants at the same time.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CronOp {
    Mutate,
    History,
}

/// Narrow bundle of daemon state the cron ops need. Cheap to clone
/// (Arc/PathResolver are cheap); handlers build one per request.
pub(crate) struct CronOps {
    path_resolver: PathResolver,
    principal_manager: Arc<PrincipalManager>,
    authority: Arc<RuntimeAuthority>,
}

impl CronOps {
    pub(crate) fn new(
        path_resolver: PathResolver,
        principal_manager: Arc<PrincipalManager>,
        authority: Arc<RuntimeAuthority>,
    ) -> Self {
        Self {
            path_resolver,
            principal_manager,
            authority,
        }
    }

    /// Add a job: resolve the owner (stable DID or internal id), run the
    /// owner-cap schedule-write gate, then append to the owner's
    /// per-principal schedule file. Returns the job id on success.
    ///
    /// The actor projection is the daemon's runtime authority
    /// (`Subject::Public`): the peer-as-CLI is a UI driver; the actual
    /// write is the daemon acting as the principal's runtime. The
    /// principal's own caps are the only gate.
    pub(crate) async fn add_job(&self, job: CronJob) -> Result<String, String> {
        let principal =
            crate::daemon::cron_engine::resolve_principal(&self.principal_manager, &job.principal_id)
                .await;
        let Some(p) = principal else {
            return Err(format!("Principal '{}' is not loaded", job.principal_id.0));
        };
        let principal_name = p.name().await;
        let caps = p.capabilities().await;
        self.gate(&principal_name, &caps, CronOp::Mutate)?;

        let cron_db = self.path_resolver.cron_schedule(&principal_name);
        let scheduler =
            CronScheduler::new(&cron_db).map_err(|e| format!("Cron DB error: {e}"))?;
        let job_id = job.id.clone();
        scheduler
            .add_job(&job)
            .map_err(|e| format!("Failed to add job: {e}"))?;
        Ok(job_id)
    }

    /// Delete a job by id. Returns `Ok(false)` when the job is not
    /// found under any loaded principal.
    pub(crate) async fn remove_job(&self, job_id: &str) -> Result<bool, String> {
        let (principal_name, cron_db, _caps) = self.authorize(job_id, CronOp::Mutate).await?;
        let scheduler =
            CronScheduler::new(&cron_db).map_err(|e| format!("Cron DB error: {e}"))?;
        let removed = scheduler
            .delete_job(job_id)
            .map_err(|e| format!("Failed to remove job: {e}"))?;
        if !removed {
            // Distinguish "never existed" from "owner resolved via
            // history fallback but job row already gone".
            warn!("cron remove: job {job_id} not found under principal {principal_name}");
        }
        Ok(removed)
    }

    /// Aggregate jobs across loaded principals (or one principal when
    /// `principal` is a filter). Read-only; no cap gate, matching the
    /// pre-extraction IPC handler behavior.
    pub(crate) async fn list_jobs(
        &self,
        include_disabled: bool,
        principal: Option<String>,
    ) -> Result<Vec<CronJob>, String> {
        let names: Vec<String> = if let Some(filter) = principal {
            vec![filter]
        } else {
            let principals = self.principal_manager.list_all().await;
            let mut n = Vec::with_capacity(principals.len());
            for p in principals {
                n.push(p.name().await);
            }
            n
        };

        let mut jobs: Vec<CronJob> = Vec::new();
        let mut first_err: Option<String> = None;
        for name in &names {
            let path = self.path_resolver.cron_schedule(name);
            match CronScheduler::new(&path) {
                Ok(scheduler) => match scheduler.list_jobs(include_disabled) {
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
            Some(e) => Err(format!("Cron DB error: {e}")),
            None => Ok(jobs),
        }
    }

    /// Find the principal that owns `job_id` and return its name and
    /// per-principal cron schedule path.
    ///
    /// `job_id` is not globally unique across principals — each
    /// principal has its own cron DB — so we walk the loaded principals
    /// and open each schedule file until we find the job.
    /// O(principals); fine for the expected single-digit counts.
    pub(crate) async fn resolve_principal_for_job(
        &self,
        job_id: &str,
    ) -> Result<(String, PathBuf), String> {
        let principals = self.principal_manager.list_all().await;
        for principal in principals {
            let name = principal.name().await;
            let path = self.path_resolver.cron_schedule(&name);
            let scheduler = match CronScheduler::new(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            match scheduler.get_job(job_id) {
                Ok(Some(_)) => return Ok((name, path)),
                Ok(None) => {
                    // Fallback: one-shot (`delete_after_run`) jobs delete
                    // themselves after firing, but their run history is
                    // preserved — so resolve via the run records too,
                    // otherwise `cron history <id>` for a just-fired
                    // one-shot errors with "Job not found" (2026-08-07
                    // field test, Finding 4). Downstream ops on the job
                    // itself (remove/run) still fail cleanly on the
                    // missing job.
                    if let Ok(runs) = scheduler.get_run_history(job_id, 1) {
                        if !runs.is_empty() {
                            return Ok((name, path));
                        }
                    }
                    continue;
                }
                Err(_) => continue,
            }
        }
        Err(format!("Job {job_id} not found"))
    }

    /// Resolve the owner of `job_id` and run the tier + capability gate
    /// for `op`. Returns the owner's name, schedule file path, and
    /// capabilities.
    ///
    /// **Cross-tenant enforcement.** The cap check fires against the
    /// OWNER's capabilities, so a caller authorized for principal B
    /// cannot mutate or read principal A's cron state. The actor
    /// projection is the daemon's runtime authority (`Subject::Public`);
    /// IPC auth already validated any remote peer upstream.
    pub(crate) async fn authorize(
        &self,
        job_id: &str,
        op: CronOp,
    ) -> Result<(String, PathBuf, Capabilities), String> {
        let (principal_name, cron_db) = self.resolve_principal_for_job(job_id).await?;

        // Look up the owner's caps. Without a loaded Principal we can't
        // proceed — the job exists on disk but its owner is gone.
        let principals = self.principal_manager.list_all().await;
        let mut caps: Option<Capabilities> = None;
        for p in principals {
            if p.name().await == principal_name {
                caps = Some(p.capabilities().await);
                break;
            }
        }
        let caps =
            caps.ok_or_else(|| format!("Principal '{principal_name}' is not loaded"))?;

        self.gate(&principal_name, &caps, op)?;
        Ok((principal_name, cron_db, caps))
    }

    /// The owner-capability gate shared by every mutating/history op.
///
/// Both arms resolve a `LocalPath` as the gate token; the IPC handler
/// doesn't need the path itself — `CronOps::authorize` only returns
/// the path for future use. The path-discarding `map(|_| ())` keeps
/// the gate surface uniform across `Mutate` and `History`.
    fn gate(&self, principal_name: &str, caps: &Capabilities, op: CronOp) -> Result<(), String> {
        let gate = match op {
            CronOp::Mutate => self
                .authority
                .local_cron_schedule_write_for_name(principal_name, Some(caps))
                .map(|_| ()),
            CronOp::History => self
                .authority
                .local_cron_history_gate_for_name(principal_name, Some(caps))
                .map(|_| ()),
        };
        gate.map_err(|e| {
            warn!("cron {op:?} capability denied for principal {principal_name}: {e}");
            format!("[permission_denied] {e}")
        })
    }
}
