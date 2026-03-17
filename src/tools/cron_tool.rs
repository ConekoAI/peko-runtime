//! Cron tool for agents - spec-compliant implementation
//!
//! Implements CAPABILITY_INTERFACE.md §3.13, §8
//! - Sub-commands: at, every, cron, idle, event, list, cancel
//! - Persistence to cron.json (atomic writes)
//! - Missed job handling on restart

use crate::tools::traits::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Cron tool for agent use
pub struct CronTool {
    storage: Arc<RwLock<CronStorage>>,
    instance_id: String,
}

/// Cron storage backed by JSON file
pub struct CronStorage {
    db_path: PathBuf,
    jobs: HashMap<String, CronJob>,
}

/// Cron job - spec compliant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub job_id: String,
    pub label: String,
    pub sub_command: SubCommand,
    pub task: String,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: DateTime<Utc>,
    pub run_count: u32,
    pub error_count: u32,
    // Schedule-specific fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>, // for 'at'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>, // for 'every'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>, // for 'cron'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>, // for 'cron'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>, // for 'idle'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat: Option<bool>, // for 'idle'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>, // for 'event'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>, // for 'event'
}

/// Sub-command types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubCommand {
    At,
    Every,
    Cron,
    Idle,
    Event,
}

impl std::fmt::Display for SubCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubCommand::At => write!(f, "at"),
            SubCommand::Every => write!(f, "every"),
            SubCommand::Cron => write!(f, "cron"),
            SubCommand::Idle => write!(f, "idle"),
            SubCommand::Event => write!(f, "event"),
        }
    }
}

/// Job status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Active,
    Running,
    Completed,
    Failed,
    Cancelled,
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

impl CronStorage {
    /// Create new storage at given path
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
            jobs: HashMap::new(),
        }
    }

    /// Load from disk
    pub fn load(&mut self) -> Result<()> {
        if !self.db_path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&self.db_path)
            .with_context(|| format!("Failed to read cron.json: {}", self.db_path.display()))?;

        let data: CronData =
            serde_json::from_str(&content).with_context(|| "Failed to parse cron.json")?;

        self.jobs = data
            .jobs
            .into_iter()
            .map(|j| (j.job_id.clone(), j))
            .collect();
        Ok(())
    }

    /// Save to disk (atomic write)
    pub fn save(&self, instance_id: &str) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let data = CronData {
            schema_version: 1,
            instance_id: instance_id.to_string(),
            jobs: self.jobs.values().cloned().collect(),
        };

        let content = serde_json::to_string_pretty(&data)?;

        // Atomic write: write to temp file, then rename
        let temp_path = self.db_path.with_extension("tmp");
        std::fs::write(&temp_path, content)
            .with_context(|| format!("Failed to write temp file: {}", temp_path.display()))?;

        std::fs::rename(&temp_path, &self.db_path).with_context(|| {
            format!("Failed to rename temp file to: {}", self.db_path.display())
        })?;

        Ok(())
    }

    /// Add or update job
    pub fn upsert(&mut self, job: CronJob) {
        self.jobs.insert(job.job_id.clone(), job);
    }

    /// Get job by ID
    pub fn get(&self, job_id: &str) -> Option<&CronJob> {
        self.jobs.get(job_id)
    }

    /// Get job by label
    pub fn get_by_label(&self, label: &str) -> Option<&CronJob> {
        self.jobs.values().find(|j| j.label == label)
    }

    /// Remove job
    pub fn remove(&mut self, job_id: &str) -> bool {
        self.jobs.remove(job_id).is_some()
    }

    /// List all jobs
    pub fn list(&self) -> Vec<&CronJob> {
        self.jobs.values().collect()
    }

    /// Get mutable reference to job
    pub fn get_mut(&mut self, job_id: &str) -> Option<&mut CronJob> {
        self.jobs.get_mut(job_id)
    }
}

/// Data structure for cron.json
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronData {
    pub schema_version: u32,
    pub instance_id: String,
    pub jobs: Vec<CronJob>,
}

impl CronTool {
    /// Create new cron tool
    pub fn new(db_path: impl AsRef<Path>, instance_id: String) -> Self {
        let storage = Arc::new(RwLock::new(CronStorage::new(db_path)));
        Self {
            storage,
            instance_id,
        }
    }

    /// Initialize and load existing jobs
    pub async fn init(&self) -> Result<()> {
        let mut storage = self.storage.write().await;
        storage.load()
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
            .map_err(|e| anyhow::anyhow!("Invalid time format (use RFC3339): {}", e))?;

        let job = CronJob {
            job_id: format!("cron_{}", Uuid::new_v4().simple()),
            label,
            sub_command: SubCommand::At,
            task,
            status: JobStatus::Active,
            created_at: Utc::now(),
            last_run_at: None,
            next_run_at: at_time.with_timezone(&Utc),
            run_count: 0,
            error_count: 0,
            time: Some(time_str),
            interval_ms: None,
            schedule: None,
            timezone: None,
            idle_ms: None,
            repeat: None,
            topic: None,
            filter: None,
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

        // Calculate next run
        let start_at = args
            .start_at
            .map(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
            .flatten();

        let next_run = start_at.unwrap_or_else(Utc::now);

        let job = CronJob {
            job_id: format!("cron_{}", Uuid::new_v4().simple()),
            label,
            sub_command: SubCommand::Every,
            task,
            status: JobStatus::Active,
            created_at: Utc::now(),
            last_run_at: None,
            next_run_at: next_run,
            run_count: 0,
            error_count: 0,
            time: None,
            interval_ms: Some(interval_ms),
            schedule: None,
            timezone: None,
            idle_ms: None,
            repeat: None,
            topic: None,
            filter: None,
        };

        let response = self.register_job(job).await?;
        Ok(response)
    }

    /// Handle 'cron' sub-command (crontab schedule)
    async fn handle_cron(&self, args: CronArgs) -> Result<serde_json::Value> {
        let schedule = args
            .schedule
            .ok_or_else(|| anyhow::anyhow!("schedule is required for 'cron' sub-command"))?;
        let task = args
            .task
            .ok_or_else(|| anyhow::anyhow!("task is required"))?;
        let label = args
            .label
            .unwrap_or_else(|| format!("cron-{}", Uuid::new_v4().simple()));

        // Validate cron expression
        let _ = cron::Schedule::from_str(&schedule)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression: {}", e))?;

        // Calculate next run
        let schedule = cron::Schedule::from_str(&schedule).unwrap();
        let next_run = schedule.upcoming(Utc).next().unwrap_or_else(Utc::now);

        let job = CronJob {
            job_id: format!("cron_{}", Uuid::new_v4().simple()),
            label,
            sub_command: SubCommand::Cron,
            task,
            status: JobStatus::Active,
            created_at: Utc::now(),
            last_run_at: None,
            next_run_at: next_run,
            run_count: 0,
            error_count: 0,
            time: None,
            interval_ms: None,
            schedule: Some(schedule.to_string()),
            timezone: args.timezone,
            idle_ms: None,
            repeat: None,
            topic: None,
            filter: None,
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

        let job = CronJob {
            job_id: format!("cron_{}", Uuid::new_v4().simple()),
            label,
            sub_command: SubCommand::Idle,
            task,
            status: JobStatus::Active,
            created_at: Utc::now(),
            last_run_at: None,
            next_run_at: Utc::now() + chrono::Duration::days(365 * 100), // Far future
            run_count: 0,
            error_count: 0,
            time: None,
            interval_ms: None,
            schedule: None,
            timezone: None,
            idle_ms: Some(idle_ms),
            repeat: Some(args.repeat.unwrap_or(false)),
            topic: None,
            filter: None,
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

        let job = CronJob {
            job_id: format!("cron_{}", Uuid::new_v4().simple()),
            label,
            sub_command: SubCommand::Event,
            task,
            status: JobStatus::Active,
            created_at: Utc::now(),
            last_run_at: None,
            next_run_at: Utc::now() + chrono::Duration::days(365 * 100), // Far future
            run_count: 0,
            error_count: 0,
            time: None,
            interval_ms: None,
            schedule: None,
            timezone: None,
            idle_ms: None,
            repeat: None,
            topic: Some(topic),
            filter: args.filter,
        };

        let response = self.register_job(job).await?;
        Ok(response)
    }

    /// Handle 'list' sub-command
    async fn handle_list(&self) -> Result<serde_json::Value> {
        let storage = self.storage.read().await;
        let jobs: Vec<_> = storage
            .list()
            .into_iter()
            .map(|j| {
                json!({
                    "job_id": j.job_id,
                    "label": j.label,
                    "sub_command": j.sub_command.to_string(),
                    "task": j.task,
                    "status": format!("{:?}", j.status).to_lowercase(),
                    "next_run_at": j.next_run_at.to_rfc3339(),
                    "run_count": j.run_count,
                })
            })
            .collect();

        Ok(json!({
            "jobs": jobs,
            "count": jobs.len(),
        }))
    }

    /// Handle 'cancel' sub-command
    async fn handle_cancel(&self, args: CronArgs) -> Result<serde_json::Value> {
        let mut storage = self.storage.write().await;

        let job_id = if let Some(id) = args.job_id {
            id
        } else if let Some(label) = args.cancel_label {
            storage
                .get_by_label(&label)
                .ok_or_else(|| anyhow::anyhow!("Job with label '{}' not found", label))?
                .job_id
                .clone()
        } else {
            return Err(anyhow::anyhow!(
                "Either job_id or label is required for cancel"
            ));
        };

        let removed = storage.remove(&job_id);

        if removed {
            storage.save(&self.instance_id)?;
            Ok(json!({
                "cancelled": true,
                "job_id": job_id,
            }))
        } else {
            Err(anyhow::anyhow!("Job {} not found", job_id))
        }
    }

    /// Register a job and persist
    async fn register_job(&self, job: CronJob) -> Result<serde_json::Value> {
        let next_run = job.next_run_at;
        let job_id = job.job_id.clone();
        let label = job.label.clone();

        let mut storage = self.storage.write().await;
        storage.upsert(job);
        storage.save(&self.instance_id)?;

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
        let storage = self.storage.read().await;

        let missed: Vec<CronJob> = storage
            .list()
            .into_iter()
            .filter(|j| {
                matches!(j.status, JobStatus::Active)
                    && j.next_run_at <= now
                    && j.last_run_at.is_none()
                    && matches!(j.sub_command, SubCommand::At)
            })
            .cloned()
            .collect();

        Ok(missed)
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn description(&self) -> &'static str {
        "Manage scheduled jobs: at, every, cron, idle, event, list, cancel. Persisted to cron.json."
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
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;
                self.handle_cancel(args).await
            }
            _ => {
                let args: CronArgs = serde_json::from_value(params)
                    .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;
                let sub = SubCommand::from_str(&args.sub_command_str).ok_or_else(|| {
                    anyhow::anyhow!("Unknown sub_command: {}", args.sub_command_str)
                })?;
                match sub {
                    SubCommand::At => self.handle_at(args).await,
                    SubCommand::Every => self.handle_every(args).await,
                    SubCommand::Cron => self.handle_cron(args).await,
                    SubCommand::Idle => self.handle_idle(args).await,
                    SubCommand::Event => self.handle_event(args).await,
                }
            }
        }
    }
}

impl SubCommand {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "at" => Some(SubCommand::At),
            "every" => Some(SubCommand::Every),
            "cron" => Some(SubCommand::Cron),
            "idle" => Some(SubCommand::Idle),
            "event" => Some(SubCommand::Event),
            _ => None,
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
        let db_path = tmp.path().join("cron.json");
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

        assert!(result.is_ok(), "cron command failed: {:?}", result);
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
        let db_path = tmp.path().join("cron.json");

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
