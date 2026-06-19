//! Cron execution engine for the daemon
//!
//! Encapsulates job polling, idle detection, event-triggered execution,
//! delivery, and audit logging. Keeps the daemon's main loop focused on
//! lifecycle and shutdown.

use crate::agent::stateless_service::{MessageRequest, StatelessAgentService};
use crate::auth::caller::CallerContext;
use crate::common::json_utils::json_subset;
use crate::cron::events::SystemEvent;
use crate::cron::{CronJob, CronRun, CronScheduler, DeliveryMode, ExecutionTarget, IdleDetector};
use crate::observability::Observability;
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
    agent_service: Option<Arc<StatelessAgentService>>,
    enable_isolated_execution: bool,
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
        enable_isolated_execution: bool,
    ) -> Self {
        Self {
            scheduler,
            idle_detector,
            observability,
            agent_service: None,
            enable_isolated_execution,
            status: Arc::new(Mutex::new(CronStatus::default())),
            data_dir,
        }
    }

    /// Attach the agent service used to execute jobs.
    pub fn set_agent_service(&mut self, service: Arc<StatelessAgentService>) {
        self.agent_service = Some(service);
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
            if let ScheduleKind::Idle { minutes, agent_id } = &job.schedule {
                let should_execute = if let Some(agent) = agent_id {
                    self.idle_detector.is_idle(agent, *minutes).await
                } else {
                    self.idle_detector.is_global_idle(*minutes).await
                };

                if should_execute {
                    info!(
                        "⏸️  Agent idle for {} minutes, executing job '{}'",
                        minutes, job.name
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
                job.agent_id.as_deref(),
                serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "schedule": job.schedule.display(),
                    "target": format!("{:?}", job.target),
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

        let result = match job.target {
            ExecutionTarget::Main => self.execute_main_job(&job).await,
            ExecutionTarget::Isolated => {
                if self.enable_isolated_execution {
                    self.execute_isolated_job(&job).await
                } else {
                    warn!("Isolated execution disabled, skipping job {}", job.id);
                    Ok(("skipped".to_string(), None))
                }
            }
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
                job.agent_id.as_deref(),
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
    // Target dispatch
    // ------------------------------------------------------------------

    async fn execute_main_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        info!("📨 Main session job: '{}'", job.message);

        if self.agent_service.is_some() {
            return self.run_job_with_agent_service(job).await;
        }

        warn!("No agent service available for main job execution");

        let output = format!(
            "[cron:{}] System event created:\n{}\n\nEvent: cron_job from {} for agent {:?}",
            job.name, job.message, job.id, job.agent_id
        );
        info!("   System event created for main session processing");
        Ok(("success".to_string(), Some(output)))
    }

    async fn execute_isolated_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        info!("🔧 Isolated job: '{}'", job.message);

        if self.agent_service.is_some() {
            return self.run_job_with_agent_service(job).await;
        }

        self.execute_main_job(job).await
    }

    async fn run_job_with_agent_service(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        let message = &job.message;

        if let Some(service) = &self.agent_service {
            let agent_id = job
                .agent_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            let request = MessageRequest::new(&agent_id, message.clone()).with_timeout(300);

            match service.execute_message(request).await {
                Ok(result) => {
                    self.idle_detector.record_activity(&agent_id).await;
                    Ok(("success".to_string(), Some(result.content)))
                }
                Err(e) => Ok(("failed".to_string(), Some(format!("Execution error: {e}")))),
            }
        } else {
            warn!("No agent service available for cron job execution");
            Ok((
                "failed".to_string(),
                Some("Agent service not available".to_string()),
            ))
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
    use tempfile::TempDir;

    fn engine_from_tmp(tmp: &TempDir) -> CronEngine {
        let scheduler = Arc::new(CronScheduler::new(&tmp.path().join("cron.json")).unwrap());
        let idle = Arc::new(IdleDetector::new());
        let obs = Arc::new(Observability::new("daemon"));
        CronEngine::new(scheduler, idle, obs, tmp.path().join("data"), false)
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
}
