//! Cron execution engine for the daemon
//!
//! Encapsulates job polling, idle detection, event-triggered execution,
//! delivery, and audit logging. Keeps the daemon's main loop focused on
//! lifecycle and shutdown.

use crate::common::json_utils::json_subset;
use crate::cron::events::SystemEvent;
use crate::cron::{CronJob, CronJobAction, CronRun, CronScheduler, DeliveryMode, IdleDetector};
use crate::extensions::framework::core::ExtensionCore;
use crate::observability::Observability;
use crate::principal::manager::PrincipalManager;
use crate::principal::router::{ChannelContext, ChannelKind};
use crate::tools::core::ToolResult;
use anyhow::Result;
use chrono::Utc;
use peko_auth::caller::CallerContext;
use peko_extension_host::async_exec::executor::{AsyncExecutor, AsyncTaskStatus, AsyncToolConfig};
use std::sync::{Arc, Weak};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Daemon-local status snapshot updated by the cron engine.
#[derive(Debug, Default, Clone)]
pub struct CronStatus {
    pub jobs_checked: u64,
    pub jobs_executed: u64,
    pub last_check: Option<chrono::DateTime<Utc>>,
}

/// Self-contained cron subsystem.
pub struct CronEngine {
    scheduler: Arc<CronScheduler>,
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
    status: Arc<Mutex<CronStatus>>,
    data_dir: std::path::PathBuf,
}

impl CronEngine {
    /// Create a new cron engine.
    ///
    /// `async_executor` is the daemon-shared executor used to fire
    /// `CronJobAction::SpawnTool` jobs. Pass a fresh `Arc<AsyncExecutor>`
    /// (built with `AsyncExecutor::new().with_inbox_registry(...)`) when
    /// no daemon-global executor is desired; the cron engine does not
    /// share its executor with any agent's per-call executor today.
    /// `extension_core` is held weakly so the cron engine never keeps
    /// the daemon's core alive past its natural lifetime.
    pub fn new(
        scheduler: Arc<CronScheduler>,
        idle_detector: Arc<IdleDetector>,
        observability: Arc<Observability>,
        data_dir: std::path::PathBuf,
        principal_manager: Option<Arc<PrincipalManager>>,
        async_executor: Arc<AsyncExecutor>,
        extension_core: Weak<ExtensionCore>,
    ) -> Self {
        Self {
            scheduler,
            idle_detector,
            observability,
            principal_manager,
            async_executor,
            extension_core,
            status: Arc::new(Mutex::new(CronStatus::default())),
            data_dir,
        }
    }

    /// Snapshot of current cron status.
    pub async fn status(&self) -> CronStatus {
        self.status.lock().await.clone()
    }

    // ------------------------------------------------------------------
    // Public entry points called by the daemon's select! loop
    // ------------------------------------------------------------------

    /// Check for time-based due jobs and execute them.
    pub async fn check_and_run(&self) -> Result<()> {
        let now = Utc::now();

        {
            let mut st = self.status.lock().await;
            st.jobs_checked += 1;
            st.last_check = Some(now);
        }

        let due_jobs = self.scheduler.due_jobs(now)?;
        if !due_jobs.is_empty() {
            info!("⏰ Found {} job(s) due for execution", due_jobs.len());
            for job in due_jobs {
                if let Err(e) = self.execute_job(job).await {
                    error!("Failed to execute job: {}", e);
                }
            }
        }
        Ok(())
    }

    /// Check for idle-triggered jobs and execute if conditions are met.
    pub async fn check_idle(&self) -> Result<()> {
        use crate::cron::ScheduleKind;

        let idle_jobs = self.scheduler.idle_jobs(false)?;
        if idle_jobs.is_empty() {
            return Ok(());
        }

        debug!("Checking {} idle-triggered jobs", idle_jobs.len());

        for job in idle_jobs {
            if let ScheduleKind::Idle { minutes } = &job.schedule {
                if self
                    .idle_detector
                    .is_idle(&job.principal_name, *minutes)
                    .await
                {
                    info!(
                        "⏸️  Principal '{}' idle for {} minutes, executing job '{}'",
                        job.principal_name, minutes, job.name
                    );
                    if let Err(e) = self.execute_job(job).await {
                        error!("Failed to execute idle job: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle a system event and trigger matching event-triggered jobs.
    pub async fn handle_event(&self, event: SystemEvent) -> Result<()> {
        use crate::cron::ScheduleKind;

        let event_type = event.event_type().to_string();
        debug!("Handling system event: {}", event_type);

        let event_jobs = self.scheduler.event_jobs(false)?;

        for job in event_jobs {
            if let ScheduleKind::Event {
                event_type: job_event_type,
                filter,
                once,
            } = &job.schedule
            {
                if job_event_type != &event_type {
                    continue;
                }

                if let Some(filter) = filter {
                    if !Self::event_matches_filter(&event, filter) {
                        continue;
                    }
                }

                info!("📡 Event '{}' matches job '{}'", event_type, job.name);
                if let Err(e) = self.execute_job(job.clone()).await {
                    error!("Failed to execute event-triggered job: {}", e);
                    continue;
                }

                if *once {
                    if let Err(e) = self.scheduler.set_job_enabled(&job.id, false) {
                        warn!("Failed to disable one-time job {}: {}", job.id, e);
                    } else {
                        info!("🔄 Disabled one-time event job: {}", job.name);
                    }
                }
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Job execution
    // ------------------------------------------------------------------

    async fn execute_job(&self, job: CronJob) -> Result<()> {
        info!("🔄 Executing job '{}' ({})", job.name, job.id);

        let run_id = Uuid::new_v4().to_string();
        let started_at = Utc::now();

        let _ = self
            .observability
            .audit_with_caller(
                Some(&CallerContext::local().subject()),
                "cron.execute",
                Some(&job.principal_name),
                serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "schedule": job.schedule.display(),
                    "principal": &job.principal_name,
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
        self.scheduler.record_run(&run)?;

        let result = match &job.action {
            CronJobAction::Send { .. } => self.run_send_job(&job).await,
            CronJobAction::SpawnTool { .. } => self.run_spawn_tool_job(&job).await,
        };

        let (status, output, error) = match result {
            Ok((s, o)) => (s, o, None),
            Err(e) => ("failed".to_string(), None, Some(e.to_string())),
        };

        let finished_at = Utc::now();
        let run = CronRun {
            id: run_id.clone(),
            job_id: job.id.clone(),
            started_at,
            finished_at: Some(finished_at),
            status: status.clone(),
            output: output.clone(),
            error: error.clone(),
        };
        self.scheduler.record_run(&run)?;

        let _ = self
            .observability
            .audit_with_caller(
                Some(&CallerContext::local().subject()),
                "cron.result",
                Some(&job.principal_name),
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

        let next_run = self
            .scheduler
            .calculate_next_run(&job.schedule, finished_at)?;
        self.scheduler
            .update_job_after_run(&job.id, &status, next_run)?;

        if let DeliveryMode::Announce { .. } = job.delivery {
            self.handle_delivery(&job, &status).await?;
        }

        if job.delete_after_run && status == "success" {
            info!(
                "🗑️  Deleting one-shot job '{}' after successful run",
                job.name
            );
            self.scheduler.delete_job(&job.id)?;
        }

        {
            let mut st = self.status.lock().await;
            st.jobs_executed += 1;
        }

        info!("✅ Job '{}' completed with status: {}", job.name, status);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Principal execution — Send path (CLI cron)
    // ------------------------------------------------------------------

    /// Run a [`CronJobAction::Send`] job by delivering its message to
    /// the Principal's owner root session. Equivalent to a deferred
    /// `peko send` from the daemon.
    async fn run_send_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        let Some(pm) = self.principal_manager.as_ref() else {
            return Ok((
                "failed".to_string(),
                Some("PrincipalManager not available".to_string()),
            ));
        };

        let principal = pm
            .get_by_name(&job.principal_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Principal '{}' not loaded", job.principal_name))?;

        let peer = {
            let config = principal.config.read().await;
            config.owner.clone()
        };

        let channel = ChannelContext {
            kind: ChannelKind::Cron,
            streaming: false,
        };

        match pm
            .receive(
                principal.id.clone(),
                peer,
                job.task_description(),
                channel,
                None,
            )
            .await
        {
            Ok(response) => {
                self.idle_detector
                    .record_activity(&job.principal_name)
                    .await;
                Ok(("success".to_string(), Some(response.content)))
            }
            Err(e) => Ok((
                "failed".to_string(),
                Some(format!("Principal execution error: {e}")),
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
    ///    root session (so `AsyncOutput`/`AsyncStatus`/`AsyncStop`
    ///    remain scoped to that root), and
    /// 3. on completion, posts a `SteeringMessage` into the principal's
    ///    root inbox when `wake_on_completion=true`.
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

        // The executor's inbox key needs to be the principal's root
        // session key so completion events and steer messages reach the
        // principal's owner session — same shape as `peko send`. The
        // owner subject is the same one `run_send_job` uses.
        let Some(pm) = self.principal_manager.as_ref() else {
            return Ok((
                "failed".to_string(),
                Some("PrincipalManager not available".to_string()),
            ));
        };
        let principal = match pm.get_by_name(&job.principal_name).await {
            Some(p) => p,
            None => {
                return Ok((
                    "failed".to_string(),
                    Some(format!("Principal '{}' not loaded", job.principal_name)),
                ));
            }
        };
        let owner = {
            let config = principal.config.read().await;
            config.owner.clone()
        };
        // F37: snapshot the principal's capability grants AND name at
        // fire time. The factory closure calls
        // `core.execute_tool_via_hook(...)`, which fires the capability
        // gate at `registry.rs:260-277` against these snapshotted
        // grants. Pre-F37, `tool.execute(...)` was called directly via
        // `core.get_tool(name)`, bypassing the gate. The cron engine
        // is the highest-trust caller — a scheduled job is the
        // principal's explicit authorization — so snapshot-at-fire is
        // correct (no revocation concerns between fire and tool
        // dispatch).
        let (snapshot_capabilities, snapshot_principal_id) = {
            let config = principal.config.read().await;
            let caps: Vec<String> = config
                .capabilities
                .grants
                .iter()
                .map(|c| c.0.clone())
                .collect();
            (caps, config.name.clone())
        };
        let principal_root_session_key = format!("root:{owner}");

        let wake = wake_on_completion.unwrap_or(false);
        let timeout = timeout_secs.or(Some(7200));

        let config = AsyncToolConfig {
            timeout_secs: timeout,
            wake_on_completion: wake,
            principal_root_session_key: Some(principal_root_session_key.clone()),
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
        let context = peko_extension_host::async_exec::executor::ToolDispatchContext::builder(
            tool_name.clone(),
            tool_params.clone(),
            principal_root_session_key.clone(),
        )
        .for_principal(snapshot_principal_id, snapshot_capabilities);

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
        let running = self.scheduler.list_running_runs()?;
        if running.is_empty() {
            return Ok(0);
        }

        let mut finalized = 0usize;
        for run in running {
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
            if self
                .scheduler
                .finalize_run(&run.id, &cron_status, output.clone(), error.clone())?
            {
                finalized += 1;
                self.scheduler
                    .set_job_last_status(&run.job_id, &cron_status)?;
                info!(
                    "🔁 Reconciled cron run {} (job={}) → {}",
                    run.id, run.job_id, cron_status
                );
            }
        }
        Ok(finalized)
    }

    async fn handle_delivery(&self, job: &CronJob, status: &str) -> Result<()> {
        match &job.delivery {
            DeliveryMode::Announce {
                channel,
                to,
                best_effort,
            } => {
                info!("📢 Announcing job '{}' result: {}", job.name, status);

                if *best_effort {
                    if let Err(e) =
                        self.send_announcement(job, status, channel.as_deref(), to.as_deref())
                    {
                        warn!("Failed to send announcement (best_effort=true): {}", e);
                    }
                } else {
                    self.send_announcement(job, status, channel.as_deref(), to.as_deref())?;
                }
            }
            DeliveryMode::None => {}
        }
        Ok(())
    }

    fn send_announcement(
        &self,
        job: &CronJob,
        status: &str,
        channel: Option<&str>,
        to: Option<&str>,
    ) -> Result<()> {
        let announcement = serde_json::json!({
            "type": "cron_announcement",
            "job_id": job.id,
            "job_name": job.name,
            "status": status,
            "message": job.task_description(),
            "channel": channel,
            "to": to,
            "timestamp": Utc::now().to_rfc3339(),
        });

        let announcements_dir = self.data_dir.join("announcements");
        std::fs::create_dir_all(&announcements_dir)?;

        let file_name = format!("{}_{}.json", job.id, Utc::now().timestamp());
        let file_path = announcements_dir.join(&file_name);

        let content = serde_json::to_string_pretty(&announcement)?;
        std::fs::write(&file_path, content)?;

        info!("📢 Announcement written to: {}", file_path.display());
        Ok(())
    }

    // ------------------------------------------------------------------
    // Event filtering
    // ------------------------------------------------------------------

    fn event_matches_filter(event: &SystemEvent, filter: &serde_json::Value) -> bool {
        let Ok(event_json) = serde_json::to_value(event) else {
            return false;
        };
        json_subset(&event_json, filter)
    }
}

/// Translate an `AsyncTaskStatus` into the wire string the cron
/// `CronRun.status` field has historically used.
///
/// `Completed` is collapsed to `"success"` so existing users (the CLI
/// renderer, history grep) keep matching what the `Send` path emitted.
/// Failures / timeouts / cancellations keep the executor's names so an
/// operator can correlate cron history with `AsyncOutput`.
fn map_async_status(status: AsyncTaskStatus) -> (String, Option<String>, Option<String>) {
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
        other => (
            other.as_str().to_string(),
            None,
            Some("run did not reach terminal state".to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use crate::engine::tool_runtime::ToolRuntime;
    use crate::extensions::framework::core::init_global_core;
    use crate::principal::{
        Capabilities, DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
        PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalManager, PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use crate::subject::Subject;
    use chrono::{Duration, Utc};
    use peko_auth::Exposure;
    use peko_auth::{Permission, PermissionGrant};
    use peko_providers::mock::MockAdapter;
    use peko_providers::resolver::LlmResolver;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn engine_from_tmp(tmp: &TempDir) -> CronEngine {
        let scheduler = Arc::new(CronScheduler::new(tmp.path().join("cron.json")).unwrap());
        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        CronEngine::new(
            scheduler,
            idle,
            obs,
            tmp.path().join("data"),
            None,
            Arc::new(AsyncExecutor::new()),
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
                workspace,
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
            )
            .with_resolver(resolver),
        )
    }

    async fn create_test_principal(
        manager: &PrincipalManager,
        workspace: &std::path::Path,
        name: &str,
    ) -> Arc<crate::principal::Principal> {
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
            quota: None,
        }
    }

    #[tokio::test]
    async fn test_cron_engine_creation() {
        let tmp = TempDir::new().unwrap();
        let engine = engine_from_tmp(&tmp);
        let status = engine.status().await;
        assert_eq!(status.jobs_checked, 0);
        assert_eq!(status.jobs_executed, 0);
    }

    #[tokio::test]
    async fn test_check_and_run_empty() {
        let tmp = TempDir::new().unwrap();
        let engine = engine_from_tmp(&tmp);
        assert!(engine.check_and_run().await.is_ok());
        let status = engine.status().await;
        assert_eq!(status.jobs_checked, 1);
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

        let scheduler = Arc::new(CronScheduler::new(tmp.path().join("cron.json")).unwrap());
        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        let engine = CronEngine::new(
            scheduler.clone(),
            idle,
            obs,
            tmp.path().join("data"),
            Some(manager.clone()),
            Arc::new(AsyncExecutor::new()),
            std::sync::Weak::new(),
        );

        let job = CronJob {
            id: "job-1".to_string(),
            name: "test-job".to_string(),
            principal_name: "crony".to_string(),
            schedule: crate::cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::Send {
                message: "Hello from cron".to_string(),
            },
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() - Duration::minutes(1),
            last_run: None,
            last_status: None,
            run_count: 0,
        };
        scheduler.add_job(&job).unwrap();

        engine.check_and_run().await.unwrap();

        let status = engine.status().await;
        assert_eq!(status.jobs_executed, 1);

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
        let scheduler = Arc::new(CronScheduler::new(tmp.path().join("cron.json")).unwrap());

        // Seed a SpawnTool job and a corresponding "running" run row.
        let job = CronJob {
            id: "job-recon".to_string(),
            name: "recon-job".to_string(),
            principal_name: "crony".to_string(),
            schedule: crate::cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "Agent".to_string(),
                tool_params: serde_json::json!({"prompt": "ping"}),
                wake_on_completion: Some(false),
                timeout_secs: Some(7200),
                description: Some("ping description".to_string()),
            },
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + Duration::minutes(5),
            last_run: None,
            last_status: None,
            run_count: 0,
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
        let async_executor = Arc::new(AsyncExecutor::new());
        let mut entry = peko_extension_host::async_exec::executor::registry::AsyncTaskEntry::new(
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

        let engine = CronEngine::new(
            scheduler.clone(),
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            tmp.path().join("data"),
            None,
            async_executor,
            std::sync::Weak::new(),
        );

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
        let scheduler = Arc::new(CronScheduler::new(tmp.path().join("cron.json")).unwrap());

        let job = CronJob {
            id: "job-vanished".to_string(),
            name: "vanished".to_string(),
            principal_name: "crony".to_string(),
            schedule: crate::cron::ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "Bash".to_string(),
                tool_params: serde_json::json!({}),
                wake_on_completion: Some(false),
                timeout_secs: Some(7200),
                description: None,
            },
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now() + Duration::minutes(5),
            last_run: None,
            last_status: None,
            run_count: 0,
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

        let async_executor = Arc::new(AsyncExecutor::new());
        let engine = CronEngine::new(
            scheduler.clone(),
            Arc::new(IdleDetector::new()),
            Arc::new(Observability::new("daemon")),
            tmp.path().join("data"),
            None,
            async_executor,
            std::sync::Weak::new(),
        );

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
}
