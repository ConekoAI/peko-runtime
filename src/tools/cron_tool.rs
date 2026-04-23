//! Cron tool for agents - SQLite-backed implementation
//!
//! Implements `CAPABILITY_INTERFACE.md` §3.13, §8
//! - Sub-commands: at, every, cron, idle, event, list, cancel
//! - Persistence via `crate::cron::CronScheduler` (SQLite)
//! - Missed job handling on restart

use crate::cron::{CronJob, CronScheduler, DeliveryMode, ExecutionTarget, ScheduleKind};
use crate::tools::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

/// Cron tool for agent use
pub struct CronTool {
    scheduler: Arc<CronScheduler>,
    db_path: PathBuf,
}

/// Cron tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronArgs {
    #[serde(rename = "sub_command")]
    pub sub_command_str: String,
    // Common fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    // For 'at'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    // For 'every'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_at: Option<String>,
    // For 'cron'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    // For 'idle'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
    #[serde(default)]
    pub repeat: Option<bool>,
    // For 'event'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>,
    // For 'cancel'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_label: Option<String>,
}

impl CronTool {
    /// Create new cron tool
    pub fn new(db_path: impl AsRef<Path>, _instance_id: String) -> Self {
        let db_path = db_path.as_ref().to_path_buf();
        // Scheduler will be initialized in `init()`
        let scheduler = Arc::new(CronScheduler::new(&db_path).expect("Failed to create CronScheduler"));
        Self {
            scheduler,
            db_path,
        }
    }

    /// Initialize and load existing jobs
    pub async fn init(&self) -> Result<()> {
        // Scheduler is already initialized in `new`; this method exists for API compatibility.
        Ok(())
    }

    /// Create and initialize in one call
    pub async fn create(db_path: impl AsRef<Path>, instance_id: String) -> Result<Self> {
        let tool = Self::new(db_path, instance_id);
        tool.init().await?;
        Ok(tool)
    }

    /// Handle 'at' sub-command
    async fn handle_at(&self, args: CronArgs) -> Result<serde_json::Value> {
        let time_str = args
            .time
            .ok_or_else(|| anyhow::anyhow!("time is required for 'at' sub-command"))?;
        let task = args
            .task
            .ok_or_else(|| anyhow::anyhow!("task is required"))?;
        let label = args
            .label
            .unwrap_or_else(|| format!("at-{}", Uuid::new_v4().simple()));

        // Parse time
        let at_time = DateTime::parse_from_rfc3339(&time_str)
            .map_err(|e| anyhow::anyhow!("Invalid time format (use RFC3339): {e}"))?;
        let at_time = at_time.with_timezone(&Utc);

        let schedule = ScheduleKind::At { at: time_str };
        let next_run = self.scheduler.calculate_next_run(&schedule, Utc::now())?;

        let job = CronJob {
            id: format!("cron_{}", Uuid::new_v4().simple()),
            name: label.clone(),
            schedule,
            target: ExecutionTarget::Main,
            agent_id: None,
            message: task,
            delivery: DeliveryMode::None,
            delete_after_run: true,
            enabled: true,
            created_at: Utc::now(),
            next_run,
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        let response = self.register_job(job).await?;
        Ok(response)
    }

    /// Handle 'every' sub-command
    async fn handle_every(&self, args: CronArgs) -> Result<serde_json::Value> {
        let interval_ms = args
            .interval_ms
            .ok_or_else(|| anyhow::anyhow!("interval_ms is required for 'every' sub-command"))?;
        let task = args
            .task
            .ok_or_else(|| anyhow::anyhow!("task is required"))?;
        let label = args
            .label
            .unwrap_or_else(|| format!("every-{}", Uuid::new_v4().simple()));

        let schedule = ScheduleKind::Every { every_ms: interval_ms };

        // Calculate next run
        let next_run = if let Some(start_at) = args.start_at {
            DateTime::parse_from_rfc3339(&start_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now())
        } else {
            self.scheduler.calculate_next_run(&schedule, Utc::now())?
        };

        let job = CronJob {
            id: format!("cron_{}", Uuid::new_v4().simple()),
            name: label.clone(),
            schedule,
            target: ExecutionTarget::Main,
            agent_id: None,
            message: task,
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run,
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        let response = self.register_job(job).await?;
        Ok(response)
    }

    /// Handle 'cron' sub-command (crontab schedule)
    async fn handle_cron(&self, args: CronArgs) -> Result<serde_json::Value> {
        let expr = args
            .schedule
            .ok_or_else(|| anyhow::anyhow!("schedule is required for 'cron' sub-command"))?;
        let task = args
            .task
            .ok_or_else(|| anyhow::anyhow!("task is required"))?;
        let label = args
            .label
            .unwrap_or_else(|| format!("cron-{}", Uuid::new_v4().simple()));

        // Validate cron expression
        let _ = cron::Schedule::from_str(&expr)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;

        let schedule = ScheduleKind::Cron {
            expr,
            tz: args.timezone,
        };
        let next_run = self.scheduler.calculate_next_run(&schedule, Utc::now())?;

        let job = CronJob {
            id: format!("cron_{}", Uuid::new_v4().simple()),
            name: label.clone(),
            schedule,
            target: ExecutionTarget::Main,
            agent_id: None,
            message: task,
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run,
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        let response = self.register_job(job).await?;
        Ok(response)
    }

    /// Handle 'idle' sub-command
    async fn handle_idle(&self, args: CronArgs) -> Result<serde_json::Value> {
        let idle_ms = args
            .idle_ms
            .ok_or_else(|| anyhow::anyhow!("idle_ms is required for 'idle' sub-command"))?;
        let task = args
            .task
            .ok_or_else(|| anyhow::anyhow!("task is required"))?;
        let label = args
            .label
            .unwrap_or_else(|| format!("idle-{}", Uuid::new_v4().simple()));

        let minutes = idle_ms / 60000;
        let schedule = ScheduleKind::Idle {
            minutes: minutes.max(1),
            agent_id: None,
        };
        let next_run = self.scheduler.calculate_next_run(&schedule, Utc::now())?;

        let job = CronJob {
            id: format!("cron_{}", Uuid::new_v4().simple()),
            name: label.clone(),
            schedule,
            target: ExecutionTarget::Main,
            agent_id: None,
            message: task,
            delivery: DeliveryMode::None,
            delete_after_run: !args.repeat.unwrap_or(false),
            enabled: true,
            created_at: Utc::now(),
            next_run,
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        let response = self.register_job(job).await?;
        Ok(response)
    }

    /// Handle 'event' sub-command
    async fn handle_event(&self, args: CronArgs) -> Result<serde_json::Value> {
        let topic = args
            .topic
            .ok_or_else(|| anyhow::anyhow!("topic is required for 'event' sub-command"))?;
        let task = args
            .task
            .ok_or_else(|| anyhow::anyhow!("task is required"))?;
        let label = args
            .label
            .unwrap_or_else(|| format!("event-{}", Uuid::new_v4().simple()));

        let schedule = ScheduleKind::Event {
            event_type: topic,
            filter: args.filter,
            once: false,
        };
        let next_run = self.scheduler.calculate_next_run(&schedule, Utc::now())?;

        let job = CronJob {
            id: format!("cron_{}", Uuid::new_v4().simple()),
            name: label.clone(),
            schedule,
            target: ExecutionTarget::Main,
            agent_id: None,
            message: task,
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run,
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        let response = self.register_job(job).await?;
        Ok(response)
    }

    /// Handle 'list' sub-command
    async fn handle_list(&self) -> Result<serde_json::Value> {
        let jobs = self.scheduler.list_jobs(true)?;

        let jobs_json: Vec<_> = jobs
            .into_iter()
            .map(|j| {
                let sub_command = match &j.schedule {
                    ScheduleKind::At { .. } => "at",
                    ScheduleKind::Every { .. } => "every",
                    ScheduleKind::Cron { .. } => "cron",
                    ScheduleKind::Idle { .. } => "idle",
                    ScheduleKind::Event { .. } => "event",
                };
                let status = if j.enabled { "active" } else { "disabled" };
                json!({
                    "job_id": j.id,
                    "label": j.name,
                    "sub_command": sub_command,
                    "task": j.message,
                    "status": status,
                    "next_run_at": j.next_run.to_rfc3339(),
                    "run_count": j.run_count,
                })
            })
            .collect();

        Ok(json!({
            "jobs": jobs_json,
            "count": jobs_json.len(),
        }))
    }

    /// Handle 'cancel' sub-command
    async fn handle_cancel(&self, args: CronArgs) -> Result<serde_json::Value> {
        let job_id = if let Some(id) = args.job_id {
            id
        } else if let Some(label) = args.cancel_label {
            let jobs = self.scheduler.list_jobs(true)?;
            jobs.into_iter()
                .find(|j| j.name == label)
                .ok_or_else(|| anyhow::anyhow!("Job with label '{label}' not found"))?
                .id
        } else {
            return Err(anyhow::anyhow!(
                "Either job_id or label is required for cancel"
            ));
        };

        let removed = self.scheduler.delete_job(&job_id)?;

        if removed {
            Ok(json!({
                "cancelled": true,
                "job_id": job_id,
            }))
        } else {
            Err(anyhow::anyhow!("Job {job_id} not found"))
        }
    }

    /// Register a job and persist
    async fn register_job(&self, job: CronJob) -> Result<serde_json::Value> {
        let next_run = job.next_run;
        let job_id = job.id.clone();
        let label = job.name.clone();

        self.scheduler.add_job(&job)?;

        Ok(json!({
            "job_id": job_id,
            "label": label,
            "status": "registered",
            "next_run_at": next_run.to_rfc3339(),
        }))
    }

    /// Handle missed jobs on restart
    pub async fn handle_missed_jobs(&self) -> Result<Vec<CronJob>> {
        let now = Utc::now();
        let jobs = self.scheduler.list_jobs(false)?;

        let missed: Vec<CronJob> = jobs
            .into_iter()
            .filter(|j| {
                j.enabled
                    && j.next_run <= now
                    && j.last_run.is_none()
                    && matches!(j.schedule, ScheduleKind::At { .. })
            })
            .collect();

        Ok(missed)
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn description(&self) -> String {
        "Manage scheduled jobs: at, every, cron, idle, event, list, cancel. Persisted to cron.db."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "sub_command": {
                    "type": "string",
                    "enum": ["at", "every", "cron", "idle", "event", "list", "cancel"],
                    "description": "Cron sub-command"
                },
                "label": {
                    "type": "string",
                    "description": "Human-readable label for the job"
                },
                "task": {
                    "type": "string",
                    "description": "Task description/message"
                },
                "time": {
                    "type": "string",
                    "description": "ISO 8601 timestamp for 'at' sub-command"
                },
                "interval_ms": {
                    "type": "integer",
                    "description": "Interval in milliseconds for 'every' sub-command"
                },
                "start_at": {
                    "type": "string",
                    "description": "Optional start time for 'every' sub-command"
                },
                "schedule": {
                    "type": "string",
                    "description": "Cron expression (5-field) for 'cron' sub-command"
                },
                "timezone": {
                    "type": "string",
                    "description": "Timezone for 'cron' sub-command (default: UTC)"
                },
                "idle_ms": {
                    "type": "integer",
                    "description": "Idle duration in milliseconds for 'idle' sub-command"
                },
                "repeat": {
                    "type": "boolean",
                    "description": "Repeat for 'idle' sub-command (default: false)"
                },
                "topic": {
                    "type": "string",
                    "description": "Event topic for 'event' sub-command"
                },
                "filter": {
                    "type": "object",
                    "description": "Optional filter for 'event' sub-command"
                },
                "job_id": {
                    "type": "string",
                    "description": "Job ID for 'cancel' sub-command"
                },
                "cancel_label": {
                    "type": "string",
                    "description": "Label for 'cancel' sub-command (alternative to job_id)"
                }
            },
            "required": ["sub_command"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        // Extract sub_command as string first to handle list/cancel specially
        let sub_command = params
            .get("sub_command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("sub_command is required"))?;

        match sub_command {
            "list" => self.handle_list().await,
            "cancel" => {
                let args: CronArgs = serde_json::from_value(params)
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
                self.handle_cancel(args).await
            }
            "at" => {
                let args: CronArgs = serde_json::from_value(params)
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
                self.handle_at(args).await
            }
            "every" => {
                let args: CronArgs = serde_json::from_value(params)
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
                self.handle_every(args).await
            }
            "cron" => {
                let args: CronArgs = serde_json::from_value(params)
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
                self.handle_cron(args).await
            }
            "idle" => {
                let args: CronArgs = serde_json::from_value(params)
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
                self.handle_idle(args).await
            }
            "event" => {
                let args: CronArgs = serde_json::from_value(params)
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
                self.handle_event(args).await
            }
            _ => Err(anyhow::anyhow!("Unknown sub_command: {sub_command}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    async fn create_test_tool() -> (CronTool, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.db");
        let tool = CronTool::new(db_path, "test-instance".to_string());
        tool.init().await.unwrap();
        (tool, tmp)
    }

    #[tokio::test]
    async fn test_cron_at() {
        let (tool, _tmp) = create_test_tool().await;

        let future_time = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();

        let result = tool
            .execute(json!({
                "sub_command": "at",
                "time": future_time,
                "task": "Send the weekly digest",
                "label": "weekly-digest"
            }))
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response["job_id"].as_str().is_some());
        assert_eq!(response["label"].as_str().unwrap(), "weekly-digest");
        assert_eq!(response["status"].as_str().unwrap(), "registered");
    }

    #[tokio::test]
    async fn test_cron_every() {
        let (tool, _tmp) = create_test_tool().await;

        let result = tool
            .execute(json!({
                "sub_command": "every",
                "interval_ms": 3600000,
                "task": "Check for new reports",
                "label": "inbox-check"
            }))
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "registered");
    }

    #[tokio::test]
    async fn test_cron_cron() {
        let (tool, _tmp) = create_test_tool().await;

        // Use 6-field cron expression (seconds minutes hours day month day-of-week)
        // "0 0 9 * * 1-5" = At 9:00 AM on Monday through Friday (cron crate v0.12 format)
        let result = tool
            .execute(json!({
                "sub_command": "cron",
                "schedule": "0 0 9 * * 1-5",
                "timezone": "Asia/Tokyo",
                "task": "Morning standup summary",
                "label": "standup"
            }))
            .await;

        assert!(result.is_ok(), "cron command failed: {result:?}");
        let response = result.unwrap();
        assert_eq!(response["status"].as_str().unwrap(), "registered");
    }

    #[tokio::test]
    async fn test_cron_list() {
        let (tool, _tmp) = create_test_tool().await;

        // Add a job first
        tool.execute(json!({
            "sub_command": "every",
            "interval_ms": 60000,
            "task": "Test task",
            "label": "test-job"
        }))
        .await
        .unwrap();

        // List jobs
        let result = tool
            .execute(json!({
                "sub_command": "list"
            }))
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        let jobs = response["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["label"].as_str().unwrap(), "test-job");
    }

    #[tokio::test]
    async fn test_cron_cancel() {
        let (tool, _tmp) = create_test_tool().await;

        // Add a job
        let add_result = tool
            .execute(json!({
                "sub_command": "every",
                "interval_ms": 60000,
                "task": "Test task",
                "label": "to-cancel"
            }))
            .await
            .unwrap();

        let job_id = add_result["job_id"].as_str().unwrap();

        // Cancel by job_id
        let cancel_result = tool
            .execute(json!({
                "sub_command": "cancel",
                "job_id": job_id
            }))
            .await;

        assert!(cancel_result.is_ok());
        assert!(cancel_result.unwrap()["cancelled"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_persistence() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.db");

        // Create tool and add job
        {
            let tool = CronTool::new(&db_path, "test-instance".to_string());
            tool.init().await.unwrap();

            tool.execute(json!({
                "sub_command": "every",
                "interval_ms": 60000,
                "task": "Test task",
                "label": "persistent-job"
            }))
            .await
            .unwrap();
        }

        // Verify file exists
        assert!(db_path.exists());

        // Create new tool instance and verify job loads
        {
            let tool = CronTool::new(&db_path, "test-instance".to_string());
            tool.init().await.unwrap();

            let list_result = tool
                .execute(json!({
                    "sub_command": "list"
                }))
                .await
                .unwrap();

            let jobs = list_result["jobs"].as_array().unwrap();
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0]["label"].as_str().unwrap(), "persistent-job");
        }
    }
}
