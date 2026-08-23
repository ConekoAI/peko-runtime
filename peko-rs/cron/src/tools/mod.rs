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
//! [`ScheduleKind`], [`CronJobAction`], and [`CronJob`] are
//! serialization-friendly types shared between the tool side
//! (peko-tools-builtin) and the daemon side (root's
//! `src/cron/mod.rs`). For Phase 10b the daemon side keeps its own
//! copy and re-exports these three from peko-tools-builtin via
//! `pub use peko_tools_builtin::cron::{ScheduleKind, CronJobAction,
//! CronJob};` — single source of truth going forward. A
//! compile-time JSON-roundtrip test pins the two sides' shapes
//! together. (Sprint 7 Commit B dropped the `DeliveryMode` enum and
//! `CronJob.delivery` field — the engine's `Announce` side-effect
//! was unread.)
//!
//! ## Port
//!
//! [`CronRuntime`] is the three-method surface the cron tools need:
//! add / delete / list. The daemon implements it (see
//! `src/cron/daemon_adapter.rs`).
//!
//! ## What stays in root
//!
//! `CronScheduler`, `CronDatabase`, `CronRun`, and the idle
//! detection submodule are daemon-internal state and stay in
//! `src/cron/` / `src/daemon/cron_engine/`. Only the
//! serialization-friendly DTOs and the tool surface lift to
//! peko-tools-builtin.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

/// Default retry budget for cron jobs that have `max_retries: None` on
/// disk (legacy records serialized before this field was added) or
/// that have not opted into a custom limit. The engine disables a job
/// after this many consecutive failed runs. `None` on the job means
/// unlimited and preserves the legacy retry-forever behavior.
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// The only accepted value for [`CronJobAction::Send`]'s `target`
/// field: route the fired turn into the principal's trunk session
/// `root:self` instead of the default per-owner cron session
/// `root:cron:{owner}` (Phase 3, 2026-08-15).
pub const SEND_TARGET_TRUNK: &str = "trunk";

/// Validate a [`CronJobAction::Send`] `target` value supplied by a
/// caller (CLI flag, tool param). `None` (default routing) and
/// `"trunk"` are accepted; anything else is a structured error. The
/// serde deserializer below applies the same rule at JSON load time;
/// this helper covers the struct-literal construction paths that
/// bypass serde.
pub fn validate_send_target(target: &Option<String>) -> Result<()> {
    match target.as_deref() {
        None | Some(SEND_TARGET_TRUNK) => Ok(()),
        Some(other) => anyhow::bail!(
            "invalid cron Send target '{other}': only \"{SEND_TARGET_TRUNK}\" is supported"
        ),
    }
}

/// Serde field deserializer for [`CronJobAction::Send`]'s `target`:
/// applies [`validate_send_target`] at load time so a hand-edited
/// `cron.json` with an unknown target fails loudly instead of silently
/// misrouting a turn.
fn deserialize_send_target<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    validate_send_target(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

/// Minimum interval for a trunk-targeted keepalive Send job: 60s
/// (Phase 3b, 2026-08-15).
///
/// A `Send` job fires a real agent turn in the principal's trunk
/// session `root:self` on every tick — each tick is a full LLM
/// round-trip over the trunk's growing history. An `Every { every_ms
/// }` schedule with no floor is a runaway token-burn anti-pattern
/// (PEKO.md "Violates K"), so creation refuses intervals below this
/// constant. One-shot `At`, `Cron` expressions, and `Idle` schedules
/// are exempt: their cadence is explicit, not a bare self-poke loop.
///
/// Phase 7 (2026-08-17): the trunk is the DEFAULT Send target
/// (`target: None` and `Some("trunk")` are the same route), so the
/// floor applies to both.
pub const TRUNK_MIN_INTERVAL_MS: u64 = 60_000;

/// Enforce [`TRUNK_MIN_INTERVAL_MS`] on trunk-bound Send jobs with
/// an `Every` schedule. Since Phase 7 every Send job is trunk-bound
/// (`None` and `"trunk"` are the same destination); other actions and
/// schedule kinds pass through unchanged. Called from
/// `CronScheduler::add_job` so every creation surface (CLI `peko cron
/// add`, the `CronCreate` tool, in-process construction) funnels
/// through it.
pub fn validate_trunk_send_interval(
    schedule: &ScheduleKind,
    target: &Option<String>,
) -> Result<()> {
    if let Some(t) = target.as_deref() {
        if t != SEND_TARGET_TRUNK {
            // Unknown targets are rejected by `validate_send_target`;
            // the floor only concerns trunk-bound jobs.
            return Ok(());
        }
    }
    if let ScheduleKind::Every { every_ms } = schedule {
        if *every_ms < TRUNK_MIN_INTERVAL_MS {
            anyhow::bail!(
                "cron Send (trunk target) with an interval schedule requires \
                 every_ms >= {TRUNK_MIN_INTERVAL_MS} ({}s); got {every_ms}ms. \
                 A faster self-targeted keepalive burns tokens on every tick with no external \
                 input — use a cron expression or a one-shot 'at' for sub-minute timing.",
                TRUNK_MIN_INTERVAL_MS / 1000,
            );
        }
    }
    Ok(())
}

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
        }
    }
}

/// What a cron job does when it fires.
///
/// Two shapes:
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
    ///
    /// `target` (Phase 3, 2026-08-15) selects the destination session:
    /// `None` (the default — and the only value pre-Phase-3 jobs can
    /// carry) preserves the legacy behavior exactly: the turn lands in
    /// the per-owner cron session `root:cron:{owner}` and the outcome
    /// is cross-posted as a note to `root:{owner}`. `"trunk"` routes
    /// the turn into the principal's forever-continuous self session
    /// `root:self` (no separate conversation projection — the turn
    /// already IS in the principal's own session). No other value
    /// is accepted (see [`validate_send_target`]).
    Send {
        message: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_send_target"
        )]
        target: Option<String>,
    },
    /// Schedule an async tool run attributed to the Principal's root.
    SpawnTool {
        tool_name: String,
        #[serde(default)]
        tool_params: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake_on_completion: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
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
}

/// A scheduled cron job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    /// **Phase B.** The principal this job belongs to, keyed by stable
    /// `PrincipalId` (DID) rather than the legacy `principal_name:
    /// String`. The on-disk filename is still derived from the principal
    /// name (see [`crate::CronScheduler::new`]) so schedule files written
    /// before this rename round-trip through the name; the engine-level
    /// keying and the wire shape carry the DID instead.
    ///
    /// Prelaunch — no compat shim for the legacy `principal: String`
    /// field. Schedule files written before Phase B must be re-created.
    #[serde(rename = "principal_id")]
    pub principal_id: PrincipalId,
    pub schedule: ScheduleKind,
    #[serde(flatten)]
    pub action: CronJobAction,
    pub delete_after_run: bool,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub run_count: u32,
    /// Number of consecutive failed runs. Reset to 0 on a successful
    /// run by [`crate::CronScheduler::update_job_after_run`] and
    /// [`crate::CronScheduler::set_job_last_status`]. `#[serde(default)]`
    /// so on-disk v2 records without the field deserialize unchanged.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Optional retry budget. `None` means unlimited (legacy behavior).
    /// When `consecutive_failures >= max_retries`, the engine disables
    /// the job via [`crate::CronScheduler::set_job_enabled`]. Default
    /// applied by the engine when this is `None`.
    #[serde(default)]
    pub max_retries: Option<u32>,
}

impl CronJob {
    /// A short description for the steer message body. Falls back to
    /// the job's `name` and finally a generic label.
    #[must_use]
    pub fn task_description(&self) -> String {
        match &self.action {
            CronJobAction::Send { message, .. } if !message.is_empty() => message.clone(),
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

/// Build a `SpawnTool`-action [`CronJob`] from caller parameters.
#[allow(clippy::too_many_arguments)]
pub fn build_spawn_tool_job(
    id: String,
    name: String,
    principal_id: PrincipalId,
    schedule: ScheduleKind,
    tool_name: String,
    tool_params: serde_json::Value,
    delete_after_run: bool,
    next_run: DateTime<Utc>,
    wake_on_completion: Option<bool>,
    timeout_secs: Option<u64>,
) -> CronJob {
    CronJob {
        id,
        name,
        principal_id,
        schedule,
        action: CronJobAction::SpawnTool {
            tool_name,
            tool_params,
            wake_on_completion,
            timeout_secs,
        },
        delete_after_run,
        enabled: true,
        created_at: Utc::now(),
        next_run,
        last_run: None,
        last_status: None,
        run_count: 0,
        consecutive_failures: 0,
        max_retries: None,
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

    Err(anyhow::anyhow!(
        "No schedule provided. Supply one of: cron, at, interval_ms, idle_ms."
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

/// Resolve whether the job should delete after run (one-shot).
#[must_use]
pub fn resolve_delete_after_run(params: &serde_json::Value) -> bool {
    params
        .get("one_shot")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Parse a human duration into milliseconds. Accepts a bare number
/// (milliseconds, matching `interval_ms`) or a number with a single
/// `s`/`m`/`h`/`d` suffix ("30s", "5m", "1h", "1d"). Hand-rolled to
/// avoid a new dependency — the workspace has no humantime-style crate.
/// Shared by the CLI (`--interval`, `--at "in 10m"`) and the
/// `CronCreate` tool's `delay` arg.
pub fn parse_duration_ms(input: &str) -> Result<u64> {
    let input = input.trim();
    let (digits, mult) = match input.chars().last() {
        Some('s') => (&input[..input.len() - 1], 1_000u64),
        Some('m') => (&input[..input.len() - 1], 60_000),
        Some('h') => (&input[..input.len() - 1], 3_600_000),
        Some('d') => (&input[..input.len() - 1], 86_400_000),
        _ => (input, 1),
    };
    let value: u64 = digits.trim().parse().map_err(|_| {
        anyhow::anyhow!("Invalid duration '{input}' (use e.g. 60000, 30s, 5m, 1h, 1d)")
    })?;
    Ok(value * mult)
}

/// Calculate the next run time for a schedule kind (pure function, no
/// storage access).
///
/// - `At { at }` parses the RFC3339 timestamp. If the parsed time is
///   in the past relative to `after` (i.e. the job has already fired),
///   returns the far-future sentinel so the job does not re-fire.
///   Otherwise returns the parsed time unchanged.
/// - `Every { every_ms }` adds the interval to `after`.
/// - `Cron { expr, tz }` uses the `cron` crate's next-occurrence logic,
///   with optional timezone resolution via `chrono-tz`.
/// - `Idle` returns a sentinel far-future timestamp (100 years) so it
///   doesn't get picked up by `due_jobs`.
pub fn calculate_next_run(schedule: &ScheduleKind, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    use std::str::FromStr;

    match schedule {
        ScheduleKind::At { at } => {
            let dt = DateTime::parse_from_rfc3339(at)
                .map_err(|e| anyhow::anyhow!("Invalid timestamp: {e}"))?;
            let dt_utc = dt.with_timezone(&Utc);
            // One-shot: if the at time has already passed, return the
            // far-future sentinel so the job does not re-fire on every
            // poll tick. Matches the Idle sentinel pattern.
            if dt_utc <= after {
                Ok(after + chrono::Duration::days(365 * 100))
            } else {
                Ok(dt_utc)
            }
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
    }
}

/// Compute the next fire time for an `Every` interval job, anchored to
/// its *scheduled* time rather than the actual finish time.
///
/// `calculate_next_run(Every)` returns `after + every_ms`; when the
/// caller passes the actual finish time (which the cron engine did), the
/// tick quantisation slip (up to one poll interval, 15s by default) plus
/// the execution time accumulate into permanent drift — a 60s job fired
/// every ~75s (2026-08-07 field test, Finding 6). Anchoring to the
/// scheduled `next_run` keeps the long-run period at `every_ms`; the
/// catch-up loop skips past-due slots after downtime without bursting.
pub fn calculate_next_interval_anchored(
    scheduled: DateTime<Utc>,
    every_ms: u64,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    if every_ms == 0 {
        return now;
    }
    let step = chrono::Duration::milliseconds(every_ms as i64);
    let mut next = scheduled + step;
    while next <= now {
        next += step;
    }
    next
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
            };
            let status = if j.enabled { "active" } else { "disabled" };
            let mut obj = serde_json::json!({
                "job_id": j.id,
                "label": j.name,
                "principal": j.principal_id.0,
                "sub_command": sub_command,
                "action": j.action.kind_label(),
                "status": status,
                "next_run_at": j.next_run.to_rfc3339(),
                "run_count": j.run_count,
            });
            let map = obj.as_object_mut().expect("object literal above");
            match &j.action {
                CronJobAction::Send { message, target } => {
                    map.insert(
                        "task".to_string(),
                        serde_json::Value::String(message.clone()),
                    );
                    if let Some(t) = target {
                        map.insert("target".to_string(), serde_json::Value::String(t.clone()));
                    }
                }
                CronJobAction::SpawnTool {
                    tool_name,
                    tool_params,
                    wake_on_completion,
                    timeout_secs,
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
    fn anchored_interval_keeps_schedule_period() {
        // Scheduled 10:00:00, every 60s, run finished 15s late (tick
        // quantisation) — the next slot must be 10:01:00, not 10:01:15.
        let scheduled = DateTime::parse_from_rfc3339("2026-08-07T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = DateTime::parse_from_rfc3339("2026-08-07T10:00:15Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = calculate_next_interval_anchored(scheduled, 60_000, now);
        assert_eq!(
            next,
            DateTime::parse_from_rfc3339("2026-08-07T10:01:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn anchored_interval_skips_missed_slots_without_bursting() {
        // Daemon was down (or the run took) 3.5 periods — the next slot
        // is the first future multiple of the anchor, not a burst of
        // catch-up fires and not now+interval.
        let scheduled = DateTime::parse_from_rfc3339("2026-08-07T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = DateTime::parse_from_rfc3339("2026-08-07T10:03:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = calculate_next_interval_anchored(scheduled, 60_000, now);
        assert_eq!(
            next,
            DateTime::parse_from_rfc3339("2026-08-07T10:04:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn anchored_interval_zero_ms_does_not_hang() {
        let now = Utc::now();
        assert_eq!(calculate_next_interval_anchored(now, 0, now), now);
    }

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
            principal_id: PrincipalId("alice".into()),
            schedule: ScheduleKind::Every { every_ms: 60_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "Read".into(),
                tool_params: serde_json::json!({"path": "/tmp/x"}),
                wake_on_completion: Some(true),
                timeout_secs: Some(3600),
            },
            delete_after_run: false,
            enabled: true,
            created_at: chrono::Utc::now(),
            next_run: chrono::Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };
        let json = serde_json::to_string(&job).unwrap();
        let back: CronJob = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{:?}", job), format!("{:?}", back));
    }

    /// Phase 3 (2026-08-15): legacy Send jobs (written before the
    /// `target` field existed) must deserialize with `target: None` —
    /// the wire change is backward-compatible by serde default.
    #[test]
    fn send_target_defaults_to_none_on_legacy_json() {
        let legacy = serde_json::json!({"kind": "send", "message": "hello"});
        let action: CronJobAction = serde_json::from_value(legacy).unwrap();
        let CronJobAction::Send { message, target } = action else {
            panic!("expected Send action");
        };
        assert_eq!(message, "hello");
        assert_eq!(target, None);

        // `None` is skipped on serialize, so a legacy job re-written by
        // a new binary stays byte-compatible with the old shape.
        let json = serde_json::to_value(&CronJobAction::Send {
            message: "hello".into(),
            target: None,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "send", "message": "hello"})
        );
    }

    /// `"trunk"` is the only accepted target; anything else is a
    /// structured error at BOTH the serde boundary and the explicit
    /// validator (struct-literal construction bypasses serde).
    #[test]
    fn send_target_validation() {
        let ok: CronJobAction = serde_json::from_value(
            serde_json::json!({"kind": "send", "message": "m", "target": "trunk"}),
        )
        .unwrap();
        let CronJobAction::Send { target, .. } = &ok else {
            panic!("expected Send action");
        };
        assert_eq!(target.as_deref(), Some(SEND_TARGET_TRUNK));

        let err = serde_json::from_value::<CronJobAction>(
            serde_json::json!({"kind": "send", "message": "m", "target": "bogey"}),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid cron Send target 'bogey'"),
            "got: {err}"
        );

        assert!(validate_send_target(&None).is_ok());
        assert!(validate_send_target(&Some("trunk".to_string())).is_ok());
        let err = validate_send_target(&Some("bogey".to_string())).unwrap_err();
        assert!(
            err.to_string().contains("invalid cron Send target 'bogey'"),
            "got: {err}"
        );
    }

    /// Phase 3b (2026-08-15): trunk-targeted Send jobs with an `Every`
    /// schedule below [`TRUNK_MIN_INTERVAL_MS`] are refused (token-burn
    /// guard); everything else passes. Phase 7: the trunk is the
    /// DEFAULT target, so `None` is held to the same floor.
    #[test]
    fn trunk_send_interval_floor() {
        let trunk = Some(SEND_TARGET_TRUNK.to_string());

        // Below the floor → structured error naming the floor.
        let err = validate_trunk_send_interval(&ScheduleKind::Every { every_ms: 30_000 }, &trunk)
            .unwrap_err();
        assert!(err.to_string().contains("every_ms >= 60000"), "got: {err}");
        // Phase 7: the DEFAULT target is the trunk — same floor.
        let err = validate_trunk_send_interval(&ScheduleKind::Every { every_ms: 30_000 }, &None)
            .unwrap_err();
        assert!(err.to_string().contains("every_ms >= 60000"), "got: {err}");

        // At and above the floor → accepted.
        validate_trunk_send_interval(&ScheduleKind::Every { every_ms: 60_000 }, &trunk).unwrap();
        validate_trunk_send_interval(&ScheduleKind::Every { every_ms: 300_000 }, &trunk).unwrap();

        // Unknown targets pass through here (rejected by
        // `validate_send_target` instead) — the floor only concerns
        // trunk-bound jobs.
        validate_trunk_send_interval(
            &ScheduleKind::Every { every_ms: 30_000 },
            &Some("bogey".to_string()),
        )
        .unwrap();

        // At / Cron / Idle are exempt even for trunk targets.
        validate_trunk_send_interval(
            &ScheduleKind::At {
                at: "2099-01-01T00:00:00Z".into(),
            },
            &trunk,
        )
        .unwrap();
        validate_trunk_send_interval(
            &ScheduleKind::Cron {
                expr: "* * * * *".into(),
                tz: None,
            },
            &trunk,
        )
        .unwrap();
        validate_trunk_send_interval(&ScheduleKind::Idle { minutes: 1 }, &trunk).unwrap();
    }

    /// PR-4b — `peko channel poll` cron recipe. A `SpawnTool` job
    /// targeting `ChannelRead` must round-trip through the on-disk
    /// cron schedule (which is what the CLI and daemon both parse),
    /// so the recipe documented in `docs/user-guide/CLI_REFERENCE.md`
    /// is wire-compatible with the cron engine.
    #[test]
    fn cron_channel_poll_recipe_roundtrips_as_spawn_tool() {
        // The exact CLI invocation from the recipe doc:
        //   peko cron add --principal bob --tool ChannelRead \
        //     --params '{"channel":"chan_a1b2c3d4","limit":50}' \
        //     --every 30000
        let job = CronJob {
            id: "test-channel-poll".into(),
            name: "channel-poll-bob".into(),
            principal_id: PrincipalId("bob".into()),
            schedule: ScheduleKind::Every { every_ms: 30_000 },
            action: CronJobAction::SpawnTool {
                tool_name: "ChannelRead".into(),
                tool_params: serde_json::json!({
                    "channel": "chan_a1b2c3d4",
                    "limit": 50,
                }),
                wake_on_completion: Some(true),
                timeout_secs: None,
            },
            delete_after_run: false,
            enabled: true,
            created_at: chrono::Utc::now(),
            next_run: chrono::Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
        };

        let json = serde_json::to_string(&job).unwrap();
        let back: CronJob = serde_json::from_str(&json).unwrap();

        // The dispatch surface must name `ChannelRead` exactly so the
        // engine can resolve it through `ExtensionCore::list_tools`.
        let CronJobAction::SpawnTool {
            tool_name,
            tool_params,
            ..
        } = &back.action
        else {
            panic!("expected SpawnTool action, got {:?}", back.action.kind_label())
        };
        assert_eq!(tool_name, "ChannelRead", "tool name must be ChannelRead");
        assert_eq!(tool_params["channel"], "chan_a1b2c3d4");
        assert_eq!(tool_params["limit"], 50);

        // And the recipe should also be reachable through the
        // canonical `render_job_list` shape the CLI displays.
        let rendered = render_job_list(vec![back]);
        let entry = &rendered["jobs"][0];
        assert_eq!(entry["tool"], "ChannelRead");
        assert_eq!(entry["action"], "spawn_tool");
        assert_eq!(entry["principal"], "bob");
        assert_eq!(entry["params"]["channel"], "chan_a1b2c3d4");
    }
}
