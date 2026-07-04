//! Cron tool internals — shared helpers for the CronCreate / CronDelete / CronList tools
//!
//! The daemon is the single source of authority for cron jobs.
//! All operations (add, list, cancel) are sent to the daemon over IPC,
//! and the daemon persists jobs to cron.json and executes them.

use crate::cron::{CronJob, DeliveryMode, ScheduleKind};
use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::str::FromStr;
use uuid::Uuid;

/// Connect to the daemon via IPC
pub async fn connect_daemon() -> anyhow::Result<DaemonClient> {
    DaemonClient::connect().await.map_err(|e| {
        anyhow::anyhow!("Cannot reach daemon for cron operations. Is it running? ({e})")
    })
}

/// Build a CronJob from common args
pub fn build_job(
    label: String,
    task: String,
    schedule: ScheduleKind,
    delete_after_run: bool,
    principal_name: String,
) -> anyhow::Result<CronJob> {
    let next_run = crate::cron::calculate_next_run(&schedule, Utc::now())?;
    Ok(CronJob {
        id: format!("cron_{}", Uuid::new_v4().simple()),
        name: label,
        principal_name,
        schedule,
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

/// Register a job via daemon IPC
pub async fn register_job_via_daemon(job: CronJob) -> Result<serde_json::Value> {
    let next_run = job.next_run;
    let job_id = job.id.clone();
    let label = job.name.clone();

    let client = connect_daemon().await?;
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
        other => Err(crate::ipc::unexpected_response(&other)),
    }
}

/// Resolve a schedule kind from CronCreate parameters.
pub fn resolve_schedule_kind(params: &serde_json::Value) -> anyhow::Result<ScheduleKind> {
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
        let normalized = crate::cron::normalize_cron_expr(expr);
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
pub fn resolve_label(params: &serde_json::Value) -> String {
    params
        .get("label")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("cron-{}", Uuid::new_v4().simple()))
}

/// Resolve the task/prompt from parameters.
pub fn resolve_prompt(params: &serde_json::Value) -> anyhow::Result<String> {
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
pub fn resolve_delete_after_run(params: &serde_json::Value) -> bool {
    // Explicit one-shot flag
    if let Some(one_shot) = params.get("one_shot").and_then(|v| v.as_bool()) {
        return one_shot;
    }
    // Claude parity: recurring=false implies one-shot
    if let Some(recurring) = params.get("recurring").and_then(|v| v.as_bool()) {
        return !recurring;
    }
    false
}

/// Render a list of CronJob values into the CronList return shape.
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
            json!({
                "job_id": j.id,
                "label": j.name,
                "principal": j.principal_name,
                "sub_command": sub_command,
                "task": j.message,
                "status": status,
                "next_run_at": j.next_run.to_rfc3339(),
                "run_count": j.run_count,
            })
        })
        .collect();

    json!({
        "jobs": jobs_json,
        "count": jobs_json.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_label_uses_provided() {
        let params = json!({"label": "my-job"});
        assert_eq!(resolve_label(&params), "my-job");
    }

    #[test]
    fn test_resolve_label_generates_default() {
        let params = json!({});
        assert!(resolve_label(&params).starts_with("cron-"));
    }

    #[test]
    fn test_resolve_prompt_requires_prompt() {
        let params = json!({});
        assert!(resolve_prompt(&params).is_err());
    }

    #[test]
    fn test_resolve_prompt_accepts_prompt() {
        let params = json!({"prompt": "do the thing"});
        assert_eq!(resolve_prompt(&params).unwrap(), "do the thing");
    }

    #[test]
    fn test_resolve_delete_after_run_defaults_false() {
        assert!(!resolve_delete_after_run(&json!({})));
    }

    #[test]
    fn test_resolve_delete_after_run_respects_recurring_false() {
        assert!(resolve_delete_after_run(&json!({"recurring": false})));
    }

    #[test]
    fn test_resolve_schedule_kind_cron() {
        let params = json!({"cron": "0 9 * * *"});
        let kind = resolve_schedule_kind(&params).unwrap();
        assert!(matches!(kind, ScheduleKind::Cron { .. }));
    }

    #[test]
    fn test_resolve_schedule_kind_invalid_cron() {
        let params = json!({"cron": "not a cron"});
        assert!(resolve_schedule_kind(&params).is_err());
    }
}
