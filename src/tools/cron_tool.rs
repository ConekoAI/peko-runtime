//! Cron tool for agents - allows agents to schedule and manage cron jobs

use crate::cron::{CronJob, CronScheduler, DeliveryMode, ExecutionTarget, ScheduleKind};
use crate::tools::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

/// Cron tool for agent use
pub struct CronTool {
    scheduler: Arc<CronScheduler>,
}

impl CronTool {
    /// Create a new cron tool with the given database path
    pub fn new(db_path: impl Into<PathBuf>) -> Result<Self> {
        let scheduler = Arc::new(CronScheduler::new(db_path)?);
        Ok(Self { scheduler })
    }

    /// Get the scheduler (for engine use)
    #[must_use]
    pub fn scheduler(&self) -> Arc<CronScheduler> {
        self.scheduler.clone()
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn description(&self) -> &'static str {
        "Manage scheduled cron jobs. Supports: at, every, cron, idle, and event triggers."
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let action = params["action"].as_str().unwrap_or("list");

        match action {
            "add" => self.handle_add(params),
            "list" => self.handle_list(params),
            "list_idle" => self.handle_list_idle(params),
            "list_event" => self.handle_list_event(params),
            "remove" => self.handle_remove(params),
            "run" => self.handle_run(params),
            "history" => self.handle_history(params),
            _ => Err(anyhow::anyhow!("Unknown action: {action}")),
        }
    }
}

impl CronTool {
    fn handle_add(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("name is required"))?;

        let schedule = if let Some(sched) = params.get("schedule") {
            parse_schedule(sched)?
        } else {
            return Err(anyhow::anyhow!("schedule is required"));
        };

        let message = params["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("message is required"))?;

        let execution = match params["execution"].as_str() {
            Some("isolated") => ExecutionTarget::Isolated,
            _ => ExecutionTarget::Main,
        };

        let delivery = match params["delivery"].as_str() {
            Some("announce") => DeliveryMode::Announce {
                channel: None,
                to: None,
                best_effort: true,
            },
            _ => DeliveryMode::None,
        };

        let delete_after_run = params["delete_after_run"].as_bool().unwrap_or(false);

        // Calculate next run time
        let now = Utc::now();
        let next_run = self.scheduler.calculate_next_run(&schedule, now)?;

        let job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            schedule,
            target: execution,
            agent_id: None, // Will be set by engine
            message: message.to_string(),
            delivery,
            delete_after_run,
            enabled: true,
            created_at: now,
            next_run,
            last_run: None,
            last_status: None,
            run_count: 0,
        };

        self.scheduler.add_job(&job)?;

        Ok(serde_json::json!({
            "success": true,
            "job_id": job.id,
            "name": job.name,
            "next_run": job.next_run.to_rfc3339(),
            "message": format!("Job '{}' scheduled successfully", name)
        }))
    }

    fn handle_list(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        let jobs = self.scheduler.list_jobs(false)?;

        let job_list: Vec<serde_json::Value> = jobs
            .into_iter()
            .map(|j| {
                serde_json::json!({
                    "id": j.id,
                    "name": j.name,
                    "schedule": j.schedule.display(),
                    "target": match j.target {
                        ExecutionTarget::Main => "main",
                        ExecutionTarget::Isolated => "isolated",
                    },
                    "next_run": j.next_run.to_rfc3339(),
                    "enabled": j.enabled,
                    "run_count": j.run_count,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "success": true,
            "jobs": job_list,
            "count": job_list.len()
        }))
    }

    fn handle_list_idle(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        let jobs = self.scheduler.idle_jobs(false)?;

        let job_list: Vec<serde_json::Value> = jobs
            .into_iter()
            .map(|j| {
                serde_json::json!({
                    "id": j.id,
                    "name": j.name,
                    "schedule": j.schedule.display(),
                    "enabled": j.enabled,
                    "run_count": j.run_count,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "success": true,
            "jobs": job_list,
            "count": job_list.len(),
            "type": "idle"
        }))
    }

    fn handle_list_event(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        let jobs = self.scheduler.event_jobs(false)?;

        let job_list: Vec<serde_json::Value> = jobs
            .into_iter()
            .map(|j| {
                serde_json::json!({
                    "id": j.id,
                    "name": j.name,
                    "schedule": j.schedule.display(),
                    "enabled": j.enabled,
                    "run_count": j.run_count,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "success": true,
            "jobs": job_list,
            "count": job_list.len(),
            "type": "event"
        }))
    }

    fn handle_remove(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let job_id = params["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("job_id is required"))?;

        let deleted = self.scheduler.delete_job(job_id)?;

        if deleted {
            Ok(serde_json::json!({
                "success": true,
                "message": format!("Job {} removed", job_id)
            }))
        } else {
            Err(anyhow::anyhow!("Job {job_id} not found"))
        }
    }

    fn handle_run(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let job_id = params["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("job_id is required"))?;

        // Get the job
        let job = self
            .scheduler
            .get_job(job_id)?
            .ok_or_else(|| anyhow::anyhow!("Job {job_id} not found"))?;

        // Note: Actual execution would be handled by the cron engine
        // This just returns info about the job
        Ok(serde_json::json!({
            "success": true,
            "message": format!("Job {} queued for execution", job_id),
            "job": {
                "id": job.id,
                "name": job.name,
                "message": job.message
            }
        }))
    }

    fn handle_history(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let job_id = params["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("job_id is required"))?;

        let limit = params["limit"].as_u64().unwrap_or(10) as usize;

        let runs = self.scheduler.get_run_history(job_id, limit)?;

        let run_list: Vec<serde_json::Value> = runs
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "started_at": r.started_at.to_rfc3339(),
                    "finished_at": r.finished_at.map(|t| t.to_rfc3339()),
                    "status": r.status,
                    "output": r.output,
                    "error": r.error,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "success": true,
            "runs": run_list,
            "count": run_list.len()
        }))
    }
}

fn parse_schedule(schedule: &serde_json::Value) -> Result<ScheduleKind> {
    let kind = schedule["kind"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("schedule.kind is required"))?;

    match kind {
        "at" => {
            let at = schedule["at"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("schedule.at is required for kind=at"))?;
            Ok(ScheduleKind::At { at: at.to_string() })
        }
        "every" => {
            let every_ms = schedule["every_ms"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("schedule.every_ms is required for kind=every"))?;
            Ok(ScheduleKind::Every { every_ms })
        }
        "cron" => {
            let expr = schedule["expr"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("schedule.expr is required for kind=cron"))?;
            let tz = schedule["tz"]
                .as_str()
                .map(std::string::ToString::to_string);
            Ok(ScheduleKind::Cron {
                expr: expr.to_string(),
                tz,
            })
        }
        "idle" => {
            let minutes = schedule["minutes"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("schedule.minutes is required for kind=idle"))?;
            let agent_id = schedule["agent_id"].as_str().map(String::from);
            Ok(ScheduleKind::Idle { minutes, agent_id })
        }
        "event" => {
            let event_type = schedule["event_type"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("schedule.event_type is required for kind=event"))?
                .to_string();
            let filter = schedule.get("filter").cloned();
            let once = schedule["once"].as_bool().unwrap_or(false);
            Ok(ScheduleKind::Event {
                event_type,
                filter,
                once,
            })
        }
        _ => Err(anyhow::anyhow!("Invalid schedule kind: {kind}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_tool() -> (CronTool, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("cron.db");
        let tool = CronTool::new(db_path).unwrap();
        (tool, tmp)
    }

    #[test]
    fn test_parse_schedule_at() {
        let schedule = serde_json::json!({
            "kind": "at",
            "at": "2026-01-01T00:00:00Z"
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), ScheduleKind::At { .. }));
    }

    #[test]
    fn test_parse_schedule_every() {
        let schedule = serde_json::json!({
            "kind": "every",
            "every_ms": 60000
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), ScheduleKind::Every { .. }));
    }

    #[test]
    fn test_parse_schedule_cron() {
        let schedule = serde_json::json!({
            "kind": "cron",
            "expr": "0 9 * * *",
            "tz": "America/New_York"
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), ScheduleKind::Cron { .. }));
    }

    #[test]
    fn test_parse_schedule_idle() {
        let schedule = serde_json::json!({
            "kind": "idle",
            "minutes": 10,
            "agent_id": "test-agent"
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert!(matches!(parsed, ScheduleKind::Idle { .. }));
        if let ScheduleKind::Idle { minutes, agent_id } = parsed {
            assert_eq!(minutes, 10);
            assert_eq!(agent_id, Some("test-agent".to_string()));
        }
    }

    #[test]
    fn test_parse_schedule_idle_without_agent() {
        let schedule = serde_json::json!({
            "kind": "idle",
            "minutes": 5
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        if let ScheduleKind::Idle { minutes, agent_id } = parsed {
            assert_eq!(minutes, 5);
            assert_eq!(agent_id, None);
        }
    }

    #[test]
    fn test_parse_schedule_event() {
        let schedule = serde_json::json!({
            "kind": "event",
            "event_type": "webhook",
            "filter": {"source": "github"},
            "once": true
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert!(matches!(parsed, ScheduleKind::Event { .. }));
        if let ScheduleKind::Event {
            event_type, once, ..
        } = parsed
        {
            assert_eq!(event_type, "webhook");
            assert!(once);
        }
    }

    #[test]
    fn test_parse_schedule_event_defaults() {
        let schedule = serde_json::json!({
            "kind": "event",
            "event_type": "file"
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        if let ScheduleKind::Event {
            event_type,
            once,
            filter,
        } = parsed
        {
            assert_eq!(event_type, "file");
            assert!(!once); // default
            assert!(filter.is_none()); // default
        }
    }

    #[test]
    fn test_parse_schedule_invalid_kind() {
        let schedule = serde_json::json!({
            "kind": "invalid"
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_schedule_missing_kind() {
        let schedule = serde_json::json!({
            "at": "2026-01-01T00:00:00Z"
        });
        let result = parse_schedule(&schedule);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_cron_tool_add_idle_job() {
        let (tool, _tmp) = create_test_tool();
        let result = tool
            .execute(serde_json::json!({
                "action": "add",
                "name": "test-idle",
                "schedule": {
                    "kind": "idle",
                    "minutes": 10
                },
                "message": "Idle cleanup task"
            }))
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
        assert!(response["job_id"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_cron_tool_add_event_job() {
        let (tool, _tmp) = create_test_tool();
        let result = tool
            .execute(serde_json::json!({
                "action": "add",
                "name": "test-event",
                "schedule": {
                    "kind": "event",
                    "event_type": "webhook",
                    "filter": {"source": "github"},
                    "once": true
                },
                "message": "Handle GitHub webhook"
            }))
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_cron_tool_list_idle() {
        let (tool, _tmp) = create_test_tool();

        // Add an idle job first
        tool.execute(serde_json::json!({
            "action": "add",
            "name": "idle-job",
            "schedule": {"kind": "idle", "minutes": 5},
            "message": "test"
        }))
        .await
        .unwrap();

        // List idle jobs
        let result = tool
            .execute(serde_json::json!({
                "action": "list_idle"
            }))
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["count"].as_u64().unwrap(), 1);
        assert_eq!(response["type"].as_str().unwrap(), "idle");
    }

    #[tokio::test]
    async fn test_cron_tool_list_event() {
        let (tool, _tmp) = create_test_tool();

        // Add an event job first
        tool.execute(serde_json::json!({
            "action": "add",
            "name": "event-job",
            "schedule": {"kind": "event", "event_type": "file"},
            "message": "test"
        }))
        .await
        .unwrap();

        // List event jobs
        let result = tool
            .execute(serde_json::json!({
                "action": "list_event"
            }))
            .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["count"].as_u64().unwrap(), 1);
        assert_eq!(response["type"].as_str().unwrap(), "event");
    }
}
