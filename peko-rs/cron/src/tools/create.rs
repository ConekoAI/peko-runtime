//! `CronCreate` tool — create scheduled jobs
//!
//! Schedules a `CronJob` through the [`CronRuntime`] port set by the
//! daemon at startup. The tool does not speak to the daemon directly;
//! per the Phase 10 plan rule, built-in tools may not import daemon
//! state.
//!
//! Always writes a `SpawnTool` job — at fire time the daemon asks the
//! `AsyncExecutor` to run `tool_name` with `tool_params`. Caller must
//! supply `tool` and `params` (the JSON schema's `required` enforces
//! this).

use crate::tools::{
    add_job_via_runtime, build_spawn_tool_job, global_runtime, resolve_delete_after_run,
    resolve_label, resolve_schedule_kind,
};
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronCreateArgs {
    /// Tool name to invoke at fire time. Required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Tool-call parameters for the scheduled `tool` call. Defaults to `{}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Whether to post a steer message into the principal's root inbox
    /// when the scheduled run completes (default `false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_on_completion: Option<bool>,
    /// `SpawnTool`-only. Per-run timeout in seconds. Defaults to the
    /// executor's `7200s` policy when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Human-readable label for the job
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Cron expression (5-field). Required unless `at`, `interval_ms`,
    /// `idle_ms`, or `event_topic` is provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// Relative delay for a one-shot job (e.g. "90s", "5m", "1h").
    /// Resolved to an absolute `at` timestamp at registration time, so
    /// the caller never does clock arithmetic. Cannot be combined with
    /// explicit schedule fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
    /// ISO 8601 timestamp for a one-shot scheduled job
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Interval in milliseconds for recurring jobs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
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
}

/// Derive `delete_after_run` from the caller's one-shot hints and the
/// schedule shape. Precedence: explicit `one_shot: true` → one-shot;
/// otherwise an `at` schedule — which can only ever fire once —
/// defaults to one-shot (without this a fired `at` job whose model
/// caller passed no recurrence hint parks on the 100-year sentinel
/// forever; 2026-08-08 round-4 verification finding).
fn resolve_one_shot(
    params: &serde_json::Value,
    schedule: &crate::ScheduleKind,
) -> bool {
    resolve_delete_after_run(params) || matches!(schedule, crate::ScheduleKind::At { .. })
}

/// Resolve the job's schedule. `delay` (relative shorthand) wins when
/// present and is resolved to an absolute one-shot `at` NOW so the
/// model never does clock arithmetic (the top remaining turn-cost
/// driver in the 2026-08-08 round-4 verification); it is mutually
/// exclusive with explicit schedule fields — ambiguity here would
/// silently schedule the wrong thing. Otherwise delegates to
/// [`resolve_schedule_kind`].
fn resolve_schedule(
    args: &CronCreateArgs,
    params: &serde_json::Value,
) -> anyhow::Result<crate::ScheduleKind> {
    if let Some(delay) = &args.delay {
        if args.at.is_some()
            || args.cron.is_some()
            || args.interval_ms.is_some()
            || args.idle_ms.is_some()
            || args.event_topic.is_some()
        {
            anyhow::bail!(
                "`delay` cannot be combined with `at`, `cron`, `interval_ms`, `idle_ms`, or `event_topic`"
            );
        }
        let ms = crate::tools::parse_duration_ms(delay)?;
        if ms == 0 {
            anyhow::bail!("`delay` must be a positive duration (e.g. \"90s\", \"5m\")");
        }
        let at = chrono::Utc::now() + chrono::Duration::milliseconds(ms as i64);
        return Ok(crate::ScheduleKind::At {
            at: at.to_rfc3339(),
        });
    }
    resolve_schedule_kind(params)
}

#[async_trait]
impl Tool for CronCreateTool {
    fn name(&self) -> &'static str {
        "CronCreate"
    }

    fn description(&self) -> String {
        "Schedule a tool to run at a future time. The tool's parameters are passed verbatim at fire time. Supports cron expressions, one-shot 'at' times, intervals, idle triggers, and event triggers. Jobs are stored and executed by the daemon.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "description": "REQUIRED. Tool name to invoke at fire time (e.g. \"Agent\", \"Bash\", \"Read\", \"ChannelRead\"). The scheduled job calls this tool with `params` at every fire."
                },
                "params": {
                    "type": "object",
                    "description": "REQUIRED. Tool-call parameters passed to `tool` at fire time. Defaults to {} when omitted."
                },
                "wake_on_completion": {
                    "type": "boolean",
                    "description": "Whether to post a steer message into the principal's root inbox when the run completes. Defaults to false for cron-spawned runs."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Per-run timeout in seconds. Defaults to the executor's 7200s policy."
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
                    "description": "RFC3339 timestamp in the FUTURE for a one-shot scheduled job (past times are rejected). One-shot by default — the job deletes itself after firing. The current date/time is in your system prompt. For 'in N units' style delays prefer `delay` — no timestamp arithmetic needed."
                },
                "delay": {
                    "type": "string",
                    "description": "PREFERRED for 'in N units' requests: relative delay for a one-shot job (e.g. \"90s\", \"5m\", \"1h\", \"1d\"). Resolved to an absolute timestamp at registration time — no clock arithmetic. Cannot be combined with at/cron/interval_ms/idle_ms/event_topic."
                },
                "interval_ms": {
                    "type": "integer",
                    "description": "Interval in milliseconds for recurring jobs"
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
                }
            },
            "required": ["tool", "params"]
        })
    }

    /// F33: cron DB write — opt out of parallel dispatch. Concurrent
    /// `CronCreate` with the same job name races on the uniqueness
    /// check; interleaving with `CronDelete` by id can land in a
    /// half-applied state.
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "CronCreate requires a Principal context; use execute_with_context"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // **Phase B.** CronJob is keyed by the principal's stable DID,
        // not its display name. The ToolContext exposes both; we use the
        // DID so the schedule's identity survives renames.
        let principal_id = ctx
            .principal_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("CronCreate requires a Principal context"))?
            .clone();

        let runtime = global_runtime().ok_or_else(|| {
            anyhow::anyhow!("CronCreate requires the daemon's cron runtime; not initialized")
        })?;

        // Parse known fields first for better error messages, then fall back
        // to the flexible parameter resolution used by the legacy cron tool.
        let args: CronCreateArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid CronCreate arguments: {e}"))?;

        let tool = args
            .tool
            .clone()
            .ok_or_else(|| anyhow::anyhow!("CronCreate requires `tool`"))?;

        let tool_params = args.params.clone().unwrap_or_else(|| serde_json::json!({}));

        let schedule = resolve_schedule(&args, &params)?;
        let delete_after_run = resolve_one_shot(&params, &schedule);
        let label = resolve_label(&params);

        // Generate the job ID + compute next_run here. `next_run` is
        // best-effort — the daemon cron engine re-evaluates on its own
        // clock, but we precompute so the `add_job_via_runtime` response
        // shape can include a `next_run_at` field immediately.
        let job_id = format!("cron_{}", uuid::Uuid::new_v4().simple());
        let next_run = crate::tools::calculate_next_run(&schedule, chrono::Utc::now())?;

        let job = build_spawn_tool_job(
            job_id,
            label,
            peko_subject::PrincipalId(principal_id.clone()),
            schedule,
            tool,
            tool_params,
            delete_after_run,
            next_run,
            args.wake_on_completion,
            args.timeout_secs,
        );
        add_job_via_runtime(&runtime, job).await
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
        // Sprint 7 Commit D: `tool` and `params` are now REQUIRED —
        // CronCreate is a SpawnTool-only factory and there is no other
        // valid shape.
        assert_eq!(
            params.get("required"),
            Some(&serde_json::json!(["tool", "params"]))
        );
        let props = params.get("properties").unwrap();
        assert!(props.get("tool").is_some());
        assert!(props.get("params").is_some());
        assert!(props.get("wake_on_completion").is_some());
        assert!(props.get("timeout_secs").is_some());
        // Sprint 7 Commit C + D + E: dropped fields stay gone.
        assert!(props.get("prompt").is_none());
        assert!(props.get("message").is_none());
        assert!(props.get("target").is_none());
        assert!(props.get("description").is_none());
        assert!(props.get("recurring").is_none());
        assert!(props.get("durable").is_none());
        assert!(props.get("task").is_none());
        assert!(props.get("start_at").is_none());
    }

    /// Phase 3: `target` parsed through the typed args struct, and the
    /// value validator (shared with the DTO deserializer) rejected
    /// unknown targets before any job was built. **Removed in Sprint 7
    /// Commit D** — `CronCreateArgs::target` is gone (and so is
    /// `validate_send_target` from this module's imports). The CLI's
    /// `peko cron add --target` continues to call `validate_send_target`
    /// on the CLI side; see `peko-rs/cli/src/commands/cron.rs`.

    /// Round-4 verification finding (2026-08-08): `at` jobs must default
    /// to one-shot when the caller passes no recurrence hint — a fired
    /// `at` job can never fire again, so a recurring default parks it on
    /// the 100-year sentinel forever. Explicit hints always win.
    #[test]
    fn test_resolve_one_shot_derivation() {
        let at = crate::ScheduleKind::At {
            at: "2099-01-01T00:00:00Z".to_string(),
        };
        let every = crate::ScheduleKind::Every { every_ms: 60_000 };

        // No hints: at → one-shot, interval → recurring.
        assert!(resolve_one_shot(&serde_json::json!({}), &at));
        assert!(!resolve_one_shot(&serde_json::json!({}), &every));

        // Explicit `one_shot: true` beats the schedule-derived default.
        assert!(resolve_one_shot(
            &serde_json::json!({"one_shot": true}),
            &every
        ));
    }

    /// `delay` resolves to an absolute future one-shot `at` at
    /// registration time (2026-08-08 round-4 finding: model-generated
    /// RFC3339 arithmetic was the top turn-cost driver).
    #[test]
    fn test_resolve_schedule_delay() {
        // Happy path: future At, one-shot via the At default.
        let args = CronCreateArgs {
            delay: Some("90s".to_string()),
            ..Default::default()
        };
        let schedule = resolve_schedule(&args, &serde_json::json!({})).unwrap();
        let crate::ScheduleKind::At { at } = &schedule else {
            panic!("delay must resolve to At, got {schedule:?}");
        };
        let at = chrono::DateTime::parse_from_rfc3339(at).unwrap();
        let delta = at.timestamp() - chrono::Utc::now().timestamp();
        assert!((80..=100).contains(&delta), "expected ~90s out, got {delta}s");
        assert!(resolve_one_shot(&serde_json::json!({}), &schedule));

        // Conflict with an explicit schedule field is rejected.
        let args = CronCreateArgs {
            delay: Some("5m".to_string()),
            interval_ms: Some(60_000),
            ..Default::default()
        };
        assert!(resolve_schedule(&args, &serde_json::json!({})).is_err());

        // Garbage and zero durations are rejected.
        let args = CronCreateArgs {
            delay: Some("soon".to_string()),
            ..Default::default()
        };
        assert!(resolve_schedule(&args, &serde_json::json!({})).is_err());
        let args = CronCreateArgs {
            delay: Some("0s".to_string()),
            ..Default::default()
        };
        assert!(resolve_schedule(&args, &serde_json::json!({})).is_err());

        // No delay → falls through to the explicit schedule fields.
        let args = CronCreateArgs::default();
        let schedule =
            resolve_schedule(&args, &serde_json::json!({"interval_ms": 60_000})).unwrap();
        assert!(matches!(
            schedule,
            crate::ScheduleKind::Every { every_ms: 60_000 }
        ));
    }
}
