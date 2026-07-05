//! `CronCreate` tool — create scheduled jobs
//!
//! Delegates to the daemon via IPC; the daemon is the source of truth for
//! cron persistence and execution. Jobs are always scoped to the current
//! Principal (taken from the tool execution context).

use crate::tools::builtin::cron::{
    build_job, register_job_via_daemon, resolve_delete_after_run, resolve_label, resolve_prompt,
    resolve_schedule_kind,
};
use crate::tools::core::exec::ToolContext;
use crate::tools::core::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `CronCreate` tool — create scheduled jobs
pub struct CronCreateTool;

impl CronCreateTool {
    /// Create a new `CronCreate` tool
    pub fn new() -> Self {
        Self
    }
}

impl Default for CronCreateTool {
    fn default() -> Self {
        Self::new()
    }
}

/// `CronCreate` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronCreateArgs {
    /// Prompt/task/message the scheduled job should execute
    pub prompt: String,
    /// Human-readable label for the job
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Cron expression (5-field). Required unless `at`, `interval_ms`,
    /// `idle_ms`, or `event_topic` is provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// ISO 8601 timestamp for a one-shot scheduled job
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Interval in milliseconds for recurring jobs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    /// Optional start time for interval-based jobs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_at: Option<String>,
    /// Timezone for cron expression (default UTC)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Idle duration in milliseconds before triggering
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
    /// Event topic to subscribe to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_topic: Option<String>,
    /// Optional filter for event jobs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_filter: Option<serde_json::Value>,
    /// Whether the job recurs (default true)
    #[serde(default = "default_recurring")]
    pub recurring: bool,
    /// Whether the job persists across restarts (peko extension; default false)
    #[serde(default)]
    pub durable: bool,
    /// Legacy alias for `prompt` (peko extension, one-release support)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

fn default_recurring() -> bool {
    true
}

#[async_trait]
impl Tool for CronCreateTool {
    fn name(&self) -> &'static str {
        "CronCreate"
    }

    fn description(&self) -> String {
        "Create a scheduled job. Supports cron expressions, one-shot 'at' times, intervals, idle triggers, and event triggers. Jobs are stored and executed by the daemon.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task or message the scheduled job should execute"
                },
                "label": {
                    "type": "string",
                    "description": "Optional human-readable label for the job"
                },
                "cron": {
                    "type": "string",
                    "description": "Cron expression (5-field). Required unless at, interval_ms, idle_ms, or event_topic is provided."
                },
                "at": {
                    "type": "string",
                    "description": "ISO 8601 timestamp for a one-shot scheduled job"
                },
                "interval_ms": {
                    "type": "integer",
                    "description": "Interval in milliseconds for recurring jobs"
                },
                "start_at": {
                    "type": "string",
                    "description": "Optional start time for interval-based jobs"
                },
                "timezone": {
                    "type": "string",
                    "description": "Timezone for the cron expression (default: UTC)"
                },
                "idle_ms": {
                    "type": "integer",
                    "description": "Idle duration in milliseconds before triggering"
                },
                "event_topic": {
                    "type": "string",
                    "description": "Event topic to subscribe to"
                },
                "event_filter": {
                    "type": "object",
                    "description": "Optional filter for event-triggered jobs"
                },
                "recurring": {
                    "type": "boolean",
                    "default": true,
                    "description": "Whether the job repeats (false creates a one-shot job)"
                },
                "durable": {
                    "type": "boolean",
                    "default": false,
                    "description": "Whether the job persists across daemon restarts"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "CronCreate requires a Principal context; use execute_with_context"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value> {
        let principal_name = ctx
            .principal_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("CronCreate requires a Principal context"))?
            .clone();

        // Parse known fields first for better error messages, then fall back
        // to the flexible parameter resolution used by the legacy cron tool.
        let args: CronCreateArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid CronCreate arguments: {e}"))?;

        let prompt = if !args.prompt.is_empty() {
            args.prompt
        } else if let Some(task) = args.task {
            task
        } else {
            resolve_prompt(&params)?
        };

        let schedule = resolve_schedule_kind(&params)?;
        let delete_after_run = resolve_delete_after_run(&params);
        let label = resolve_label(&params);

        let job = build_job(label, prompt, schedule, delete_after_run, principal_name)?;
        register_job_via_daemon(job).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_create_tool_name() {
        let tool = CronCreateTool::new();
        assert_eq!(tool.name(), "CronCreate");
    }

    #[test]
    fn test_cron_create_tool_parameters() {
        let tool = CronCreateTool::new();
        let params = tool.parameters();
        assert!(params.get("properties").is_some());
        assert!(params.get("required").is_some());
    }
}
