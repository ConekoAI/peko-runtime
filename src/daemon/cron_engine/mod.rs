//! Cron execution engine for the daemon
//!
//! Encapsulates job polling, idle detection, event-triggered execution,
//! delivery, and audit logging. Keeps the daemon's main loop focused on
//! lifecycle and shutdown.

use crate::auth::caller::CallerContext;
use crate::common::json_utils::json_subset;
use crate::cron::events::SystemEvent;
use crate::cron::{CronJob, CronRun, CronScheduler, DeliveryMode, IdleDetector};
use crate::observability::Observability;
use crate::principal::manager::PrincipalManager;
use crate::principal::router::{ChannelContext, ChannelKind};
use anyhow::Result;
use chrono::Utc;
use std::sync::Arc;
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
    status: Arc<Mutex<CronStatus>>,
    data_dir: std::path::PathBuf,
}

impl CronEngine {
    /// Create a new cron engine.
    pub fn new(
        scheduler: Arc<CronScheduler>,
        idle_detector: Arc<IdleDetector>,
        observability: Arc<Observability>,
        data_dir: std::path::PathBuf,
        principal_manager: Option<Arc<PrincipalManager>>,
    ) -> Self {
        Self {
            scheduler,
            idle_detector,
            observability,
            principal_manager,
            status: Arc::new(Mutex::new(CronStatus::default())),
            data_dir,
        }
    }

    /// Attach the PrincipalManager used to execute jobs.
    pub fn set_principal_manager(&mut self, pm: Arc<PrincipalManager>) {
        self.principal_manager = Some(pm);
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

        let result = self.run_job_with_principal_manager(&job).await;

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
    // Principal execution
    // ------------------------------------------------------------------

    async fn run_job_with_principal_manager(
        &self,
        job: &CronJob,
    ) -> Result<(String, Option<String>)> {
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
            .receive(principal.id.clone(), peer, job.message.clone(), channel)
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
    // Delivery
    // ------------------------------------------------------------------

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
            "message": job.message,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Permission, PermissionGrant};
    use crate::common::paths::PathResolver;
    use crate::engine::tool_runtime::ToolRuntime;
    use crate::extensions::framework::core::init_global_core;
    use crate::principal::{
        AllowedExtensions, DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
        PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalManager, PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use crate::providers::mock::MockAdapter;
    use crate::providers::resolver::LlmResolver;
    use crate::subject::Subject;
    use crate::tunnel::protocol::InstanceExposure;
    use chrono::{Duration, Utc};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn engine_from_tmp(tmp: &TempDir) -> CronEngine {
        let scheduler = Arc::new(CronScheduler::new(tmp.path().join("cron.json")).unwrap());
        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        CronEngine::new(scheduler, idle, obs, tmp.path().join("data"), None)
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

        let catalog_path = tmp.path().join("providers.toml");
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
            allowed_extensions: AllowedExtensions::default(),
            exposure: InstanceExposure::Private,
            status: None,
            permissions: vec![PermissionGrant {
                subject: Subject::Public,
                permission: Permission::Chat,
                granted_at: chrono::Utc::now().to_rfc3339(),
                granted_by: Subject::User("test-owner".to_string()),
            }],
            preferred_provider_id: None,
            preferred_model_id: None,
            transport_preference: Default::default(),
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
        );

        let job = CronJob {
            id: "job-1".to_string(),
            name: "test-job".to_string(),
            principal_name: "crony".to_string(),
            schedule: crate::cron::ScheduleKind::Every { every_ms: 60_000 },
            message: "Hello from cron".to_string(),
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
}
