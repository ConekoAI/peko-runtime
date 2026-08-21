//! `CronCreate` tool — create scheduled jobs
//!
//! Schedules a `CronJob` through the [`CronRuntime`] port set by the
//! daemon at startup. The tool does not speak to the daemon directly;
//! per the Phase 10 plan rule, built-in tools may not import daemon
//! state.
//!
//! Supports two action kinds:
//! - `prompt` shorthand — schedules an `Agent` tool run (a `SpawnTool`
//!   job whose `tool_name="Agent"` and `params={ prompt }`).
//! - explicit `tool` + `params` — schedules any tool run.
//!
//! Plus the delivery-only `message` path (a `Notify` job — or, with
//! `target="trunk"`, a `Send` job firing a turn into the principal's
//! trunk session `root:self`; Phase 3, 2026-08-15).

use crate::tools::{
    add_job_via_runtime, build_notify_job, build_send_job, build_spawn_tool_job, global_runtime,
    resolve_delete_after_run, resolve_label, resolve_schedule_kind, validate_send_target,
};
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
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
    /// Prompt/task — required unless `tool` or `message` is provided.
    /// When supplied (and no `tool`), it is shorthand for
    /// `tool="Agent", params={ prompt }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// `Send`-action message for reminders/notifications. When set, the
    /// job delivers this message to the user (as a labeled
    /// notification) instead of running a tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `message`-only. Optional session target for the fired turn.
    /// `"trunk"` (the only accepted value) turns the job into a `Send`
    /// action whose turn lands in the principal's forever-continuous
    /// self session `root:self` instead of delivering a user-visible
    /// notification (Phase 3, 2026-08-15). Invalid with `prompt`/`tool`
    /// — SpawnTool jobs take no `target` param; their wake attribution
    /// is fixed to `root:self` (Phase 3b).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
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
    /// Whether the job recurs. `Some(false)` creates a one-shot job.
    /// When omitted, one-shotness is derived from the schedule: `at`
    /// jobs can only ever fire once, so they default to one-shot;
    /// everything else recurs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurring: Option<bool>,
    /// Whether the job persists across restarts (peko extension; default false)
    #[serde(default)]
    pub durable: bool,
    /// Legacy alias for `prompt` (peko extension, one-release support)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// Derive `delete_after_run` from the caller's one-shot hints and the
/// schedule shape. Precedence: explicit `one_shot: true` or
/// `recurring: false` → one-shot; explicit `recurring: true` →
/// recurring; otherwise an `at` schedule — which can only ever fire
/// once — defaults to one-shot (without this a fired `at` job whose
/// model caller passed no recurrence hint parks on the 100-year
/// sentinel forever; 2026-08-08 round-4 verification finding).
fn resolve_one_shot(
    params: &serde_json::Value,
    recurring: Option<bool>,
    schedule: &crate::ScheduleKind,
) -> bool {
    if resolve_delete_after_run(params) || matches!(recurring, Some(false)) {
        return true;
    }
    if matches!(recurring, Some(true)) {
        return false;
    }
    matches!(schedule, crate::ScheduleKind::At { .. })
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
        "Create a scheduled job. Supports cron expressions, one-shot 'at' times, intervals, idle triggers, and event triggers. For user-facing reminders use `message`; for background work use `prompt` or `tool`. Jobs are stored and executed by the daemon.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "REMINDERS / NOTIFICATIONS: use this for 'remind me …' requests. At fire time the message is delivered to the user as a labeled notification (visible in the next chat turn). For background work (research, edits, checks) use `prompt` or `tool` instead — those produce no user-visible output. Combine with target=\"trunk\" to fire the message as a turn in YOUR OWN trunk session (self-prompts, memory upkeep) instead of notifying the user."
                },
                "target": {
                    "type": "string",
                    "enum": ["trunk"],
                    "description": "message-only: with target=\"trunk\" the message fires as an agent turn in the principal's forever-continuous self session root:self (no user notification). Omit for normal user-facing reminders. Invalid together with prompt/tool."
                },
                "prompt": {
                    "type": "string",
                    "description": "Background task the job executes as an agent turn. Shorthand for tool=\"Agent\", params={ prompt }. Produces NO user-visible output — for reminders use `message`. Required unless `tool` or `message` is provided."
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
                    "description": "RFC3339 timestamp in the FUTURE for a one-shot scheduled job (past times are rejected). One-shot by default — the job deletes itself after firing unless recurring=true is passed. The current date/time is in your system prompt. For 'in N units' style delays prefer `delay` — no timestamp arithmetic needed."
                },
                "delay": {
                    "type": "string",
                    "description": "PREFERRED for 'in N units' requests: relative delay for a one-shot job (e.g. \"90s\", \"5m\", \"1h\", \"1d\"). Resolved to an absolute timestamp at registration time — no clock arithmetic. Cannot be combined with at/cron/interval_ms/idle_ms/event_topic."
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
                    "description": "Whether the job repeats (false creates a one-shot job). Optional: at-jobs default to one-shot, all other schedules default to recurring."
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

        let message = args.message.clone().or_else(|| {
            params
                .get("message")
                .and_then(|v| v.as_str())
                .map(String::from)
        });

        // `target` is message-only (Phase 3): `"trunk"` upgrades the
        // delivery into a `Send` turn in the principal's own trunk
        // session. With `prompt`/`tool` it is a structured error —
        // SpawnTool jobs take no `target` param; their wake attribution
        // is fixed to `root:self` (Phase 3b).
        let target = args.target.clone().or_else(|| {
            params
                .get("target")
                .and_then(|v| v.as_str())
                .map(String::from)
        });
        validate_send_target(&target)?;
        if target.is_some() && message.is_none() {
            anyhow::bail!("`target` is only valid together with `message`");
        }

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

        let schedule = resolve_schedule(&args, &params)?;
        let delete_after_run = resolve_one_shot(&params, args.recurring, &schedule);
        let label = resolve_label(&params);

        // Generate the job ID + compute next_run here. `next_run` is
        // best-effort — the daemon cron engine re-evaluates on its own
        // clock, but we precompute so the `add_job_via_runtime` response
        // shape can include a `next_run_at` field immediately.
        let job_id = format!("cron_{}", uuid::Uuid::new_v4().simple());
        let next_run = crate::tools::calculate_next_run(&schedule, chrono::Utc::now())?;

        let job = if let Some(message_text) = message {
            if let Some(target) = target {
                // Trunk-targeted Send (Phase 3): the message fires as a
                // real agent turn in the principal's self session
                // `root:self` — the mechanism by which the principal
                // schedules its own upkeep (memory organization, child
                // supervision) without user visibility.
                build_send_job(
                    job_id,
                    label,
                    peko_subject::PrincipalId(principal_id.clone()),
                    schedule,
                    message_text,
                    delete_after_run,
                    next_run,
                    Some(target),
                )
            } else {
                // Notify path (reminders/notifications): pure delivery —
                // the message text lands in the user's conversational
                // session as a labeled note at fire time. No agent turn,
                // no tokens spent (2026-08-08 unification).
                build_notify_job(
                    job_id,
                    label,
                    peko_subject::PrincipalId(principal_id.clone()),
                    schedule,
                    message_text,
                    delete_after_run,
                    next_run,
                )
            }
        } else if let Some(tool_name) = tool {
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
                job_id,
                label,
                peko_subject::PrincipalId(principal_id.clone()),
                schedule,
                tool_name,
                final_params,
                delete_after_run,
                next_run,
                args.wake_on_completion,
                args.timeout_secs,
                args.description.or(prompt.clone()),
            )
        } else {
            // Shorthand: prompt → SpawnTool{ tool="Agent", params={ prompt } }.
            let prompt_text = prompt.ok_or_else(|| {
                anyhow::anyhow!("CronCreate requires `message`, `prompt`, or `tool`")
            })?;
            build_spawn_tool_job(
                job_id,
                label,
                peko_subject::PrincipalId(principal_id.clone()),
                schedule,
                "Agent".to_string(),
                serde_json::json!({ "prompt": prompt_text }),
                delete_after_run,
                next_run,
                None,
                None,
                Some(prompt_text),
            )
        };
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
        // Phase 3: `target` is exposed and constrained to "trunk".
        let target = props.get("target").expect("target param");
        assert_eq!(target["enum"], serde_json::json!(["trunk"]));
    }

    /// Phase 3: `target` parses through the typed args struct, and the
    /// value validator (shared with the DTO deserializer) rejects
    /// unknown targets before any job is built.
    #[test]
    fn test_target_arg_parsing_and_validation() {
        let args: CronCreateArgs =
            serde_json::from_value(json!({"message": "m", "target": "trunk"})).unwrap();
        assert_eq!(args.target.as_deref(), Some("trunk"));
        validate_send_target(&args.target).unwrap();

        let args: CronCreateArgs = serde_json::from_value(json!({"message": "m"})).unwrap();
        assert_eq!(args.target, None);
        validate_send_target(&args.target).unwrap();

        let args: CronCreateArgs =
            serde_json::from_value(json!({"message": "m", "target": "bogey"})).unwrap();
        assert!(validate_send_target(&args.target).is_err());
    }

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
        assert!(resolve_one_shot(&serde_json::json!({}), None, &at));
        assert!(!resolve_one_shot(&serde_json::json!({}), None, &every));

        // Explicit hints beat the schedule-derived default.
        assert!(!resolve_one_shot(&serde_json::json!({}), Some(true), &at));
        assert!(resolve_one_shot(&serde_json::json!({}), Some(false), &every));
        assert!(resolve_one_shot(
            &serde_json::json!({"one_shot": true}),
            Some(true),
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
        assert!(resolve_one_shot(&serde_json::json!({}), None, &schedule));

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
