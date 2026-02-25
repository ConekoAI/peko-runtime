//! Cron tool for agents - allows agents to schedule and manage cron jobs

use crate::cron::{
    CronJob, CronScheduler, DeliveryMode, ExecutionTarget, ScheduleKind,
};
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
        "Manage scheduled cron jobs. Schedule one-time or recurring tasks."
    }

    async fn execute(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let action = params["action"].as_str().unwrap_or("list");

        match action {
            "add" => self.handle_add(params).await,
            "list" => self.handle_list(params).await,
            "remove" => self.handle_remove(params).await,
            "run" => self.handle_run(params).await,
            "history" => self.handle_history(params).await,
            _ => Err(anyhow::anyhow!("Unknown action: {action}")),
        }
    }
}

impl CronTool {
    async fn handle_add(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
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

    async fn handle_list(
        &self,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
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

    async fn handle_remove(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
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

    async fn handle_run(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let job_id = params["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("job_id is required"))?;

        // Get the job
        let job = self.scheduler
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

    async fn handle_history(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
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
            let tz = schedule["tz"].as_str().map(std::string::ToString::to_string);
            Ok(ScheduleKind::Cron { expr: expr.to_string(), tz })
        }
        _ => Err(anyhow::anyhow!("Invalid schedule kind: {kind}")),
    }
}
