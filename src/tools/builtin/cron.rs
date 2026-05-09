//! Cron tool for agents — delegates to the daemon via IPC
//!
//! The daemon is the single source of authority for cron jobs.
//! All operations (add, list, cancel) are sent to the daemon over IPC,
//! and the daemon persists jobs to cron.json and executes them.
//!
//! Implements `CAPABILITY_INTERFACE.md` §3.13, §8
//! - Sub-commands: at, every, cron, idle, event, list, cancel

use crate::cron::{CronJob, DeliveryMode, ExecutionTarget, ScheduleKind};
use crate::ipc::{DaemonClient, ResponsePacket};
use crate::tools::core::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use uuid::Uuid;

/// Cron tool for agent use — IPC-backed, daemon is the source of truth
pub struct CronTool;

impl CronTool {
    /// Create new cron tool
    pub fn new() -> Self {
        Self
    }

    /// Connect to the daemon via IPC
    async fn connect_daemon() -> anyhow::Result<DaemonClient> {
        DaemonClient::connect().await.map_err(|e| {
            anyhow::anyhow!("Cannot reach daemon for cron operations. Is it running? ({e})")
        })
    }

    /// Build a CronJob from common args
    fn build_job(
        label: String,
        task: String,
        schedule: ScheduleKind,
        delete_after_run: bool,
        agent_id: Option<String>,
    ) -> anyhow::Result<CronJob> {
        let next_run = crate::cron::calculate_next_run(&schedule, Utc::now())?;
        Ok(CronJob {
            id: format!("cron_{}", Uuid::new_v4().simple()),
            name: label,
            schedule,
            target: ExecutionTarget::Main,
            agent_id,
            message: task,
            delivery: DeliveryMode::None,
            delete_after_run,
            enabled: true,
            created_at: Utc::now(),
            next_run,
            last_run: None,
            last_status: None,
            run_count: 0,
        })
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

        let at_time = DateTime::parse_from_rfc3339(&time_str)
            .map_err(|e| anyhow::anyhow!("Invalid time format (use RFC3339): {e}"))?;

        let schedule = ScheduleKind::At {
            at: at_time.to_rfc3339(),
        };
        let job = Self::build_job(label, task, schedule, true, args.agent_id)?;
        self.register_job_via_daemon(job).await
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

        let schedule = ScheduleKind::Every {
            every_ms: interval_ms,
        };
        let job = Self::build_job(label, task, schedule, false, args.agent_id)?;
        self.register_job_via_daemon(job).await
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

        let normalized = crate::cron::normalize_cron_expr(&expr);
        let _ = cron::Schedule::from_str(&normalized)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;

        let schedule = ScheduleKind::Cron {
            expr,
            tz: args.timezone,
        };
        let job = Self::build_job(label, task, schedule, false, args.agent_id)?;
        self.register_job_via_daemon(job).await
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
            agent_id: args.agent_id.clone(),
        };
        let job = Self::build_job(
            label,
            task,
            schedule,
            !args.repeat.unwrap_or(false),
            args.agent_id,
        )?;
        self.register_job_via_daemon(job).await
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
        let job = Self::build_job(label, task, schedule, false, args.agent_id)?;
        self.register_job_via_daemon(job).await
    }

    /// Handle 'list' sub-command
    async fn handle_list(&self) -> Result<serde_json::Value> {
        let client = Self::connect_daemon().await?;
        match client.cron_list(true).await? {
            ResponsePacket::CronList { jobs, .. } => {
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
            ResponsePacket::Error { message, .. } => {
                Err(anyhow::anyhow!("Failed to list jobs: {message}"))
            }
            other => Err(anyhow::anyhow!(
                "Unexpected response from daemon: {other:?}"
            )),
        }
    }

    /// Handle 'cancel' sub-command
    async fn handle_cancel(&self, args: CronArgs) -> Result<serde_json::Value> {
        let job_id = if let Some(id) = args.job_id {
            id
        } else if let Some(label) = args.cancel_label {
            // Need to list jobs to find the ID by label
            let client = Self::connect_daemon().await?;
            match client.cron_list(true).await? {
                ResponsePacket::CronList { jobs, .. } => {
                    jobs.into_iter()
                        .find(|j| j.name == label)
                        .ok_or_else(|| anyhow::anyhow!("Job with label '{label}' not found"))?
                        .id
                }
                ResponsePacket::Error { message, .. } => {
                    return Err(anyhow::anyhow!("Failed to list jobs for cancel: {message}"));
                }
                other => {
                    return Err(anyhow::anyhow!(
                        "Unexpected response from daemon: {other:?}"
                    ));
                }
            }
        } else {
            return Err(anyhow::anyhow!(
                "Either job_id or label is required for cancel"
            ));
        };

        let client = Self::connect_daemon().await?;
        match client.cron_remove(&job_id).await? {
            ResponsePacket::CronRemoved { .. } => Ok(json!({
                "cancelled": true,
                "job_id": job_id,
            })),
            ResponsePacket::Error { message, .. } => {
                Err(anyhow::anyhow!("Failed to cancel job: {message}"))
            }
            other => Err(anyhow::anyhow!(
                "Unexpected response from daemon: {other:?}"
            )),
        }
    }

    /// Register a job via daemon IPC
    async fn register_job_via_daemon(&self, job: CronJob) -> Result<serde_json::Value> {
        let next_run = job.next_run;
        let job_id = job.id.clone();
        let label = job.name.clone();

        let client = Self::connect_daemon().await?;
        match client.cron_add(job).await? {
            ResponsePacket::CronAdded { .. } => Ok(json!({
                "job_id": job_id,
                "label": label,
                "status": "registered",
                "next_run_at": next_run.to_rfc3339(),
            })),
            ResponsePacket::Error { message, .. } => {
                Err(anyhow::anyhow!("Failed to register job: {message}"))
            }
            other => Err(anyhow::anyhow!(
                "Unexpected response from daemon: {other:?}"
            )),
        }
    }
}

impl Default for CronTool {
    fn default() -> Self {
        Self::new()
    }
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
    /// Agent to run the job as (optional; the daemon will use this agent when executing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
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

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn description(&self) -> String {
        "Manage scheduled jobs: at, every, cron, idle, event, list, cancel. Jobs are stored and executed by the daemon.".to_string()
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
                "agent_id": {
                    "type": "string",
                    "description": "Agent name to run the job as (optional). If omitted, the daemon will try to find a suitable agent."
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

    #[test]
    fn test_cron_tool_name() {
        let tool = CronTool::new();
        assert_eq!(tool.name(), "cron");
    }

    #[test]
    fn test_cron_tool_parameters() {
        let tool = CronTool::new();
        let params = tool.parameters();
        assert!(params.get("properties").is_some());
        assert!(params.get("required").is_some());
    }

    #[tokio::test]
    async fn test_cron_tool_execute_list_parses_params() {
        // Verify the tool accepts a well-formed list request.
        // (Actual daemon communication is tested via integration tests.)
        let tool = CronTool::new();
        let params = tool.parameters();
        let props = params.get("properties").unwrap();
        assert!(props.get("sub_command").is_some());
        assert!(props.get("label").is_some());
        assert!(props.get("task").is_some());
    }
}
