//! `peko_tools_builtin::cron` — Cron tool surface + `CronRuntime` port.
//!
//! Phase 10b extracts the four cron tools (`cron.rs` helpers +
//! `CronCreateTool`, `CronDeleteTool`, `CronListTool`) out of root.
//! Per the Phase 10 plan rule ("Built-ins must not import daemon
//! state"), the tools here do NOT call `crate::ipc::DaemonClient`
//! directly. They speak to a runtime port trait
//! ([`CronRuntime`]) that the daemon side implements.
//!
//! ## DTOs
//!
//! [`ScheduleKind`], [`DeliveryMode`], [`CronJobAction`], and
//! [`CronJob`] are serialization-friendly types shared between the
//! tool side (peko-tools-builtin) and the daemon side (root's
//! `src/cron/mod.rs`). For Phase 10b the daemon side keeps its own
//! copy and re-exports these four from peko-tools-builtin via
//! `pub use peko_tools_builtin::cron::{ScheduleKind, DeliveryMode,
//! CronJobAction, CronJob};` — single source of truth going forward.
//! A compile-time JSON-roundtrip test pins the two sides' shapes
//! together.
//!
//! ## Port
//!
//! [`CronRuntime`] is the three-method surface the cron tools need:
//! add / delete / list. The daemon implements it (see
//! `src/cron/daemon_adapter.rs`).
//!
//! ## What stays in root
//!
//! `CronScheduler`, `CronDatabase`, `CronRun`, and the cron event
//! trigger / idle detection submodules are daemon-internal state
//! and stay in `src/cron/` / `src/daemon/cron_engine/`. Only the
//! serialization-friendly DTOs and the tool surface lift to
//! peko-tools-builtin.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

// ─── DTOs (canonical home; root re-exports these) ─────────────────

/// Schedule kinds for cron jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleKind {
    /// One-shot at specific time.
    At { at: String },
    /// Recurring interval in milliseconds.
    Every { every_ms: u64 },
    /// Cron expression with optional timezone.
    Cron { expr: String, tz: Option<String> },
    /// Trigger when a Principal has been idle for N minutes.
    Idle { minutes: u64 },
    /// Trigger when specific system event occurs.
    Event {
        event_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<serde_json::Value>,
        #[serde(default)]
        once: bool,
    },
}

impl ScheduleKind {
    /// Get display name for the schedule.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::At { at } => format!("at {at}"),
            Self::Every { every_ms } => {
                let secs = every_ms / 1000;
                if secs < 60 {
                    format!("every {secs}s")
                } else if secs < 3600 {
                    format!("every {}m", secs / 60)
                } else {
                    format!("every {}h", secs / 3600)
                }
            }
            Self::Cron { expr, tz } => {
                if let Some(tz) = tz {
                    format!("cron '{expr}' ({tz})")
                } else {
                    format!("cron '{expr}'")
                }
            }
            Self::Idle { minutes } => {
                format!("idle {minutes}m")
            }
            Self::Event {
                event_type,
                filter,
                once,
            } => {
                let filter_info = filter.as_ref().map_or("", |_| " [filtered]");
                let once_info = if *once { " (once)" } else { "" };
                format!("event '{event_type}'{filter_info}{once_info}")
            }
        }
    }
}

/// Delivery configuration for job results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// No delivery, silent execution.
    #[default]
    None,
    /// Announce results to channel.
    Announce {
        channel: Option<String>,
        to: Option<String>,
        best_effort: bool,
    },
}

/// What a cron job does when it fires.
///
/// One shape covers both surfaces:
/// - CLI cron (`peko cron add …`) writes a [`Self::Send`] job — at fire
///   time the daemon delivers `message` to the Principal's owner root
///   session as a user-message, exactly like a deferred `peko send`.
/// - Agent cron (`CronCreate` tool) writes a [`Self::SpawnTool`] job —
///   at fire time the daemon asks the `AsyncExecutor` to run
///   `tool_name` with `tool_params`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CronJobAction {
    /// Deliver a user-message to the Principal's owner root session.
    Send { message: String },
    /// Schedule an async tool run attributed to the Principal's root.
    SpawnTool {
        tool_name: String,
        #[serde(default)]
        tool_params: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake_on_completion: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl CronJobAction {
    /// Short, human-readable kind label for list rendering.
    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Send { .. } => "send",
            Self::SpawnTool { .. } => "spawn_tool",
        }
    }

    /// Whether the action is a [`Self::Send`].
    #[must_use]
    pub fn is_send(&self) -> bool {
        matches!(self, Self::Send { .. })
    }

    /// Whether the action is a [`Self::SpawnTool`].
    #[must_use]
    pub fn is_spawn_tool(&self) -> bool {
        matches!(self, Self::SpawnTool { .. })
    }
}

/// A scheduled cron job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    #[serde(rename = "principal")]
    pub principal_name: String,
    pub schedule: ScheduleKind,
    #[serde(flatten)]
    pub action: CronJobAction,
    pub delivery: DeliveryMode,
    pub delete_after_run: bool,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub run_count: u32,
}

impl CronJob {
    /// Whether the job's action is [`CronJobAction::Send`].
    #[must_use]
    pub fn is_send(&self) -> bool {
        self.action.is_send()
    }

    /// Whether the job's action is [`CronJobAction::SpawnTool`].
    #[must_use]
    pub fn is_spawn_tool(&self) -> bool {
        self.action.is_spawn_tool()
    }

    /// A short description for the steer message body. Falls back to
    /// the job's `name` and finally a generic label.
    #[must_use]
    pub fn task_description(&self) -> String {
        match &self.action {
            CronJobAction::Send { message } if !message.is_empty() => message.clone(),
            CronJobAction::SpawnTool { description, .. } if description.is_some() => {
                description.clone().unwrap()
            }
            _ => format!("scheduled job '{}'", self.name),
        }
    }
}

// ─── CronRuntime port trait ────────────────────────────────────────

/// Runtime port the cron tools use to talk to the daemon.
///
/// The daemon implements this (see `src/cron/daemon_adapter.rs`).
/// Production deployments inject a real implementation; tests can
/// substitute an in-memory mock. Object-safe so the engine holds
/// `Arc<dyn CronRuntime>`.
#[async_trait]
pub trait CronRuntime: Send + Sync {
    /// Register a new cron job. Returns the assigned job ID.
    async fn add_job(&self, job: CronJob) -> Result<String>;

    /// Delete a cron job by ID. Returns `Ok(())` whether the job
    /// existed or not (idempotent).
    async fn delete_job(&self, job_id: &str) -> Result<()>;

    /// List all cron jobs (across all principals — call sites filter
    /// by `principal_name` if needed).
    async fn list_jobs(&self) -> Result<Vec<CronJob>>;
}

// ─── Public helpers used by the cron tools ────────────────────────

/// Normalize a 5-field cron expression to the 7-field format required
/// by the `cron` crate.
///
/// The `cron` crate v0.12 expects: `sec min hour day month weekday year`.
/// Standard crontab uses: `min hour day month weekday`. This helper
/// adds `0` for seconds and `*` for year when a 5-field expression
/// is detected. Expressions with 6 or 7 fields are left unchanged.
pub fn normalize_cron_expr(expr: &str) -> String {
    let trimmed = expr.trim();
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    match parts.len() {
        5 => format!("0 {trimmed} *"),
        _ => trimmed.to_string(),
    }
}

/// Build a `Send`-action [`CronJob`] from caller parameters.
///
/// `next_run` is precomputed by the caller (the cron engine will
/// re-evaluate on its own clock, but the initial schedule fires
/// from this value).
#[allow(clippy::too_many_arguments)]
pub fn build_send_job(
    id: String,
    name: String,
    principal_name: String,
    schedule: ScheduleKind,
    message: String,
    delivery: DeliveryMode,
    delete_after_run: bool,
    next_run: DateTime<Utc>,
) -> CronJob {
    CronJob {
        id,
        name,
        principal_name,
        schedule,
        action: CronJobAction::Send { message },
        delivery,
        delete_after_run,
        enabled: true,
        created_at: Utc::now(),
        next_run,
        last_run: None,
        last_status: None,
        run_count: 0,
    }
}

/// Build a `SpawnTool`-action [`CronJob`] from caller parameters.
#[allow(clippy::too_many_arguments)]
pub fn build_spawn_tool_job(
    id: String,
    name: String,
    principal_name: String,
    schedule: ScheduleKind,
    tool_name: String,
    tool_params: serde_json::Value,
    delivery: DeliveryMode,
    delete_after_run: bool,
    next_run: DateTime<Utc>,
    wake_on_completion: Option<bool>,
    timeout_secs: Option<u64>,
    description: Option<String>,
) -> CronJob {
    CronJob {
        id,
        name,
        principal_name,
        schedule,
        action: CronJobAction::SpawnTool {
            tool_name,
            tool_params,
            wake_on_completion,
            timeout_secs,
            description,
        },
        delivery,
        delete_after_run,
        enabled: true,
        created_at: Utc::now(),
        next_run,
        last_run: None,
        last_status: None,
        run_count: 0,
    }
}

/// Resolve a schedule kind from `CronCreate` tool parameters.
pub fn resolve_schedule_kind(params: &serde_json::Value) -> Result<ScheduleKind> {
    use std::str::FromStr;

    // 'at' takes precedence
    if let Some(time_str) = params.get("at").and_then(|v| v.as_str()) {
        let _at_time = DateTime::parse_from_rfc3339(time_str)
            .map_err(|e| anyhow::anyhow!("Invalid 'at' time format (use RFC3339): {e}"))?;
        return Ok(ScheduleKind::At {
            at: time_str.to_string(),
        });
    }

    // 'interval_ms'
    if let Some(interval_ms) = params.get("interval_ms").and_then(|v| v.as_u64()) {
        return Ok(ScheduleKind::Every {
            every_ms: interval_ms,
        });
    }

    // 'cron' expression
    if let Some(expr) = params.get("cron").and_then(|v| v.as_str()) {
        let normalized = normalize_cron_expr(expr);
        let _ = cron::Schedule::from_str(&normalized)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;
        let tz = params
            .get("timezone")
            .and_then(|v| v.as_str())
            .map(String::from);
        return Ok(ScheduleKind::Cron {
            expr: expr.to_string(),
            tz,
        });
    }

    // 'idle_ms'
    if let Some(idle_ms) = params.get("idle_ms").and_then(|v| v.as_u64()) {
        let minutes = idle_ms / 60000;
        return Ok(ScheduleKind::Idle {
            minutes: minutes.max(1),
        });
    }

    // 'event_topic'
    if let Some(topic) = params.get("event_topic").and_then(|v| v.as_str()) {
        let filter = params.get("event_filter").cloned();
        return Ok(ScheduleKind::Event {
            event_type: topic.to_string(),
            filter,
            once: false,
        });
    }

    Err(anyhow::anyhow!(
        "No schedule provided. Supply one of: cron, at, interval_ms, idle_ms, event_topic."
    ))
}

/// Build a human-readable label from parameters or generate one.
#[must_use]
pub fn resolve_label(params: &serde_json::Value) -> String {
    params
        .get("label")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("cron-{}", Uuid::new_v4().simple()))
}

/// Resolve the task/prompt from parameters.
pub fn resolve_prompt(params: &serde_json::Value) -> Result<String> {
    params
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            params
                .get("task")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .ok_or_else(|| anyhow::anyhow!("prompt is required"))
}

/// Resolve whether the job should delete after run (one-shot).
#[must_use]
pub fn resolve_delete_after_run(params: &serde_json::Value) -> bool {
    params
        .get("one_shot")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Calculate the next run time for a schedule kind (pure function, no
/// storage access).
///
/// - `At { at }` parses the RFC3339 timestamp and returns it.
/// - `Every { every_ms }` adds the interval to `after`.
/// - `Cron { expr, tz }` uses the `cron` crate's next-occurrence logic,
///   with optional timezone resolution via `chrono-tz`.
/// - `Idle` and `Event` return a sentinel far-future timestamp
///   (100 years) so they don't get picked up by `due_jobs`.
pub fn calculate_next_run(schedule: &ScheduleKind, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    use std::str::FromStr;

    match schedule {
        ScheduleKind::At { at } => {
            let dt = DateTime::parse_from_rfc3339(at)
                .map_err(|e| anyhow::anyhow!("Invalid timestamp: {e}"))?;
            Ok(dt.with_timezone(&Utc))
        }
        ScheduleKind::Every { every_ms } => {
            Ok(after + chrono::Duration::milliseconds(*every_ms as i64))
        }
        ScheduleKind::Cron { expr, tz } => {
            let normalized = normalize_cron_expr(expr);
            let schedule = cron::Schedule::from_str(&normalized)
                .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;

            if let Some(tz_str) = tz {
                let tz: chrono_tz::Tz = tz_str
                    .parse()
                    .map_err(|e| anyhow::anyhow!("Invalid timezone: {e}"))?;
                let local_after = after.with_timezone(&tz);
                if let Some(next) = schedule.after(&local_after).next() {
                    Ok(next.with_timezone(&Utc))
                } else {
                    Err(anyhow::anyhow!("No next occurrence found"))
                }
            } else if let Some(next) = schedule.after(&after).next() {
                Ok(next)
            } else {
                Err(anyhow::anyhow!("No next occurrence found"))
            }
        }
        ScheduleKind::Idle { .. } => Ok(after + chrono::Duration::days(365 * 100)),
        ScheduleKind::Event { .. } => Ok(after + chrono::Duration::days(365 * 100)),
    }
}

/// Render a list of [`CronJob`] values into the canonical `CronList`
/// return shape shared by the CLI and the `CronList` tool.
pub fn render_job_list(jobs: Vec<CronJob>) -> serde_json::Value {
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
            let mut obj = serde_json::json!({
                "job_id": j.id,
                "label": j.name,
                "principal": j.principal_name,
                "sub_command": sub_command,
                "action": j.action.kind_label(),
                "status": status,
                "next_run_at": j.next_run.to_rfc3339(),
                "run_count": j.run_count,
            });
            let map = obj.as_object_mut().expect("object literal above");
            match &j.action {
                CronJobAction::Send { message } => {
                    map.insert(
                        "task".to_string(),
                        serde_json::Value::String(message.clone()),
                    );
                }
                CronJobAction::SpawnTool {
                    tool_name,
                    tool_params,
                    wake_on_completion,
                    timeout_secs,
                    description,
                } => {
                    map.insert(
                        "tool".to_string(),
                        serde_json::Value::String(tool_name.clone()),
                    );
                    map.insert("params".to_string(), tool_params.clone());
                    if let Some(w) = wake_on_completion {
                        map.insert(
                            "wake_on_completion".to_string(),
                            serde_json::Value::Bool(*w),
                        );
                    }
                    if let Some(t) = timeout_secs {
                        map.insert(
                            "timeout_secs".to_string(),
                            serde_json::Value::Number((*t).into()),
                        );
                    }
                    if let Some(d) = description {
                        map.insert(
                            "description".to_string(),
                            serde_json::Value::String(d.clone()),
                        );
                    }
                }
            }
            obj
        })
        .collect();

    serde_json::json!({
        "jobs": jobs_json,
        "count": jobs_json.len(),
    })
}

// ─── Submodules (the three cron tools) ────────────────────────────

pub mod create;
pub mod delete;
pub mod list;

pub use create::CronCreateTool;
pub use delete::CronDeleteTool;
pub use list::CronListTool;

/// Register a job via the runtime port. Returns the standard
/// `{"job_id", "label", "status", "next_run_at"}` JSON shape.
pub async fn add_job_via_runtime(
    runtime: &Arc<dyn CronRuntime>,
    job: CronJob,
) -> Result<serde_json::Value> {
    use serde_json::json;
    let next_run = job.next_run;
    let label = job.name.clone();
    let returned_id = runtime.add_job(job).await?;
    Ok(json!({
        "job_id": returned_id,
        "label": label,
        "status": "registered",
        "next_run_at": next_run.to_rfc3339(),
    }))
}

// ─── Global runtime registration ──────────────────────────────────

/// Global cron runtime slot. Set once at daemon startup; the
/// `CronCreateTool` / `CronDeleteTool` / `CronListTool` constructors
/// read from it.
///
/// The global is justified because the cron tools are constructed
/// by the tool factory at agent-init time (long before any tool
/// call) and the daemon's `CronEngine` is the only legitimate
/// implementation. Tests that need a different runtime should
/// construct the tools directly with `CronCreateTool::new(mock)`
/// (and skip the global path).
static RUNTIME: OnceLock<Arc<dyn CronRuntime>> = OnceLock::new();

/// Set the global cron runtime. Panics if called more than once.
pub fn set_global_runtime(runtime: Arc<dyn CronRuntime>) {
    if RUNTIME.set(runtime).is_err() {
        // Idempotent: if the same runtime is set twice, that's a
        // misconfiguration but not catastrophic. Silently no-op
        // rather than panicking in test harnesses that re-init.
    }
}

/// Read the global cron runtime. Returns `None` if not yet set
/// (factory skips the cron tools in that case).
pub fn global_runtime() -> Option<Arc<dyn CronRuntime>> {
    RUNTIME.get().cloned()
}

#[cfg(test)]
mod tests {
    //! Pin the JSON wire shape against the daemon-side mirror.
    //!
    //! Root's `src/cron/mod.rs` re-exports the same four DTOs from
    //! this module, so deserializing a value through both paths and
    //! asserting equality proves the wire shapes still match.
    use super::*;

    #[test]
    fn schedule_kind_roundtrip() {
        let cases = vec![
            ScheduleKind::At {
                at: "2026-07-21T10:00:00Z".into(),
            },
            ScheduleKind::Every { every_ms: 60_000 },
            ScheduleKind::Cron {
                expr: "0 * * * *".into(),
                tz: Some("UTC".into()),
            },
            ScheduleKind::Idle { minutes: 5 },
        ];
        for s in cases {
            let json = serde_json::to_string(&s).unwrap();
            let back: ScheduleKind = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{:?}", s), format!("{:?}", back));
        }
    }

    #[test]
    fn cron_job_roundtrip() {
        let job = CronJob {
            id: "test-1".into(),
            name: "test".into(),
            principal_name: "alice".into(),
            schedule: ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "Read".into(),
                tool_params: serde_json::json!({"path": "/tmp/x"}),
                wake_on_completion: Some(true),
                timeout_secs: Some(3600),
                description: Some("read file".into()),
            },
            delivery: DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: chrono::Utc::now(),
            next_run: chrono::Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
        };
        let json = serde_json::to_string(&job).unwrap();
        let back: CronJob = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{:?}", job), format!("{:?}", back));
    }
}
