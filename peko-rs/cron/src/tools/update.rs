//! `Cron action update` tool — patch mutable fields of a scheduled job
//!
//! Updates a `CronJob` through the [`CronRuntime`] port set by the
//! daemon at startup, like the sibling cron tools. Covers the fields an
//! agent legitimately changes after creation: `enabled` (pause/resume
//! without delete), `wake_on_completion` (result subscription — whether
//! each fire's outcome steers back into the creating session's inbox
//! (trunk fallback)), a message job's `model`, `when` and `message`,
//! and the schedule (`interval_ms`, `cron`, `timezone`, `idle_ms`).
//! Retuning in place keeps the job's id and creator, so a runtime-seeded
//! job keeps the behaviour the runtime attaches to its id (ADR-080,
//! ADR-083); deleting and re-creating it does not.

use crate::tools::delete::{resolve_id_by_label, verify_id_belongs_to_principal};
use crate::tools::{resolve_schedule_kind, JobPatch, RuntimeBinding};
use crate::{CronJob, ScheduleKind};
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `Cron action update` tool — patch a scheduled job's mutable fields
pub struct CronUpdateAction {
    runtime: RuntimeBinding,
}

impl CronUpdateAction {
    /// Bind the action to the runtime its domain tool resolves.
    pub(crate) fn bound(runtime: RuntimeBinding) -> Self {
        Self { runtime }
    }
}

/// `Cron action update` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronUpdateArgs {
    /// Job ID to update
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional label to update (alternative to `id`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Enable/disable the job
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Subscribe/unsubscribe the creating session's inbox (trunk fallback) to the job's
    /// result each fire (SpawnTool jobs only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_on_completion: Option<bool>,
    /// Message jobs only: the configured model each fire runs on; an
    /// empty string returns the job to the principal's pinned model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Message jobs only: when a due fire runs its turn (ADR-083)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<crate::FireWhen>,
    /// Message jobs only: the instruction each fire delivers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// New recurring interval in milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    /// New 5-field cron expression
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// IANA timezone for the cron expression
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// New quiet-stretch trigger in milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
}

/// The schedule `args` asks for, given the job's current one: at most one
/// of `interval_ms`, `cron` and `idle_ms`; `timezone` alone moves a cron
/// job to another zone, and a new `cron` without `timezone` keeps the
/// job's zone. `None` leaves the schedule as it is.
fn resolve_new_schedule(
    args: &CronUpdateArgs,
    params: &serde_json::Value,
    current: &ScheduleKind,
) -> anyhow::Result<Option<ScheduleKind>> {
    let given = [
        args.interval_ms.is_some(),
        args.cron.is_some(),
        args.idle_ms.is_some(),
    ]
    .into_iter()
    .filter(|g| *g)
    .count();
    if given > 1 {
        anyhow::bail!("give one new schedule: `interval_ms`, `cron` or `idle_ms`");
    }
    let current_tz = match current {
        ScheduleKind::Cron { tz, .. } => tz.clone(),
        _ => None,
    };
    if let Some(tz) = &args.timezone {
        if args.cron.is_none() {
            let ScheduleKind::Cron { expr, .. } = current else {
                anyhow::bail!(
                    "`timezone` applies to cron expressions; this job runs {}",
                    current.display()
                );
            };
            if given > 0 {
                anyhow::bail!(
                    "`timezone` applies to cron expressions, not `interval_ms` or `idle_ms`"
                );
            }
            tz.parse::<chrono_tz::Tz>()
                .map_err(|e| anyhow::anyhow!("Invalid timezone '{tz}': {e}"))?;
            return Ok(Some(ScheduleKind::Cron {
                expr: expr.clone(),
                tz: Some(tz.clone()),
            }));
        }
    }
    if given == 0 {
        return Ok(None);
    }
    let mut schedule_params = params.clone();
    if let (Some(_), None, Some(tz)) = (&args.cron, &args.timezone, &current_tz) {
        schedule_params["timezone"] = serde_json::json!(tz);
    }
    resolve_schedule_kind(&schedule_params).map(Some)
}

#[async_trait]
impl Tool for CronUpdateAction {
    fn name(&self) -> &'static str {
        "Cron"
    }

    fn description(&self) -> String {
        "Update a scheduled job's mutable fields by ID (or label): `enabled` pauses/resumes without deleting (re-enabling resets the failure budget); `wake_on_completion` subscribes or unsubscribes you to the job's result on each fire (tool jobs only — message jobs already land in your session); `model` picks the configured model a message job's turn runs on (an empty string returns it to your pinned model); `when` (\"always\" / \"attention\" / \"quiet\") sets whether every due fire runs a message job's turn, only fires when something waits on you, or waits for a quiet stretch; `message` replaces a message job's instruction; `interval_ms`, `cron` (with optional `timezone`) or `idle_ms` replaces a recurring job's schedule, and `timezone` alone moves a cron job to another zone. Retune jobs here rather than deleting and re-creating them: an update keeps the job's id, and the runtime's own jobs (`keepalive`, `memory`) keep what the runtime does for them only under their ids.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "ID of the scheduled job to update"
                },
                "label": {
                    "type": "string",
                    "description": "Label of the scheduled job to update (alternative to id)"
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Enable or disable the job. Re-enabling resets its consecutive-failure budget."
                },
                "wake_on_completion": {
                    "type": "boolean",
                    "description": "Subscribe (true) or unsubscribe (false) the creating session's inbox (trunk fallback) to the job's result each fire. SpawnTool jobs only; message jobs already run in your session."
                },
                "model": {
                    "type": "string",
                    "description": "Message jobs only. Configured model id (see `ModelList`) each fire's turn runs on, e.g. a cheaper one for routine bookkeeping. An empty string returns the job to your pinned model."
                },
                "when": {
                    "type": "string",
                    "enum": ["always", "attention", "quiet"],
                    "description": "Recurring message jobs only. \"attention\": a due fire runs its turn only when something waits on you or the job's last turn is an hour old; otherwise it is a quiet tick with no model call. \"quiet\" (interval or cron schedules): a due fire waits until nobody has talked to you for 4 hours, at most 12 hours past due. \"always\": every fire runs."
                },
                "message": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Message jobs only. The instruction each fire delivers. The `memory` job's message is the runtime's marker for its current memory brief and cannot be replaced; create a separate job for a routine of your own."
                },
                "interval_ms": {
                    "type": "integer",
                    "description": "New recurring interval in milliseconds (message jobs: at least 60000). The next fire is one interval from now."
                },
                "cron": {
                    "type": "string",
                    "description": "New 5-field cron expression. Keeps the job's timezone unless `timezone` is given."
                },
                "timezone": {
                    "type": "string",
                    "description": "IANA timezone for the cron expression, e.g. 'Europe/Berlin'. Alone, moves a cron job to this zone."
                },
                "idle_ms": {
                    "type": "integer",
                    "description": "New trigger: fire once nobody has talked to you for this long, in milliseconds (whole minutes, at least one)."
                }
            },
            "allOf": [
                {"anyOf": [{"required": ["id"]}, {"required": ["label"]}]},
                {"anyOf": [
                    {"required": ["enabled"]}, {"required": ["wake_on_completion"]},
                    {"required": ["model"]}, {"required": ["when"]}, {"required": ["message"]},
                    {"required": ["interval_ms"]}, {"required": ["cron"]},
                    {"required": ["timezone"]}, {"required": ["idle_ms"]}
                ]}
            ]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "Cron action update requires a Principal context; use execute_with_context"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let principal_id = ctx
            .principal_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Cron action update requires a Principal context"))?
            .clone();

        let runtime = self.runtime.resolve().ok_or_else(|| {
            anyhow::anyhow!(
                "Cron action update requires the daemon's cron runtime; not initialized"
            )
        })?;

        let args: CronUpdateArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid Cron action update arguments: {e}"))?;

        let asks_change = args.enabled.is_some()
            || args.wake_on_completion.is_some()
            || args.model.is_some()
            || args.when.is_some()
            || args.message.is_some()
            || args.interval_ms.is_some()
            || args.cron.is_some()
            || args.timezone.is_some()
            || args.idle_ms.is_some();
        if !asks_change {
            return Err(anyhow::anyhow!(
                "Cron action update requires at least one field to change: `enabled`, \
                 `wake_on_completion`, `model`, `when`, `message`, `interval_ms`, `cron`, \
                 `timezone`, or `idle_ms`"
            ));
        }

        let job_id = if let Some(id) = args.id.clone().filter(|s| !s.is_empty()) {
            verify_id_belongs_to_principal(&*runtime, &id, &principal_id, &ctx.acting_for())
                .await?;
            id
        } else if let Some(label) = args.label.clone() {
            resolve_id_by_label(&*runtime, &label, &principal_id, &ctx.acting_for()).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for Cron action update"
            ));
        };
        let jobs = runtime.list_jobs().await?;
        let job: &CronJob = jobs
            .iter()
            .find(|j| j.id == job_id && j.principal_id.0 == principal_id)
            .ok_or_else(|| anyhow::anyhow!("Job '{job_id}' not found"))?;

        let patch = JobPatch {
            enabled: args.enabled,
            wake_on_completion: args.wake_on_completion,
            model: args
                .model
                .as_deref()
                .map(|m| Some(m.trim().to_string()).filter(|m| !m.is_empty())),
            when: args.when,
            schedule: resolve_new_schedule(&args, &params, &job.schedule)?,
            message: args.message.clone(),
        };

        // ADR-078 D5: re-enabling a non-owner job counts against its
        // compartment's limit like creating one, and a new schedule
        // keeps to the non-owner cadence floor.
        if args.enabled == Some(true) || patch.schedule.is_some() {
            let mut job = job.clone();
            if args.enabled == Some(true) {
                job.enabled = true;
            }
            if let Some(schedule) = &patch.schedule {
                job.schedule = schedule.clone();
            }
            crate::tools::check_non_owner_job(&job, &jobs)?;
        }
        let owner = peko_subject::PrincipalId(principal_id);
        runtime.update_job(&owner, &job_id, &patch).await?;
        let mut out = json!({
            "updated": true,
            "job_id": job_id,
            "enabled": patch.enabled,
            "wake_on_completion": patch.wake_on_completion,
        });
        if let Some(model) = patch.model {
            out["model"] = json!(model);
        }
        if let Some(when) = patch.when {
            out["when"] = json!(when.as_str());
        }
        if let Some(schedule) = &patch.schedule {
            out["schedule"] = json!(schedule.display());
        }
        if patch.message.is_some() {
            out["message_replaced"] = json!(true);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(
        params: serde_json::Value,
        current: &ScheduleKind,
    ) -> anyhow::Result<Option<ScheduleKind>> {
        let args: CronUpdateArgs = serde_json::from_value(params.clone()).unwrap();
        resolve_new_schedule(&args, &params, current)
    }

    #[test]
    fn new_schedule_resolution() {
        let every = ScheduleKind::Every { every_ms: 600_000 };
        let berlin = ScheduleKind::Cron {
            expr: "0 9 * * *".into(),
            tz: Some("Europe/Berlin".into()),
        };

        assert_eq!(
            resolve(serde_json::json!({"enabled": true}), &every).unwrap(),
            None
        );
        assert_eq!(
            resolve(serde_json::json!({"interval_ms": 1_800_000}), &berlin).unwrap(),
            Some(ScheduleKind::Every {
                every_ms: 1_800_000
            })
        );
        assert_eq!(
            resolve(serde_json::json!({"cron": "0 3 * * *"}), &berlin).unwrap(),
            Some(ScheduleKind::Cron {
                expr: "0 3 * * *".into(),
                tz: Some("Europe/Berlin".into()),
            })
        );
        assert_eq!(
            resolve(serde_json::json!({"timezone": "Asia/Tokyo"}), &berlin).unwrap(),
            Some(ScheduleKind::Cron {
                expr: "0 9 * * *".into(),
                tz: Some("Asia/Tokyo".into()),
            })
        );
        assert_eq!(
            resolve(serde_json::json!({"idle_ms": 1_800_000}), &every).unwrap(),
            Some(ScheduleKind::Idle { minutes: 30 })
        );

        for (params, refusal) in [
            (
                serde_json::json!({"interval_ms": 60_000, "cron": "0 3 * * *"}),
                "one new schedule",
            ),
            (
                serde_json::json!({"timezone": "Asia/Tokyo"}),
                "applies to cron",
            ),
            (
                serde_json::json!({"timezone": "Mars/Base"}),
                "applies to cron",
            ),
            (serde_json::json!({"cron": "not a cron"}), "Invalid cron"),
        ] {
            let error = resolve(params.clone(), &every).unwrap_err();
            assert!(error.to_string().contains(refusal), "{params}: {error}");
        }
        let error = resolve(serde_json::json!({"timezone": "Mars/Base"}), &berlin).unwrap_err();
        assert!(error.to_string().contains("Invalid timezone"), "{error}");
    }
}
