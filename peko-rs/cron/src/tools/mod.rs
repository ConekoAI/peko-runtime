//! Cron scheduling tool, runtime port, and persistent job DTOs.
//!
//! Cron has six explicit actions. Private action handlers call CronRuntime;
//! the daemon implements that port without a dependency back to core.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

use crate::CronRun;

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
/// add`, the `Cron action create` tool, in-process construction) funnels
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
/// - [`Self::Send`] — at fire time the daemon delivers `message` to the
///   principal's trunk session as a user-message and runs a full agent
///   turn (LLM-driven, dynamic output; the agent answers via
///   ChannelSend). Written by `Cron action create` with `message` (and formerly
///   by the retired `peko cron add` CLI).
/// - [`Self::SpawnTool`] — at fire time the daemon asks the
///   `AsyncExecutor` to run `tool_name` with `tool_params` (fixed
///   dispatch; the invoked tool may itself call an LLM). Written by
///   `Cron action create` with `tool` + `params`.
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
    /// The session the job was created from (2026-09-08). At fire
    /// time the job runs with THIS session as its caller context —
    /// relative `Agent` paths resolve against it, SpawnTool runs
    /// attribute to it, and `Send` (message) jobs land in it, so a
    /// "remind me" created from `/user-bob` reaches bob's conversation
    /// instead of the trunk. `None` (legacy jobs, trunk-created jobs)
    /// falls back to the trunk session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_session: Option<String>,
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

    /// Patch mutable job fields (`enabled`, `wake_on_completion`).
    /// `None` fields are left untouched. Returns an error when the job
    /// does not exist.
    async fn update_job(
        &self,
        job_id: &str,
        enabled: Option<bool>,
        wake_on_completion: Option<bool>,
    ) -> Result<()>;

    /// Fire a job immediately, out of schedule. Returns the run id;
    /// coalesces with an in-flight run of the same job (returns the
    /// existing run id instead of double-firing). Manual triggers
    /// ignore the job's `enabled` flag (debugging path). The run
    /// executes in the background; its outcome lands in run history.
    async fn trigger_job(&self, job_id: &str) -> Result<String>;

    /// Read a job's run history, most recent first, capped at `limit`.
    /// Errors when the job (or its history) does not exist.
    async fn run_history(&self, job_id: &str, limit: usize) -> Result<Vec<CronRun>>;

    /// Whether `principal_id` owns `job_id`'s run history: the job is live
    /// in that principal's schedule, or it was deleted (a fired one-shot
    /// job reaps itself) and its runs remain there. Live-job listings
    /// cannot answer this for reaped jobs.
    async fn owns_job_history(&self, principal_id: &PrincipalId, job_id: &str) -> Result<bool>;
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

/// Resolve a schedule kind from `Cron action create` tool parameters.
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

/// Parse a human duration into milliseconds. Accepts a bare number
/// (milliseconds, matching `interval_ms`) or a number with a single
/// `s`/`m`/`h`/`d` suffix ("30s", "5m", "1h", "1d"). Hand-rolled to
/// avoid a new dependency — the workspace has no humantime-style crate.
/// Shared by the CLI (`--interval`, `--at "in 10m"`) and the
/// `Cron action create` tool's `delay` arg.
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
/// scheduled `next_run` preserves the grid. Slots at or before completion
/// are skipped after overruns or downtime without bursting; this does not
/// guarantee one execution or observation per interval.
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

/// Render a list of [`CronJob`] values into the canonical `Cron action list`
/// return shape shared by the CLI and the `Cron action list` tool.
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

// ─── Private action handlers ────────────────────────────

mod create;
mod delete;
mod history;
mod list;
mod trigger;
mod update;

pub(crate) use create::CronCreateAction;
pub(crate) use delete::CronDeleteAction;
pub(crate) use history::CronHistoryAction;
pub(crate) use list::CronListAction;
pub(crate) use trigger::CronTriggerAction;
pub(crate) use update::CronUpdateAction;

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

/// Cron runtime slot, installed once by the daemon. Cron actions resolve it
/// at execution time, so the stable catalog can be installed before startup
/// completes. A call before installation reports that cron is unavailable.
static RUNTIME: OnceLock<Arc<dyn CronRuntime>> = OnceLock::new();

/// Install the cron runtime. Subsequent registrations leave it unchanged.
pub fn set_global_runtime(runtime: Arc<dyn CronRuntime>) {
    if RUNTIME.set(runtime).is_err() {
        // Idempotent: if the same runtime is set twice, that's a
        // misconfiguration but not catastrophic. Silently no-op
        // rather than panicking in test harnesses that re-init.
    }
}

/// Read the cron runtime, or `None` before daemon installation.
pub fn global_runtime() -> Option<Arc<dyn CronRuntime>> {
    RUNTIME.get().cloned()
}

/// The runtime a [`CronTool`] dispatches to: an explicitly bound runtime,
/// or the daemon-installed slot when unbound. Explicit binding lets tests
/// (and future composition roots) drive the tool without process-global
/// state, which can only be installed once per process.
#[derive(Clone, Default)]
pub(crate) struct RuntimeBinding(Option<Arc<dyn CronRuntime>>);

impl RuntimeBinding {
    pub(crate) fn resolve(&self) -> Option<Arc<dyn CronRuntime>> {
        self.0.clone().or_else(global_runtime)
    }
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
    fn anchored_interval_boundary_and_worker_overrun() {
        let scheduled = DateTime::parse_from_rfc3339("2026-10-07T05:25:56Z")
            .unwrap()
            .with_timezone(&Utc);
        // Finishing on the next slot counts as overdue, not a catch-up.
        // The 60.759s live worker likewise loses a whole scheduled check.
        for (finish_ms, next_ms) in [(59_999, 60_000), (60_000, 120_000), (60_759, 120_000)] {
            assert_eq!(
                calculate_next_interval_anchored(
                    scheduled,
                    60_000,
                    scheduled + chrono::Duration::milliseconds(finish_ms),
                ),
                scheduled + chrono::Duration::milliseconds(next_ms)
            );
        }
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
            origin_session: None,
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
            origin_session: None,
        };

        let json = serde_json::to_string(&job).unwrap();
        let back: CronJob = serde_json::from_str(&json).unwrap();

        // The dispatch surface must name `ChannelRead` exactly so the
        // engine can resolve it through `ToolingRuntime::list_tools`.
        let CronJobAction::SpawnTool {
            tool_name,
            tool_params,
            ..
        } = &back.action
        else {
            panic!(
                "expected SpawnTool action, got {:?}",
                back.action.kind_label()
            )
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

mod tool;
pub use tool::CronTool;

#[cfg(test)]
mod tool_tests;
