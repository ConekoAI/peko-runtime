//! `CronCreate` tool — create scheduled jobs
//!
//! Delegates to the daemon via IPC; the daemon is the source of truth for
//! cron persistence and execution. Jobs are always scoped to the current
//! Principal (taken from the tool execution context).
//!
//! Supports two action kinds:
//! - `prompt` shorthand — schedules an `Agent` tool run (a `SpawnTool`
//!   job whose `tool_name="Agent"` and `params={ prompt }`).
//! - explicit `tool` + `params` — schedules any tool run.

use crate::tools::builtin::cron::{
    build_spawn_tool_job, register_job_via_daemon, resolve_delete_after_run, resolve_label,
    resolve_schedule_kind,
};
use crate::tools::core::exec::ToolContext;
use crate::tools::core::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronCreateArgs {
    /// Prompt/task/message — required unless `tool` is provided.
    /// When supplied (and no `tool`), it is shorthand for
    /// `tool="Agent", params={ prompt }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Tool name to invoke at fire time. When provided, the job is a
    /// `SpawnTool` job calling this tool with `params`. When omitted
    /// and `prompt` is non-empty, defaults to `"Agent"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Tool-call parameters for `SpawnTool` jobs. Defaults to `{}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// `SpawnTool`-only. Whether to post a steer message into the
    /// principal's root inbox when the scheduled run completes
    /// (default `false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_on_completion: Option<bool>,
    /// `SpawnTool`-only. Per-run timeout in seconds. Defaults to the
    /// executor's `7200s` policy when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Human-readable description surfaced in the steer message that
    /// wakes the principal on completion. Falls back to the
    /// `prompt`/`label`/`job.name` if absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
                    "description": "Task or message the scheduled job should execute. Shorthand for tool=\"Agent\", params={ prompt }. Required unless `tool` is provided."
                },
                "tool": {
                    "type": "string",
                    "description": "Tool name to invoke at fire time (e.g. \"Agent\", \"Bash\", \"Read\"). When provided, the job calls this tool with `params`."
                },
                "params": {
                    "type": "object",
                    "description": "Tool-call parameters passed to `tool` at fire time. Defaults to {} when omitted."
                },
                "wake_on_completion": {
                    "type": "boolean",
                    "description": "SpawnTool-only: post a steer message into the principal's root inbox when the run completes. Defaults to false for cron-spawned runs."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "SpawnTool-only: per-run timeout in seconds. Defaults to the executor's 7200s policy."
                },
                "description": {
                    "type": "string",
                    "description": "Human-readable description surfaced in the wake-on-completion steer message. Falls back to the prompt or label."
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
            }
        })
    }

    /// F33: cron DB write — opt out of parallel dispatch. Concurrent
    /// `CronCreate` with the same job name races on the uniqueness
    /// check; interleaving with `CronDelete` by id can land in a
    /// half-applied state.
    fn parallelizable(&self) -> bool {
        false
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

        let prompt = args
            .prompt
            .clone()
            .or_else(|| args.task.clone())
            .or_else(|| {
                params
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });

        let tool = args.tool.clone().or_else(|| {
            params
                .get("tool")
                .and_then(|v| v.as_str())
                .map(String::from)
        });

        let tool_params = args.params.clone().unwrap_or_else(|| {
            if tool.is_some() {
                serde_json::json!({})
            } else {
                serde_json::Value::Null
            }
        });

        let schedule = resolve_schedule_kind(&params)?;
        let delete_after_run = resolve_delete_after_run(&params);
        let label = resolve_label(&params);

        let job = if let Some(tool_name) = tool {
            // Explicit SpawnTool path.
            let final_params = if prompt.is_some() && args.params.is_none() {
                // When the caller omits `params` but supplies `prompt`,
                // pass the prompt as a top-level `prompt` field —
                // matches the `Agent` tool's contract.
                let mut p = serde_json::Map::new();
                if let Some(p_text) = &prompt {
                    p.insert("prompt".to_string(), Value::String(p_text.clone()));
                }
                Value::Object(p)
            } else {
                tool_params
            };
            build_spawn_tool_job(
                label,
                tool_name,
                final_params,
                args.wake_on_completion,
                args.timeout_secs,
                args.description.or(prompt.clone()),
                schedule,
                delete_after_run,
                principal_name,
            )?
        } else {
            // Shorthand: prompt → SpawnTool{ tool="Agent", params={ prompt } }.
            let prompt_text = prompt
                .ok_or_else(|| anyhow::anyhow!("CronCreate requires either `prompt` or `tool`"))?;
            build_spawn_tool_job(
                label,
                "Agent".to_string(),
                serde_json::json!({ "prompt": prompt_text }),
                None,
                None,
                Some(prompt_text),
                schedule,
                delete_after_run,
                principal_name,
            )?
        };
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
        // The schema documents `prompt` and `tool` as optional; callers
        // must supply at least one of them, but the JSON Schema stays
        // open so the agent can omit both and recover from a missing
        // `task` alias.
        assert!(params.get("required").is_none());
        let props = params.get("properties").unwrap();
        assert!(props.get("prompt").is_some());
        assert!(props.get("tool").is_some());
        assert!(props.get("wake_on_completion").is_some());
        assert!(props.get("timeout_secs").is_some());
    }
}
