//! Cron execution engine for the daemon
//!
//! Encapsulates job polling, idle detection, delivery, and audit
//! logging. Keeps the daemon's main loop focused on lifecycle and
//! shutdown.

use crate::common::authority::{RuntimeAuthority, TierPath};
use crate::common::paths::PathResolver;
use crate::extensions::framework::async_exec::executor::{
    AsyncExecutor, AsyncTaskStatus, AsyncToolConfig,
};
use crate::extensions::framework::core::ExtensionCore;
use crate::principal::manager::PrincipalManager;
#[cfg(test)]
use crate::principal::router::{ChannelContext, ChannelKind};
use anyhow::Result;
use chrono::Utc;
use peko_auth::caller::CallerContext;
use peko_cron::{CronJob, CronJobAction, CronRun, CronScheduler, DEFAULT_MAX_RETRIES, IdleDetector};
use peko_observability::Observability;
use peko_subject::PrincipalId;
use peko_tools_core::ToolResult;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Self-contained cron subsystem.
#[derive(Clone)]
pub struct CronEngine {
    /// Per-principal scheduler map (Phase A, keyed by `PrincipalId`
    /// since Phase B). Each loaded principal's cron schedule file is
    /// held as its own `CronScheduler` so writes from the IPC handler
    /// at `<resolver>.cron_schedule(name)` are visible to the engine's
    /// poll loop without a global cache. Pre-Phase A this was a single
    /// `Arc<CronScheduler>` pointing at `<data_dir>/cron.json`.
    ///
    /// **Phase B.** The hash key is `PrincipalId` (stable DID) so the
    /// engine's identity survives a principal rename. The on-disk
    /// schedule file is still keyed by name
    /// (`<resolver>.cron_schedule(name)`); we resolve DID → name
    /// through `PrincipalManager::get` before touching the file.
    schedulers: Arc<Mutex<HashMap<PrincipalId, Arc<CronScheduler>>>>,
    path_resolver: PathResolver,
    /// **Phase C.** Engine-internal `RuntimeAuthority` for the
    /// `local_cron_schedule_runtime` accessor. The engine writes cron
    /// files on behalf of the principal owner (not a peer session), so
    /// the capability gate is intentionally bypassed — the principal's
    /// `[[permissions]]` ACL is the only gate at this layer. Built once
    /// at construction via `RuntimeAuthority::for_runtime(...)`.
    authority: Arc<RuntimeAuthority>,
    idle_detector: Arc<IdleDetector>,
    observability: Arc<Observability>,
    principal_manager: Option<Arc<PrincipalManager>>,
    /// Cron-owned `AsyncExecutor`. Spawned with a `Weak` reference to
    /// the daemon's global `ExtensionCore` so it can resolve tool
    /// instances by name without keeping the core alive longer than the
    /// daemon. Wired to the daemon's `InboxRegistry` so completion
    /// events and steer messages land in the same inboxes the
    /// in-flight `AgenticLoop` drains.
    async_executor: Arc<AsyncExecutor>,
    extension_core: Weak<ExtensionCore>,
}

impl CronEngine {
    /// Create a new cron engine.
    ///
    /// `path_resolver` is the typed resolver; cron state for each
    /// principal lives at `<resolver>.cron_schedule(name)`.
    /// `async_executor` is the daemon-shared executor used to fire
    /// `CronJobAction::SpawnTool` jobs. Pass a fresh `Arc<AsyncExecutor>`
    /// (built with `AsyncExecutor::new(standalone_inbox_registry())`) when
    /// no daemon-global executor is desired; the cron engine does not
    /// share its executor with any agent's per-call executor today.
    /// `extension_core` is held weakly so the cron engine never keeps
    /// the daemon's core alive past its natural lifetime.
    pub fn new(
        path_resolver: PathResolver,
        idle_detector: Arc<IdleDetector>,
        observability: Arc<Observability>,
        principal_manager: Option<Arc<PrincipalManager>>,
        async_executor: Arc<AsyncExecutor>,
        extension_core: Weak<ExtensionCore>,
    ) -> Self {
        Self {
            schedulers: Arc::new(Mutex::new(HashMap::new())),
            path_resolver: path_resolver.clone(),
            authority: Arc::new(RuntimeAuthority::for_runtime(path_resolver)),
            idle_detector,
            observability,
            principal_manager,
            async_executor,
            extension_core,
        }
    }

    /// Look up (or lazily construct) the `CronScheduler` for a given
    /// principal. Each principal's schedule file is opened on demand
    /// and cached in `schedulers` so repeated polls are cheap. The
    /// returned scheduler points at
    /// `<resolver>.cron_schedule(principal_name)` and any writes go
    /// straight back to the typed Local-tier path.
    ///
    /// **Phase B.** The in-memory key is the principal's stable DID.
    /// The on-disk file is keyed by the principal's display name; we
    /// resolve DID → name via `PrincipalManager::get` (the manager is
    /// the canonical authority on the DID ↔ name binding) and fall
    /// back to the disk scan in `PathResolver::lookup_principal_name`
    /// when the manager has not loaded the principal yet (cold start,
    /// test contexts).
    async fn scheduler_for(&self, principal_id: &PrincipalId) -> Result<Arc<CronScheduler>> {
        let mut map = self.schedulers.lock().await;
        if let Some(existing) = map.get(principal_id) {
            return Ok(existing.clone());
        }
        // Resolve DID → name via the manager-aware helper (which also
        // falls back to a disk scan). Then hand the resolved name to the
        // engine-internal authority accessor — Phase C bypasses the
        // capability gate because the engine writes on behalf of the
        // principal owner (Subject::Public), not on behalf of a peer.
        // The principal's `[[permissions]]` ACL is the only gate at this
        // layer.
        let principal_name = self
            .principal_name_for(principal_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("principal '{principal_id}' not found on disk"))?;
        let path = self
            .authority
            .local_cron_schedule_runtime_for_name(&principal_name)
            .map_err(|e| anyhow::anyhow!("cron path for {principal_id}: {e}"))?
            .to_path_buf();
        let scheduler = Arc::new(
            CronScheduler::new(&path)
                .map_err(|e| anyhow::anyhow!("cron scheduler init for {principal_id}: {e}"))?,
        );
        map.insert(principal_id.clone(), scheduler.clone());
        Ok(scheduler)
    }

    /// Resolve the on-disk name for a `PrincipalId`. Tries the
    /// loaded `PrincipalManager` first (cheap HashMap lookup by id or
    /// DID), then falls back to a best-effort scan of
    /// `principals_root_dir` via `PathResolver::lookup_principal_name`.
    ///
    /// **Phase B.** The manager stores `Principal`s keyed by an
    /// internal generated `PrincipalId`, but the wire identity
    /// (`CronJob::principal_id`, `Subject::Principal`, on-disk
    /// `principal.toml`'s `did` field) is the DID. The two are not
    /// the same in the freshly-created case, so we try both lookups:
    /// id first (the manager's primary key), then DID (the wire
    /// identity that cron jobs carry).
    async fn principal_name_for(&self, principal_id: &PrincipalId) -> Option<String> {
        if let Some(pm) = self.principal_manager.as_ref() {
            if let Some(p) = resolve_principal(pm, principal_id).await {
                return Some(p.name().await);
            }
        }
        self.path_resolver.lookup_principal_name(principal_id)
    }

    /// Test hook: install a pre-built `CronScheduler` for the named
    /// principal. Used by reconciler tests that don't go through the
    /// engine's normal manager-driven lookup path.
    #[cfg(test)]
    pub(crate) async fn install_scheduler_for_test(
        &self,
        principal_id: &PrincipalId,
        scheduler: Arc<CronScheduler>,
    ) {
        let mut map = self.schedulers.lock().await;
        map.insert(principal_id.clone(), scheduler);
    }

    /// Enumerate the loaded principals (best-effort) and return
    /// `(principal_id, scheduler)` pairs. If `principal_manager` is
    /// `None` (test contexts), fall back to the cached schedulers.
    ///
    /// **Phase B.** Walks the loaded principals by `PrincipalId`,
    /// resolving each to a scheduler lazily through `scheduler_for`.
    async fn all_schedulers(&self) -> Vec<(PrincipalId, Arc<CronScheduler>)> {
        if let Some(pm) = self.principal_manager.as_ref() {
            let principals = pm.list_all().await;
            let mut out = Vec::with_capacity(principals.len());
            for p in principals {
                let id = PrincipalId::from_did(&p.did().await);
                match self.scheduler_for(&id).await {
                    Ok(s) => out.push((id, s)),
                    Err(e) => warn!("Skipping principal '{id}' in cron poll: {e}"),
                }
            }
            out
        } else {
            let map = self.schedulers.lock().await;
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }
    }

    // ------------------------------------------------------------------
    // Public entry points called by the daemon's select! loop
    // ------------------------------------------------------------------

    /// Check for time-based due jobs and execute them.
    pub async fn check_and_run(&self) -> Result<()> {
        let now = Utc::now();

        // Phase A: aggregate due jobs across all loaded
        // principals. Each principal's scheduler points at its
        // typed Local-tier schedule file.
        let pairs = self.all_schedulers().await;
        let mut due_jobs: Vec<CronJob> = Vec::new();
        for (name, scheduler) in &pairs {
            match scheduler.due_jobs(now) {
                Ok(mut j) => due_jobs.append(&mut j),
                Err(e) => warn!("cron due_jobs for {name} failed: {e}"),
            }
        }
        if !due_jobs.is_empty() {
            info!("⏰ Found {} job(s) due for execution", due_jobs.len());
            for job in due_jobs {
                // Detach per-job execution so a slow `Send` job
                // (which awaits the principal's LLM turn) does not
                // block other due jobs in the same poll tick.
                // `CronEngine` is cheaply cloneable (all fields are
                // `Arc`); the spawned task owns its own clone.
                let engine = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = engine.execute_job(job).await {
                        error!("Failed to execute job: {}", e);
                    }
                });
            }
        }
        Ok(())
    }

    /// Check for idle-triggered jobs and execute if conditions are met.
    pub async fn check_idle(&self) -> Result<()> {
        use peko_cron::ScheduleKind;

        let pairs = self.all_schedulers().await;
        let mut idle_jobs: Vec<CronJob> = Vec::new();
        for (name, scheduler) in &pairs {
            match scheduler.idle_jobs(false) {
                Ok(mut j) => idle_jobs.append(&mut j),
                Err(e) => warn!("cron idle_jobs for {name} failed: {e}"),
            }
        }
        if idle_jobs.is_empty() {
            return Ok(());
        }

        debug!("Checking {} idle-triggered jobs", idle_jobs.len());

        for job in idle_jobs {
            if let ScheduleKind::Idle { minutes } = &job.schedule {
                // `is_idle` is keyed by principal name (its idle-window
                // store is per-name). Resolve DID → name lazily — most
                // idle-trigger jobs come from a recently-active
                // principal so the manager hit is the common path.
                let principal_name = self
                    .principal_name_for(&job.principal_id)
                    .await
                    .unwrap_or_else(|| job.principal_id.0.clone());
                if self.idle_detector.is_idle(&principal_name, *minutes).await {
                    info!(
                        "⏸️  Principal '{}' idle for {} minutes, executing job '{}'",
                        principal_name, minutes, job.name
                    );
                    // Detach per-job execution (same rationale as
                    // `check_and_run`).
                    let engine = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = engine.execute_job(job).await {
                            error!("Failed to execute idle job: {}", e);
                        }
                    });
                }
            }
        }

        Ok(())
    }

    /// Handle a system event and trigger matching event-triggered jobs.
    // ------------------------------------------------------------------
    // Job execution
    // ------------------------------------------------------------------

    /// Manually trigger a job by id, walking the loaded principals'
    /// schedulers to find the owner. Returns the engine's `run_id`
    /// on fire; coalesces with any in-flight run for the same job
    /// (manual triggers and poll-cycle fires share the coalescing
    /// rule — a manual trigger against a running job returns the
    /// existing in-flight `run_id` and does NOT spawn a second
    /// execution).
    ///
    /// Errors:
    /// - `"job {id} not found"` if no loaded principal owns the job.
    /// - `"cron lookup"` for low-level scheduler failures.
    pub async fn execute_job_for_id(&self, job_id: &str) -> Result<String> {
        // Walk loaded schedulers looking for the one that owns the
        // job. Schedulers are keyed by principal_id (DID); the
        // on-disk file is keyed by name. The walk is the canonical
        // way to resolve a job_id → principal_id without a separate
        // index.
        let pairs = self.all_schedulers().await;
        let mut found: Option<(PrincipalId, Arc<CronScheduler>, CronJob)> = None;
        for (id, scheduler) in &pairs {
            if let Ok(Some(job)) = scheduler.get_job(job_id) {
                found = Some((id.clone(), scheduler.clone(), job));
                break;
            }
        }
        let Some((_principal_id, scheduler, job)) = found else {
            return Err(anyhow::anyhow!("job {job_id} not found"));
        };

        // Coalesce: if there is an in-flight run for this job, return
        // its run_id and do NOT spawn a duplicate execution.
        let running = scheduler
            .list_running_runs()
            .map_err(|e| anyhow::anyhow!("cron lookup: {e}"))?;
        if let Some(open) = running.into_iter().find(|r| r.job_id == job_id) {
            info!(
                "⏰ Job '{}' already running as run_id={}; coalescing manual trigger",
                job.name, open.id
            );
            return Ok(open.id);
        }

        // Spawn the execution. Same rationale as `check_and_run`:
        // a slow `Send` job must not block the IPC handler that
        // queued the trigger.
        let run_id = Uuid::new_v4().to_string();
        let engine = self.clone();
        let job_id_for_log = job.id.clone();
        tokio::spawn(async move {
            if let Err(e) = engine.execute_job(job).await {
                error!("manual-trigger job {job_id_for_log} failed: {e}");
            }
        });
        Ok(run_id)
    }

    async fn execute_job(&self, job: CronJob) -> Result<()> {
        info!("🔄 Executing job '{}' ({})", job.name, job.id);

        let run_id = Uuid::new_v4().to_string();
        let started_at = Utc::now();

        let _ = self
            .observability
            .audit_with_caller(
                Some(&CallerContext::local().subject()),
                "cron.execute",
                Some(&job.principal_id.0),
                serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "schedule": job.schedule.display(),
                    "principal": &job.principal_id.0,
                    "run_id": &run_id,
                }),
            )
            .await;

        let run = CronRun {
            id: run_id.clone(),
            job_id: job.id.clone(),
            started_at,
            finished_at: None,
            status: "running".to_string(),
            output: None,
            error: None,
        };
        let scheduler = self.scheduler_for(&job.principal_id).await?;
        scheduler.record_run(&run)?;

        let result = match &job.action {
            CronJobAction::Send { .. } => self.run_send_job(&job).await,
            CronJobAction::SpawnTool { .. } => self.run_spawn_tool_job(&job).await,
        };

        let (status, output, error) = match result {
            Ok((s, o)) => (s, o, None),
            Err(e) => ("failed".to_string(), None, Some(e.to_string())),
        };

        let finished_at = Utc::now();
        if status == "running" {
            // SpawnTool fire returned immediately with the async task
            // id: the run stays open until the janitor reconciles the
            // task's terminal state via `finalize_run`. Attach the task
            // id to the START row — a second `record_run` with the same
            // id appended a duplicate row stuck on "running" forever
            // (2026-08-07 field test, F3).
            scheduler.attach_run_output(&run_id, output.clone(), error.clone())?;
        } else {
            // Terminal outcome (Send path, or an immediate failure):
            // close the start row in place.
            scheduler.finalize_run(&run_id, &status, output.clone(), error.clone())?;
        }

        let _ = self
            .observability
            .audit_with_caller(
                Some(&CallerContext::local().subject()),
                "cron.result",
                Some(&job.principal_id.0),
                serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "run_id": run_id,
                    "status": &status,
                    "error": error,
                    "duration_ms": (finished_at - started_at).num_milliseconds(),
                }),
            )
            .await;

        // Interval jobs anchor to the *scheduled* `next_run`, not the
        // actual finish time — otherwise tick quantisation (up to one
        // poll interval) plus execution time accumulate into permanent
        // drift (60s job fired every ~75s; 2026-08-07 field test,
        // Finding 6). Other schedule kinds compute absolute times and
        // are immune.
        let next_run = match &job.schedule {
            peko_cron::ScheduleKind::Every { every_ms } => {
                peko_cron::calculate_next_interval_anchored(job.next_run, *every_ms, finished_at)
            }
            _ => peko_cron::calculate_next_run(&job.schedule, finished_at)?,
        };
        scheduler.update_job_after_run(&job.id, &status, next_run)?;

        // Retry budget enforcement. `update_job_after_run` just bumped
        // `consecutive_failures` (or reset it on success); re-read the
        // job to inspect the updated counter and disable the job when
        // the budget is exhausted. `max_retries: None` means unlimited
        // — that preserves the legacy behavior for callers that have
        // not picked up the new field yet.
        if status != "success" {
            if let Ok(Some(updated)) = scheduler.get_job(&job.id) {
                let budget = updated.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
                if updated.consecutive_failures >= budget {
                    warn!(
                        "🚫 Disabling job '{}' after {} consecutive failures (max_retries={})",
                        updated.name, updated.consecutive_failures, budget
                    );
                    let _ = scheduler.set_job_enabled(&updated.id, false);
                    let _ = self
                        .observability
                        .audit_with_caller(
                            Some(&CallerContext::local().subject()),
                            "cron.disabled",
                            Some(&updated.principal_id.0),
                            serde_json::json!({
                                "job_id": updated.id,
                                "job_name": updated.name,
                                "consecutive_failures": updated.consecutive_failures,
                                "max_retries": budget,
                                "last_status": status,
                            }),
                        )
                        .await;
                }
            }
        }

        // Sprint 7 Commit B: `DeliveryMode::Announce` side-effect retired.
        // The engine previously branched on `job.delivery` here, calling
        // `handle_delivery` → `send_announcement`, which only wrote an
        // unread JSON file to `{data_dir}/runtime/announcements/`. No
        // reader existed; the `--announce` CLI flag is gone, the
        // `CronJob.delivery` field is gone, and the engine's
        // announce-related helpers are deleted below.

        // One-shot reaping keys on the FIRE, not the run outcome:
        // "fired" is the lifecycle fact that retires a one-shot job;
        // the run's status belongs to history (preserved). Gating on
        // `status == "success"` left SpawnTool one-shots (which return
        // "running" immediately) parked on the 100-year sentinel
        // forever (2026-08-07 field test, F3).
        if job.delete_after_run {
            info!(
                "🗑️  Deleting one-shot job '{}' after run (status: {})",
                job.name, status
            );
            scheduler.delete_job(&job.id)?;
        }

        info!("✅ Job '{}' completed with status: {}", job.name, status);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Principal execution — Send path (CLI cron)
    // ------------------------------------------------------------------

    /// Run a [`CronJobAction::Send`] job by firing its message into the
    /// principal's TRUNK session `root:self` via
    /// [`PrincipalManager::receive_trunk`]. Equivalent to a deferred
    /// self-turn from the daemon.
    ///
    /// Phase 7 (sprint 2, 2026-08-17): the trunk is the DEFAULT (and
    /// only) destination — `target: None` and `target = "trunk"` are
    /// the same route; the per-owner `root:cron:{owner}` session is
    /// retired.
    ///
    /// **Cron is silent to the user.** The agent (the trunk) is the
    /// active entry point for any user-facing message — it talks back
    /// via `ChannelSend`. The cron engine does NOT cross-post the
    /// agent's reply into the owner's standing peer child or the
    /// user's DM channel: doing so duplicates the agent's `ChannelSend`
    /// and reads as either noise (when the agent already replied) or a
    /// fake reply (when the agent failed). The agent is responsible
    /// for communication; the cron engine's only audit trail is the
    /// `CronRun` history (operator-facing, via `peko cron history`),
    /// the trunk session JSONL (engine context), and the
    /// `IdleDetector::record_activity` bump.
    async fn run_send_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        let Some(pm) = self.principal_manager.as_ref() else {
            return Ok((
                "failed".to_string(),
                Some("PrincipalManager not available".to_string()),
            ));
        };

        let principal = resolve_principal(&pm, &job.principal_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Principal '{}' not loaded", job.principal_id.0))?;

        // Validate the target. `None` (default) and "trunk" are the
        // same destination since Phase 7; the DTO deserializer and
        // `CronScheduler::add_job` both reject unknown targets, but a
        // struct-literal construction could still slip one through —
        // fail loudly rather than misroute.
        match &job.action {
            CronJobAction::Send { target, .. } => match target.as_deref() {
                None | Some(peko_cron::tools::SEND_TARGET_TRUNK) => {}
                Some(other) => {
                    return Ok((
                        "failed".to_string(),
                        Some(format!(
                            "invalid cron Send target '{other}': only \"{}\" is supported",
                            peko_cron::tools::SEND_TARGET_TRUNK
                        )),
                    ));
                }
            },
            // The dispatch in `execute_job` only routes Send here.
            _ => {}
        }

        match pm
            .receive_trunk(principal.id.clone(), job.task_description(), None)
            .await
        {
            Ok(response) => {
                self.idle_detector
                    .record_activity(&principal.name().await)
                    .await;
                Ok(("success".to_string(), Some(response.content)))
            }
            Err(e) => Ok((
                "failed".to_string(),
                Some(format!("Principal trunk execution error: {e}")),
            )),
        }
    }

    // ------------------------------------------------------------------
    // Async execution — SpawnTool path (agent cron)
    // ------------------------------------------------------------------

    /// Run a [`CronJobAction::SpawnTool`] job by handing it to the
    /// cron engine's `AsyncExecutor`. The executor:
    /// 1. resolves the tool instance via the daemon's `ExtensionCore`,
    /// 2. records an `AsyncTask` entry attributed to the principal's
    ///    trunk session (so `AsyncOutput`/`AsyncStatus`/`AsyncStop`
    ///    remain scoped to that root), and
    /// 3. on completion, posts a `SteeringMessage` into the principal's
    ///    trunk inbox (`root:self`) when `wake_on_completion=true`
    ///    (Phase 3b — see below).
    ///
    /// Returns `("running", Some(task_id))` immediately — the actual
    /// tool execution is async. The daemon's janitor loop reconciles
    /// the eventual outcome against the executor's registry to update
    /// `last_status` (Phase 4).
    async fn run_spawn_tool_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        let CronJobAction::SpawnTool {
            tool_name,
            tool_params,
            wake_on_completion,
            timeout_secs,
            ..
        } = &job.action
        else {
            // Defensive: the dispatch in `execute_job` only routes
            // SpawnTool actions here. Anything else is a bug.
            return Ok((
                "failed".to_string(),
                Some("run_spawn_tool_job called with non-SpawnTool action".to_string()),
            ));
        };

        let core = match self.extension_core.upgrade() {
            Some(c) => c,
            None => {
                return Ok((
                    "failed".to_string(),
                    Some("ExtensionCore dropped; cannot resolve tool".to_string()),
                ));
            }
        };

        // The executor's inbox key is the principal's TRUNK session id
        // (`root:self`) so completion events and steer messages reach
        // the principal's own root — Phase 3b (2026-08-15). Pre-3b this
        // was the owner's conversational root (`root:{owner}`), which
        // gave one PEKO two "roots": cron Send with target="trunk" fired
        // into `root:self` while SpawnTool wakes landed in the owner's
        // human-facing thread. PEKO.md §K requires both to target the
        // principal's root; the trunk is that root. The steer machinery
        // is purely session-keyed (`InboxRegistry::get_or_create`
        // creates the inbox on demand), so this is a key change only:
        // when no trunk run is live the message waits in the registry
        // and the trunk's next turn drains it at iteration start —
        // identical semantics to the old key with no active run.
        let trunk_session_key = crate::principal::routers::root::trunk_session_id();

        let Some(pm) = self.principal_manager.as_ref() else {
            return Ok((
                "failed".to_string(),
                Some("PrincipalManager not available".to_string()),
            ));
        };
        let principal = match resolve_principal(&pm, &job.principal_id).await {
            Some(p) => p,
            None => {
                return Ok((
                    "failed".to_string(),
                    Some(format!("Principal '{}' not loaded", job.principal_id.0)),
                ));
            }
        };
        // Snapshot the principal's grants at fire time, then derive active
        // extensions from that same snapshot. Both values flow through the
        // canonical tool funnel so extension ownership requirements cannot be
        // bypassed by scheduled execution.
        let (snapshot_capabilities, snapshot_principal_id, capabilities) = {
            let config = principal.config.read().await;
            let capabilities = config.capabilities.clone();
            let caps = capabilities.grants.iter().map(|c| c.0.clone()).collect();
            (caps, config.name.clone(), capabilities)
        };
        let snapshot_active_extensions = pm
            .active_extensions_for(&principal, &capabilities)
            .await
            .to_vec();

        let wake = wake_on_completion.unwrap_or(false);
        let timeout = timeout_secs.or(Some(7200));

        let config = AsyncToolConfig {
            timeout_secs: timeout,
            wake_on_completion: wake,
            principal_root_session_key: Some(trunk_session_key.clone()),
            label: Some(job.name.clone()),
            ..Default::default()
        };

        let executor = self.async_executor.clone();

        // F38: route through `executor.dispatch_tool(...)` so the F37
        // canonical-funnel closure construction lives inside the
        // executor. The cron engine doesn't currently have a
        // `CancellationToken` to bridge into
        // `dispatch_tool_with_signal` — the registry-level cancel still
        // works (status flips to `Cancelled`) but the inner tool body
        // doesn't observe `is_aborted()`. Future work: wire a job-level
        // CancellationToken into `run_spawn_tool_job` and switch to
        // `dispatch_tool_with_signal`. The funnel is mandatory now,
        // which is the F38 invariant we care about.
        let context =
            crate::extensions::framework::async_exec::executor::ToolDispatchContext::builder(
                tool_name.clone(),
                tool_params.clone(),
                trunk_session_key.clone(),
            )
            .for_principal(snapshot_principal_id, snapshot_capabilities)
            .with_active_extensions(snapshot_active_extensions);

        let receipt = executor.dispatch_tool(&core, context, config).await?;

        // The fire itself completed synchronously (the tool runs in the
        // background). Return immediately so the cron engine records
        // the run with the spawn receipt.
        Ok(("running".to_string(), Some(receipt.task_id)))
    }

    // ------------------------------------------------------------------
    // Delivery
    // ------------------------------------------------------------------

    /// Reconcile `CronRun` rows still marked `"running"` against the
    /// executor's task registry. Each row's `output` carries the
    /// async `task_id` we wrote at fire time; we look it up and, when
    /// terminal, finalize the row with the executor's outcome
    /// (`success`/`failed`/`timed_out`/`cancelled`) and propagate
    /// `last_status` onto the owning `CronJob`.
    pub async fn reconcile_running_runs(&self) -> Result<usize> {
        // Phase A: aggregate running rows across all loaded
        // principals. We keep `(principal_id, run)` pairs so the
        // finalize step writes back to the principal that owns the
        // job — `CronRun` itself doesn't carry a principal
        // identifier today. **Phase B.** Pair key is `PrincipalId`
        // so the engine doesn't have to resolve DID → name just to
        // look up the scheduler.
        let pairs = self.all_schedulers().await;
        let mut running: Vec<(PrincipalId, CronRun)> = Vec::new();
        for (id, scheduler) in &pairs {
            match scheduler.list_running_runs() {
                Ok(r) => {
                    for run in r {
                        running.push((id.clone(), run));
                    }
                }
                Err(e) => warn!("cron list_running_runs for {id} failed: {e}"),
            }
        }
        if running.is_empty() {
            return Ok(0);
        }

        let mut finalized = 0usize;
        for (principal_id, run) in running {
            let Some(task_id) = run.output.clone() else {
                // Running row without a task id (e.g. a Send job left
                // in this state by an older code path). Leave it.
                continue;
            };

            let status = match self.async_executor.check_status(&task_id).await {
                Some(s) => s,
                // Registry no longer holds this task. Treat it as a
                // successful no-op so the cron row lands somewhere
                // other than "running" forever.
                None => AsyncTaskStatus::Completed {
                    result: ToolResult::success(serde_json::json!({
                        "note": "task disappeared from registry"
                    })),
                },
            };

            if !status.is_terminal() {
                continue;
            }

            let (cron_status, output, error) = map_async_status(status);
            // Phase A: target the principal that owns this run so
            // the finalize + last_status writes land on the right
            // schedule file.
            let scheduler = self.scheduler_for(&principal_id).await?;
            if scheduler.finalize_run(&run.id, &cron_status, output.clone(), error.clone())? {
                finalized += 1;
                scheduler.set_job_last_status(&run.job_id, &cron_status)?;
                info!(
                    "🔁 Reconciled cron run {} (job={}, principal={}) → {}",
                    run.id, run.job_id, principal_id.0, cron_status
                );
            }
        }
        Ok(finalized)
    }
}

/// Translate a terminal `AsyncTaskStatus` into the wire string the cron
/// `CronRun.status` field has historically used.
///
/// `Completed` is collapsed to `"success"` so existing users (the CLI
/// renderer, history grep) keep matching what the `Send` path emitted.
/// Failures / timeouts / cancellations keep the executor's names so an
/// operator can correlate cron history with `AsyncOutput`. The caller
/// gates on [`AsyncTaskStatus::is_terminal`] so non-terminal variants
/// (Pending, Running) never reach this match; the catch-all arm is a
/// defensive `unreachable!` for that contract.
fn map_async_status(status: AsyncTaskStatus) -> (String, Option<String>, Option<String>) {
    debug_assert!(
        status.is_terminal(),
        "map_async_status called with non-terminal status: {status:?}"
    );
    match status {
        AsyncTaskStatus::Completed { result } => {
            let rendered = result
                .data
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<no result data>".to_string());
            ("success".to_string(), Some(rendered), None)
        }
        AsyncTaskStatus::Failed { error } => ("failed".to_string(), None, Some(error)),
        AsyncTaskStatus::Cancelled => (
            "cancelled".to_string(),
            None,
            Some("cancelled by user".to_string()),
        ),
        AsyncTaskStatus::TimedOut { error } => ("timed_out".to_string(), None, Some(error)),
        _ => unreachable!("caller must gate on AsyncTaskStatus::is_terminal()"),
    }
}

/// Resolve a `PrincipalId` (typically a wire-format DID) to a loaded
/// `Arc<Principal>`. Tries the manager's primary id-keyed lookup first
/// (cheap), then falls back to a DID scan.
///
/// **Phase B.** The wire identity is the DID (`CronJob::principal_id`,
/// `Subject::Principal`'s inner `PrincipalDID`), but the manager's
/// internal hashmap is keyed by the *generated* `PrincipalId` that
/// `PrincipalManager::create` minted at construction time. In the
/// freshly-created case those two strings differ; the DID lookup is
/// what makes the wire identity work.
/// Resolve a `Principal` by its `PrincipalId` (wire form).
///
/// The manager's `principals` hash is keyed by the internal
/// `PrincipalId::generate()` — NOT by the DID that travels on the
/// wire. Callers receiving a wire `PrincipalId` (from `CronJob`,
/// `CronRemove`, etc.) must try both lookups. Public so the cron
/// IPC handler can reuse this rather than duplicating the logic.
pub(crate) async fn resolve_principal(
    pm: &PrincipalManager,
    principal_id: &PrincipalId,
) -> Option<Arc<crate::principal::Principal>> {
    if let Some(p) = pm.get(principal_id.clone()).await {
        return Some(p);
    }
    pm.find_by_did(&principal_id.0).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use crate::engine::tool_runtime::ToolRuntime;
    use crate::extensions::framework::core::init_global_core;
    use crate::principal::config::{
        PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use crate::principal::{
        DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalManager,
    };
    use chrono::{Duration, Utc};
    use peko_auth::Exposure;
    use peko_auth::{Permission, PermissionGrant};
    use peko_extension_api::Capabilities;
    use peko_providers::mock::MockAdapter;
    use peko_providers::resolver::LlmResolver;
    use peko_subject::Subject;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn engine_from_tmp(tmp: &TempDir) -> CronEngine {
        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        // Phase A: the engine derives per-principal schedulers
        // from the typed path resolver. The legacy
        // `tmp.path().join("cron.json")` is gone.
        let resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        CronEngine::new(
            resolver,
            idle,
            obs,
            None,
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        )
    }

    async fn setup_principal_manager(tmp: &TempDir) -> Arc<PrincipalManager> {
        let path_resolver = PathResolver::with_dirs(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), tmp.path())
            .await
            .expect("tool runtime should initialize");
        init_global_core(tool_runtime.extension_core().clone());

        let workspace = tmp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = tmp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        adapter.queue_text("Hello from cron");
        Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(resolver),
        )
    }

    async fn create_test_principal(
        manager: &PrincipalManager,
        workspace: &std::path::Path,
        name: &str,
    ) -> Arc<crate::principal::Principal> {
        // Phase A: agents live in the Shared tier
        // (`{config_dir}/principals/{name}/agents`). In tests the
        // `workspace` passed in is the Shared principal root
        // (`{tmp}/principals`), so `agents` is just one join below.
        let agents_dir = workspace.join(name).join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        let prompt_path = agents_dir.join("primary.md");
        let prompt_body = format!(
            "---\ndescription: \"Test assistant for {name}\"\n---\n\n\
             You are {name}, a test assistant. Reply concisely.\n"
        );
        tokio::fs::write(&prompt_path, prompt_body).await.unwrap();

        let config = test_config(name);
        manager.create(config).await.unwrap()
    }

    fn test_config(name: &str) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: None,
            owner: Subject::User("test-owner".to_string()),
            identity: PrincipalIdentityConfig {
                display_name: Some(name.to_string()),
                description: Some(format!("The {name} Principal")),
                avatar: None,
            },
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            capabilities: Capabilities::default(),
            exposure: Exposure::Private,
            status: None,
            permissions: vec![PermissionGrant {
                subject: Subject::Public,
                permission: Permission::Chat,
                granted_at: chrono::Utc::now().to_rfc3339(),
                granted_by: Subject::User("test-owner".to_string()),
            }],
            preferred_model_id: Some("mock".to_string()),
            transport_preference: Default::default(),
            authority: None,
            quota: None,
            children: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_cron_engine_creation() {
        let tmp = TempDir::new().unwrap();
        let _engine = engine_from_tmp(&tmp);
    }

    #[tokio::test]
    async fn test_check_and_run_empty() {
        let tmp = TempDir::new().unwrap();
        let engine = engine_from_tmp(&tmp);
        assert!(engine.check_and_run().await.is_ok());
    }

    #[tokio::test]
    async fn test_check_idle_empty() {
        let tmp = TempDir::new().unwrap();
        let engine = engine_from_tmp(&tmp);
        assert!(engine.check_idle().await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_check_and_run_executes_principal_job() {
        let tmp = TempDir::new().unwrap();
        let manager = setup_principal_manager(&tmp).await;
        let workspace = tmp.path().join("principals");
        let principal = create_test_principal(&manager, &workspace, "crony").await;

        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        // Phase A: build a resolver pointed at this test's tmp so
        // the engine can derive per-principal schedule files.
        let resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        // Schedule file lives at the typed per-principal path
        // (`<resolver>.cron_schedule("crony")`), not the legacy
        // `<tmp>/cron.json`.
        let scheduler = Arc::new(CronScheduler::new(resolver.cron_schedule("crony")).unwrap());
        let engine = CronEngine::new(
            resolver,
            idle,
            obs,
            Some(manager.clone()),
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        );

        let job = CronJob {
            id: "job-1".to_string(),
            name: "test-job".to_string(),
            // **Phase B.** Cron jobs key on the principal's stable
            // DID, which is also what `principal.did()` returns.
            principal_id: PrincipalId::from_did(&principal.did().await),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::Send {
                message: "Hello from cron".to_string(),
                target: None,
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - Duration::minutes(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        engine.check_and_run().await.unwrap();

        // PR 2: per-job execution is now detached onto a tokio task,
        // so poll the run history rather than asserting
        // synchronously. 5s budget is more than enough for the
        // in-process test setup.
        let mut success = false;
        for _ in 0..50 {
            let runs = scheduler.get_run_history(&job.id, 10).unwrap();
            if runs.iter().any(|r| r.status == "success") {
                success = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(success, "expected the due Send job to land a success row");

        let runs = scheduler.get_run_history(&job.id, 10).unwrap();
        let success = runs.iter().find(|r| r.status == "success");
        assert!(
            success.is_some(),
            "expected a successful run in history, got: {runs:?}"
        );

        // Activity should have been recorded for the Principal.
        assert!(!engine.idle_detector.is_idle("crony", 1).await);

        // Avoid dropping the principal early; it is not needed after this.
        drop(principal);
    }

    /// Direct unit test for the cron reconciler: a synthetic
    /// "running" CronRun whose `output` matches a real entry in the
    /// AsyncTaskRegistry with a terminal status must be finalized
    /// and the parent job's `last_status` updated.
    #[tokio::test]
    async fn test_reconcile_running_runs_finalizes_known_task() {
        let tmp = TempDir::new().unwrap();
        // Phase A: per-principal cron DB lives at the typed path
        // under the principal's Local tier. Build a resolver to
        // derive the canonical location.
        let cron_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let scheduler = Arc::new(CronScheduler::new(cron_resolver.cron_schedule("crony")).unwrap());

        // Seed a SpawnTool job and a corresponding "running" run row.
        let job = CronJob {
            id: "job-recon".to_string(),
            name: "recon-job".to_string(),
            principal_id: PrincipalId("crony".to_string()),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "Agent".to_string(),
                tool_params: serde_json::json!({"prompt": "ping"}),
                wake_on_completion: Some(false),
                timeout_secs: Some(7200),
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + Duration::minutes(5),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        let run = CronRun {
            id: "run-recon".to_string(),
            job_id: job.id.clone(),
            started_at: Utc::now(),
            finished_at: None,
            status: "running".to_string(),
            output: Some("shell:abc".to_string()),
            error: None,
        };
        scheduler.record_run(&run).unwrap();

        // Pre-mark `last_status = "running"` so we can see the
        // reconciler update it.
        scheduler.set_job_last_status(&job.id, "running").unwrap();

        // Build a CronEngine with an executor whose registry holds a
        // terminal entry for `shell:abc`.
        let async_executor = Arc::new(AsyncExecutor::new(
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        ));
        let mut entry =
            crate::extensions::framework::async_exec::executor::registry::AsyncTaskEntry::new(
                "shell:abc".to_string(),
                "Bash".to_string(),
                serde_json::json!({"command": "echo done"}),
                "session_worker_1".to_string(),
                AsyncToolConfig::default(),
            );
        entry.set_result(serde_json::json!("done"));
        async_executor.registry().write().await.register(entry);
        // Mark the entry as Completed so reconcile treats it as terminal.
        async_executor.registry().write().await.update_status(
            &"shell:abc".to_string(),
            AsyncTaskStatus::Completed {
                result: ToolResult::success(serde_json::json!("done")),
            },
        );

        // Phase A: build a resolver pointing at the test tmp so the
        // engine derives per-principal schedule files at
        // `<tmp>/principals/{name}/local/cron/...`.
        let cron_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let engine = CronEngine::new(
            cron_resolver,
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            None,
            async_executor,
            std::sync::Weak::new(),
        );
        // Phase A: the engine no longer holds a single global
        // scheduler; install the test's scheduler into the engine's
        // per-principal cache so the reconciler can find it.
        engine
            .install_scheduler_for_test(&PrincipalId("crony".into()), scheduler.clone())
            .await;

        let n = engine.reconcile_running_runs().await.unwrap();
        assert_eq!(n, 1, "expected exactly one finalized run");

        let updated = scheduler.get_run_history(&job.id, 10).unwrap();
        let run = updated
            .iter()
            .find(|r| r.id == "run-recon")
            .expect("run row should still be present");
        assert_eq!(run.status, "success");
        assert!(run.finished_at.is_some());
        // The output is the JSON-serialized form of the value the executor
        // produced — a JSON string `"done"` serializes to `\"done\"`.
        let output = run.output.as_deref().unwrap_or_default();
        assert!(
            output.contains("done"),
            "expected output to mention 'done', got {output:?}"
        );

        // And the job's last_status is updated without bumping run_count
        // (run_count remains 0 because we used the helper, not
        // update_job_after_run).
        let updated_job = scheduler.get_job(&job.id).unwrap().unwrap();
        assert_eq!(updated_job.last_status.as_deref(), Some("success"));
        assert_eq!(updated_job.run_count, 0);
    }

    /// When the AsyncTaskRegistry no longer holds the task (e.g. the
    /// janitor already cleaned it up), the reconciler still finalizes
    /// the cron row as `success` so it does not stay marked "running"
    /// forever.
    #[tokio::test]
    async fn test_reconcile_finalizes_when_task_disappeared() {
        let tmp = TempDir::new().unwrap();
        // Phase A: per-principal cron DB lives at the typed path
        // under the principal's Local tier.
        let cron_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let scheduler = Arc::new(CronScheduler::new(cron_resolver.cron_schedule("crony")).unwrap());

        let job = CronJob {
            id: "job-vanished".to_string(),
            name: "vanished".to_string(),
            principal_id: PrincipalId("crony".to_string()),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "Bash".to_string(),
                tool_params: serde_json::json!({}),
                wake_on_completion: Some(false),
                timeout_secs: Some(7200),
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + Duration::minutes(5),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        // Two rows: one with a real task id we will orphan, one with
        // a task id that no longer exists in the registry. Both must
        // become terminal in one reconcile pass.
        scheduler
            .record_run(&CronRun {
                id: "run-vanish".to_string(),
                job_id: job.id.clone(),
                started_at: Utc::now(),
                finished_at: None,
                status: "running".to_string(),
                output: Some("ghost:gone".to_string()),
                error: None,
            })
            .unwrap();

        let async_executor = Arc::new(AsyncExecutor::new(
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        ));
        // Phase A: build a path resolver pointing at the test's
        // tmp dir so the cron engine derives per-principal schedule
        // files at `<tmp>/principals/{name}/local/cron/...`.
        let cron_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let engine = CronEngine::new(
            cron_resolver,
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            None,
            async_executor,
            std::sync::Weak::new(),
        );
        // Phase A: the engine no longer holds a single global
        // scheduler; install the test's scheduler into the engine's
        // per-principal cache so the reconciler can find it.
        engine
            .install_scheduler_for_test(&PrincipalId("crony".into()), scheduler.clone())
            .await;

        let n = engine.reconcile_running_runs().await.unwrap();
        assert_eq!(n, 1);

        let updated = scheduler.get_run_history(&job.id, 10).unwrap();
        let run = updated
            .iter()
            .find(|r| r.id == "run-vanish")
            .expect("run should still be present");
        assert_eq!(run.status, "success", "missing tasks finalize as success");
        assert!(run.finished_at.is_some());
    }

    /// Recursively find a file by exact file name under `dir`.
    fn find_file_named(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_file_named(&path, name) {
                    return Some(found);
                }
            } else if entry.file_name() == name {
                return Some(path);
            }
        }
        None
    }

    /// Phase 7 helper: the owner peer's standing child session id
    /// (the `/user-test-owner` child of the trunk), resolved find-only
    /// from the principal's sessions dir.
    async fn owner_child_id(
        principal: &Arc<crate::principal::Principal>,
        peer: &Subject,
    ) -> Option<String> {
        let mut mgr = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(principal.memory.sessions_dir().clone());
        let metas = mgr.list_all_sessions(false).await.ok()?;
        crate::principal::peer_children::find_peer_child(&metas, peer)
    }

    /// Phase 7 (sprint 2): a `Send` cron job (default target) fires its
    /// turn into the principal's TRUNK session `root:self` — the
    /// `root:cron:{owner}` session is retired. The cron engine is now
    /// silent to the user (PR-B, 2026-08-21): the agent owns
    /// user-facing communication via `ChannelSend`, and the cron
    /// engine does NOT cross-post the reply into the owner's standing
    /// peer child or the user's DM channel. The trunk JSONL still
    /// carries the agent's full transcript (engine context), and the
    /// run lands in `CronRun` history (operator-facing).
    #[tokio::test(flavor = "multi_thread")]
    async fn test_send_job_fires_into_trunk_and_is_silent_to_user() {
        let tmp = TempDir::new().unwrap();

        // Principal manager with a mock resolver; two queued texts:
        // one for the conversational turn, one for the cron turn.
        let path_resolver = PathResolver::with_dirs(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), tmp.path())
            .await
            .expect("tool runtime should initialize");
        init_global_core(tool_runtime.extension_core().clone());
        let catalog_path = tmp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        adapter.queue_text("conversational reply");
        adapter.queue_text("cron turn reply");
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(resolver),
        );

        let workspace = tmp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let principal = create_test_principal(&manager, &workspace, "crony").await;

        let owner = Subject::User("test-owner".to_string());

        // 1. A conversational (CLI) turn creates the owner's standing
        //    peer child.
        manager
            .receive_streaming(
                principal.id.clone(),
                owner.clone(),
                "hello there".to_string(),
                ChannelContext {
                    kind: ChannelKind::Cli,
                    streaming: false,
                },
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("conversational turn should succeed");

        // 2. A due Send job (DEFAULT target) fires a trunk turn.
        let engine_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let scheduler =
            Arc::new(CronScheduler::new(engine_resolver.cron_schedule("crony")).unwrap());
        let engine = CronEngine::new(
            engine_resolver,
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            Some(manager.clone()),
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        );
        let job = CronJob {
            id: "job-iso".to_string(),
            name: "test-job".to_string(),
            principal_id: PrincipalId::from_did(&principal.did().await),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::Send {
                message: "cron tick payload".to_string(),
                target: None,
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - Duration::minutes(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        engine.check_and_run().await.unwrap();

        // PR 2: per-job execution is now detached onto a tokio task,
        // so poll the run history until the row is finalized. 5s
        // budget.
        let mut runs: Vec<peko_cron::CronRun> = Vec::new();
        for _ in 0..50 {
            runs = scheduler.get_run_history(&job.id, 10).unwrap();
            if runs.len() >= 1 && runs[0].status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Exactly ONE run row, closed as success (F3: no duplicate
        // start/completion rows).
        assert_eq!(runs.len(), 1, "expected a single run row, got: {runs:?}");
        assert_eq!(runs[0].status, "success");
        assert!(runs[0].finished_at.is_some());

        // The cron turn ran in the TRUNK session.
        let trunk_path = find_file_named(
            tmp.path(),
            &format!("{}.jsonl", peko_session::SessionId::from("root:self")),
        )
        .expect("trunk session JSONL should exist");
        let trunk_jsonl = std::fs::read_to_string(&trunk_path).unwrap();
        assert!(
            trunk_jsonl.contains("cron tick payload"),
            "trunk session should contain the cron turn, got: {trunk_jsonl}"
        );
        assert!(
            trunk_jsonl.contains("cron turn reply"),
            "trunk session should contain the turn's reply, got: {trunk_jsonl}"
        );
        // PR-B: the cron engine no longer cross-posts anything — the
        // dispatcher's `[notify]` self-view line is gone too (it was a
        // side effect of `deliver_note` calling `messenger.deliver_note`,
        // which is no longer invoked).
        assert!(
            !trunk_jsonl.contains("[notify]"),
            "trunk session must NOT carry a cron note-delivery self-view line, got: {trunk_jsonl}"
        );

        // The owner's peer child holds ONLY the human turn from the
        // conversational reply — the cron engine's reply is silent
        // (PR-B, 2026-08-21). The agent is responsible for any
        // user-facing communication via `ChannelSend`.
        let child_id = owner_child_id(&principal, &owner)
            .await
            .expect("owner peer child should exist");
        let conv_path = find_file_named(tmp.path(), &format!("{child_id}.jsonl"))
            .expect("owner child session JSONL should exist");
        let conv_jsonl = std::fs::read_to_string(&conv_path).unwrap();
        assert!(
            conv_jsonl.contains("hello there"),
            "owner child should contain the human turn"
        );
        assert!(
            !conv_jsonl.contains("⏰ [cron job 'test-job' fired]"),
            "owner child must NOT contain a cron-fired note (PR-B silence), got: {conv_jsonl}"
        );
        assert!(
            !conv_jsonl.contains("cron turn reply"),
            "owner child must NOT contain the cron turn's reply text (PR-B silence), got: {conv_jsonl}"
        );
        assert!(
            !conv_jsonl.contains("cron tick payload"),
            "cron message must NOT leak into the owner child session"
        );

        // Retired session ids are never created.
        assert!(
            find_file_named(tmp.path(), "root:cron:user:test-owner.jsonl").is_none(),
            "the per-owner cron session is retired — it must never be created"
        );
        assert!(
            find_file_named(tmp.path(), "root:user:test-owner.jsonl").is_none(),
            "the per-peer root session is retired — it must never be created"
        );

        drop(principal);
    }

    /// Phase 7 (2026-08-17): `target = "trunk"` stays accepted and is
    /// the SAME route as the default — the turn fires into the
    /// principal's trunk session `root:self`. PR-B (2026-08-21) makes
    /// the cron engine silent to the user, so unlike the previous
    /// Phase-3 behavior the owner child does NOT receive a cross-post
    /// note. The agent owns any user-facing communication via
    /// `ChannelSend`.
    ///
    /// `#[serial]` because the principal-manager tests mutate the
    /// process-global `PEKO_HOME` (identity key storage root); running
    /// concurrently with them makes principal DID resolution fail
    /// mid-test.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn test_send_job_explicit_trunk_target_matches_default() {
        let tmp = TempDir::new().unwrap();

        // Principal manager with a mock resolver; two queued texts:
        // one for the conversational turn, one for the trunk turn.
        let path_resolver = PathResolver::with_dirs(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), tmp.path())
            .await
            .expect("tool runtime should initialize");
        init_global_core(tool_runtime.extension_core().clone());
        let catalog_path = tmp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        adapter.queue_text("conversational reply");
        adapter.queue_text("trunk turn reply");
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(resolver),
        );

        let workspace = tmp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let principal = create_test_principal(&manager, &workspace, "crony").await;
        let owner = Subject::User("test-owner".to_string());

        // 1. A conversational (CLI) turn creates the owner's standing
        //    peer child — so the note cross-post has somewhere to
        //    land.
        manager
            .receive_streaming(
                principal.id.clone(),
                owner.clone(),
                "hello there".to_string(),
                ChannelContext {
                    kind: ChannelKind::Cli,
                    streaming: false,
                },
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("conversational turn should succeed");

        // 2. A due trunk-targeted Send job fires.
        let engine_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let scheduler =
            Arc::new(CronScheduler::new(engine_resolver.cron_schedule("crony")).unwrap());
        let engine = CronEngine::new(
            engine_resolver,
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            Some(manager.clone()),
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        );
        let job = CronJob {
            id: "job-trunk".to_string(),
            name: "self-upkeep".to_string(),
            principal_id: PrincipalId::from_did(&principal.did().await),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::Send {
                message: "organize your memory".to_string(),
                target: Some("trunk".to_string()),
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - Duration::minutes(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        engine.check_and_run().await.unwrap();

        // PR 2: poll until the detached per-job execution finalizes the
        // run row. 5s budget.
        let mut runs: Vec<peko_cron::CronRun> = Vec::new();
        for _ in 0..50 {
            runs = scheduler.get_run_history(&job.id, 10).unwrap();
            if runs.len() >= 1 && runs[0].status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(runs.len(), 1, "expected a single run row, got: {runs:?}");
        assert_eq!(runs[0].status, "success");
        assert!(runs[0].finished_at.is_some());

        // The turn ran in the trunk session.
        let trunk_path = find_file_named(
            tmp.path(),
            &format!("{}.jsonl", peko_session::SessionId::from("root:self")),
        )
        .expect("trunk session JSONL should exist");
        let trunk_jsonl = std::fs::read_to_string(&trunk_path).unwrap();
        assert!(
            trunk_jsonl.contains("organize your memory"),
            "trunk session should contain the cron payload, got: {trunk_jsonl}"
        );
        assert!(
            trunk_jsonl.contains("trunk turn reply"),
            "trunk session should contain the turn's reply"
        );

        // No per-owner cron session was created.
        assert!(
            find_file_named(tmp.path(), "root:cron:user:test-owner.jsonl").is_none(),
            "the per-owner cron session is retired — it must never be created"
        );

        // The owner's peer child holds ONLY the human turn — the cron
        // engine is silent (PR-B, 2026-08-21). The agent is responsible
        // for any user-facing communication via `ChannelSend`.
        let child_id = owner_child_id(&principal, &owner)
            .await
            .expect("owner peer child should exist");
        let conv_path = find_file_named(tmp.path(), &format!("{child_id}.jsonl"))
            .expect("owner child session JSONL should exist");
        let conv_jsonl = std::fs::read_to_string(&conv_path).unwrap();
        assert!(
            conv_jsonl.contains("hello there"),
            "owner child should contain the human turn"
        );
        assert!(
            !conv_jsonl.contains("⏰ [cron job 'self-upkeep' fired]"),
            "owner child must NOT contain a cron-fired note (PR-B silence), got: {conv_jsonl}"
        );
        assert!(
            !conv_jsonl.contains("trunk turn reply"),
            "owner child must NOT contain the cron turn's reply text (PR-B silence), got: {conv_jsonl}"
        );
        assert!(
            !conv_jsonl.contains("organize your memory"),
            "trunk payload must NOT leak into the owner child session"
        );

        drop(principal);
    }

    /// F3 regression (2026-08-07 field test): a one-shot
    /// (`delete_after_run`) SpawnTool job must be reaped after its fire
    /// even though the fire path returns "failed" (here: no
    /// `ExtensionCore`, so the spawn refuses immediately). The old
    /// `status == "success"` gate parked such jobs on the 100-year
    /// sentinel forever.
    #[tokio::test]
    async fn test_one_shot_spawn_tool_job_reaped_after_failed_fire() {
        let tmp = TempDir::new().unwrap();
        let engine_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let scheduler =
            Arc::new(CronScheduler::new(engine_resolver.cron_schedule("crony")).unwrap());
        // No ExtensionCore (Weak::new) and no PrincipalManager: the
        // spawn refuses immediately with "failed".
        let engine = CronEngine::new(
            engine_resolver,
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            None,
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        );

        let job = CronJob {
            id: "job-one-shot".to_string(),
            name: "one-shot".to_string(),
            principal_id: PrincipalId("crony".to_string()),
            schedule: peko_cron::ScheduleKind::At {
                // `add_job` validates `at` is in the future; `next_run`
                // (what the poll loop keys on) is separately in the past
                // so the job is due immediately.
                at: (Utc::now() + Duration::hours(1)).to_rfc3339(),
            },
            action: CronJobAction::SpawnTool {
                tool_name: "Bash".to_string(),
                tool_params: serde_json::json!({"command": "true"}),
                wake_on_completion: Some(false),
                timeout_secs: Some(60),
            },
                        delete_after_run: true,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - Duration::minutes(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();
        // No PrincipalManager in this test: install the scheduler into
        // the engine's cache so `all_schedulers` (test fallback path)
        // can see it.
        engine
            .install_scheduler_for_test(&PrincipalId("crony".into()), scheduler.clone())
            .await;

        engine.check_and_run().await.unwrap();

        // PR 2: poll until the one-shot job is reaped. The deletion
        // happens inside the spawned execution task, so we cannot
        // assert synchronously anymore.
        let mut reaped = false;
        for _ in 0..50 {
            if scheduler.get_job(&job.id).unwrap().is_none() {
                reaped = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            reaped,
            "one-shot job must be reaped after its fire regardless of status"
        );
        let runs = scheduler.get_run_history(&job.id, 10).unwrap();
        assert_eq!(runs.len(), 1, "exactly one run row, got: {runs:?}");
        assert_eq!(runs[0].status, "failed");
        assert!(runs[0].finished_at.is_some());
    }

    /// Minimal stub tool so `dispatch_tool` can resolve a tool instance
    /// for the wake-attribution test below.
    struct CronStubTool;

    #[async_trait::async_trait]
    impl peko_tools_core::Tool for CronStubTool {
        fn name(&self) -> &str {
            "cron_stub"
        }
        fn description(&self) -> String {
            "stub tool for cron wake-attribution tests".to_string()
        }
        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    /// Phase 3b (2026-08-15): a SpawnTool job with
    /// `wake_on_completion=true` posts its completion steer into the
    /// principal's TRUNK inbox (`root:self`) — NOT the owner's
    /// conversational root inbox (`root:{owner}`). PEKO.md §K: cron
    /// Send (target="trunk") and SpawnTool wakes must both target the
    /// principal's root; two "roots" for one PEKO is a contract
    /// violation. The test principal has no live trunk run, which also
    /// pins the no-active-run behavior: `InboxRegistry::get_or_create`
    /// creates the inbox on demand and the message waits there.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_spawn_tool_wake_posts_to_trunk_inbox() {
        let tmp = TempDir::new().unwrap();
        // Build the manager WITHOUT `init_global_core` (unlike
        // `setup_principal_manager`): this test never runs an agentic
        // turn — `run_spawn_tool_job` only reads the principal config
        // and dispatches through the `ExtensionCore` passed to the
        // engine — so it must not replace the process-wide core that
        // parallel agentic-loop tests register hooks on (test-build
        // `init_global_core` overwrites the global; pre-existing race
        // surfaced in
        // `after_agent_hook_fires_from_loop_with_agent_name_and_did`).
        let path_resolver = PathResolver::with_dirs(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let catalog_path = tmp.path().join("models.toml");
        let (llm_resolver, _adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(llm_resolver),
        );
        let workspace = tmp.path().join("principals");
        let principal = create_test_principal(&manager, &workspace, "crony").await;

        // Executor wired to an inbox registry the test can inspect.
        let registry =
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry();
        let executor = Arc::new(AsyncExecutor::new(registry.clone()));

        // ExtensionCore with the stub tool registered so the dispatch
        // resolves an instance. The capability grant is irrelevant
        // here: the wake steer fires on ANY terminal status (success or
        // gate rejection), and this test pins the destination key only.
        let core = Arc::new(ExtensionCore::new());
        core.insert_tool_instance("cron_stub".to_string(), Arc::new(CronStubTool))
            .await;

        let resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let engine = CronEngine::new(
            resolver,
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            Some(manager.clone()),
            executor,
            Arc::downgrade(&core),
        );

        let job = CronJob {
            id: "job-wake-trunk".to_string(),
            name: "wake-trunk".to_string(),
            principal_id: PrincipalId::from_did(&principal.did().await),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "cron_stub".to_string(),
                tool_params: serde_json::json!({}),
                wake_on_completion: Some(true),
                timeout_secs: Some(60),
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

        let (status, receipt) = engine.run_spawn_tool_job(&job).await.unwrap();
        assert_eq!(status, "running");
        let task_id = receipt.expect("spawn receipt carries the task id");

        // Poll up to 2s for the steer message to land in the trunk
        // inbox (the closure runs in the background).
        let trunk_key = crate::principal::routers::root::trunk_session_id();
        for _ in 0..200 {
            let trunk_inbox = registry.get_or_create(&trunk_key).await;
            if !trunk_inbox.is_empty().await {
                let items = trunk_inbox.drain_all().await;
                assert_eq!(
                    items.len(),
                    1,
                    "expected exactly one steer message in the trunk inbox"
                );
                match &items[0] {
                    peko_extension_api::AsyncInboxItem::Steering(s) => {
                        assert!(s.content.contains("wake-trunk"));
                        assert!(s.content.contains(&task_id));
                    }
                    other => panic!("expected AsyncInboxItem::Steering, got {other:?}"),
                }

                // The owner's peer-child inbox must stay empty —
                // pre-3b the steer landed in the owner's
                // conversational inbox instead of the trunk's. (No
                // peer child exists in this test — nothing was ever
                // sent — so any non-trunk inbox key stays empty; the
                // trunk assertion above is the load-bearing one.)
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for steer message in {trunk_key} inbox");
    }

    /// PR 2: `execute_job_for_id` finds the owning principal's
    /// scheduler, returns a fresh `run_id`, and actually fires the
    /// job (a closed success row appears in the run history).
    #[tokio::test]
    async fn execute_job_for_id_fires_job_for_loaded_principal() {
        let tmp = TempDir::new().unwrap();
        let manager = setup_principal_manager(&tmp).await;
        let workspace = tmp.path().join("principals");
        let principal = create_test_principal(&manager, &workspace, "crony").await;

        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        let resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let scheduler = Arc::new(CronScheduler::new(resolver.cron_schedule("crony")).unwrap());
        let engine = CronEngine::new(
            resolver,
            idle,
            obs,
            Some(manager.clone()),
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        );
        // No jobs registered yet — install the scheduler so the
        // engine can find the job after the manual trigger.
        engine
            .install_scheduler_for_test(&PrincipalId::from_did(&principal.did().await), scheduler.clone())
            .await;

        let job = peko_cron::CronJob {
            id: "job-abc".to_string(),
            name: "manual-target".to_string(),
            principal_id: PrincipalId::from_did(&principal.did().await),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "Bash".to_string(),
                tool_params: serde_json::json!({"command": "true"}),
                wake_on_completion: Some(false),
                timeout_secs: Some(60),
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + Duration::hours(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        // Manual trigger returns a non-empty run_id immediately.
        let run_id = engine.execute_job_for_id("job-abc").await.unwrap();
        assert!(!run_id.is_empty());

        // And the work actually happens (poll until finalized).
        // Sprint 7 Commit D: the original assertion was `status == "success"`
        // (Notify was a no-tool-call delivery). SpawnTool jobs run through
        // `AsyncExecutor` → `ExtensionCore` → `ToolRuntime` which this test
        // intentionally does not bootstrap, so the job lands on `"failed"`.
        // The plumbing being verified here is `execute_job_for_id` itself —
        // any terminal status other than "running" proves the job fired.
        let mut runs: Vec<peko_cron::CronRun> = Vec::new();
        for _ in 0..50 {
            runs = scheduler.get_run_history("job-abc", 10).unwrap();
            if runs.len() >= 1 && runs[0].status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            runs.len(),
            1,
            "manual trigger must produce exactly one run row, got: {runs:?}"
        );
        assert_ne!(
            runs[0].status, "running",
            "manual trigger must reach a terminal status, got: {runs:?}"
        );
        assert!(runs[0].finished_at.is_some());

        drop(principal);
    }

    /// PR 2: `execute_job_for_id` errors when no principal owns the
    /// job (id not present in any loaded scheduler).
    #[tokio::test]
    async fn execute_job_for_id_returns_error_for_unknown_job() {
        let tmp = TempDir::new().unwrap();
        let manager = setup_principal_manager(&tmp).await;
        let workspace = tmp.path().join("principals");
        let principal = create_test_principal(&manager, &workspace, "crony").await;

        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        let resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let _scheduler = Arc::new(CronScheduler::new(resolver.cron_schedule("crony")).unwrap());
        let engine = CronEngine::new(
            resolver,
            idle,
            obs,
            Some(manager.clone()),
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        );

        // No jobs registered — the lookup must fail.
        let result = engine.execute_job_for_id("does-not-exist").await;
        assert!(result.is_err(), "unknown job id must error, got: {result:?}");

        drop(principal);
    }

    /// PR 2: `check_and_run` returns immediately even when one of the
    /// due jobs is a slow Send — the per-job execution is detached
    /// onto a `tokio::spawn`, so the poll tick is not blocked.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_and_run_does_not_block_on_slow_send_job() {
        let tmp = TempDir::new().unwrap();
        let manager = setup_principal_manager(&tmp).await;
        let workspace = tmp.path().join("principals");
        let principal = create_test_principal(&manager, &workspace, "crony").await;

        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        let resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let scheduler = Arc::new(CronScheduler::new(resolver.cron_schedule("crony")).unwrap());
        let engine = CronEngine::new(
            resolver,
            idle,
            obs,
            Some(manager.clone()),
            Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
            std::sync::Weak::new(),
        );
        engine
            .install_scheduler_for_test(&PrincipalId::from_did(&principal.did().await), scheduler.clone())
            .await;

        // One job, due immediately. We can't easily inject a sleep
        // inside `execute_job`, so the assertion is that
        // `check_and_run()` returns promptly — the spawned task runs
        // in the background.
        let job = peko_cron::CronJob {
            id: "job-quick".to_string(),
            name: "quick-send".to_string(),
            principal_id: PrincipalId::from_did(&principal.did().await),
            schedule: peko_cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::Send {
                message: "fast fire".to_string(),
                target: None,
            },
                        delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - Duration::minutes(1),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        scheduler.add_job(&job).unwrap();

        // The poll tick itself must complete in well under a second.
        let start = std::time::Instant::now();
        engine.check_and_run().await.unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "check_and_run must return promptly (per-job execution is detached), took: {elapsed:?}"
        );

        drop(principal);
    }
}
