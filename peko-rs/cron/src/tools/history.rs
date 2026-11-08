//! `Cron action history` tool — read a scheduled job's run history
//!
//! Reads `CronRun` rows (status, timestamps, output, error) through the
//! [`CronRuntime`] port. The data already lives in the principal's
//! schedule file (1000-run retention cap); this tool is the agent-facing
//! reader so "why did my reminder not fire / fail" is answerable without
//! shelling out to the schedule file.

use crate::tools::delete::{resolve_id_by_label, verify_id_belongs_to_principal};
use crate::tools::RuntimeBinding;
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `Cron action history` tool — read a scheduled job's run history
pub struct CronHistoryAction {
    runtime: RuntimeBinding,
}

impl CronHistoryAction {
    /// Bind the action to the runtime its domain tool resolves.
    pub(crate) fn bound(runtime: RuntimeBinding) -> Self {
        Self { runtime }
    }
}

/// `Cron action history` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronHistoryArgs {
    /// Job ID to read history for
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional label (alternative to `id`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Max runs to return (most recent first). Defaults to 10, capped at 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[async_trait]
impl Tool for CronHistoryAction {
    fn name(&self) -> &'static str {
        "Cron"
    }

    fn description(&self) -> String {
        r"A job's runs, newest first: status, start and finish times, output, and error. Use it to see why a job failed (`list` shows only the last fire's status). One-shot jobs delete themselves after firing, so read theirs by the job_id create returned; labels resolve only live jobs.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Job id"
                },
                "label": {
                    "type": "string",
                    "description": "Job label (instead of id)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Runs to return (default 10, at most 50)"
                }
            },
            "oneOf": [
                { "required": ["id"] },
                { "required": ["label"] }
            ]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "Cron action history requires a Principal context; use execute_with_context"
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
            .ok_or_else(|| anyhow::anyhow!("Cron action history requires a Principal context"))?
            .clone();

        let runtime = self.runtime.resolve().ok_or_else(|| {
            anyhow::anyhow!(
                "Cron action history requires the daemon's cron runtime; not initialized"
            )
        })?;

        let args: CronHistoryArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid Cron action history arguments: {e}"))?;

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            // Fired one-shot jobs delete themselves but keep their runs, so
            // ownership is checked against history, not the live job list.
            let owner = peko_subject::PrincipalId(principal_id.clone());
            if !runtime.owns_job_history(&owner, &id).await? {
                anyhow::bail!("Job '{id}' not found for Principal '{principal_id}'");
            }
            // ADR-078: a non-owner run reads only its own conversation's
            // live jobs; a deleted job's history belongs to the owner.
            let stamp = ctx.acting_for();
            if !stamp.is_owner() {
                verify_id_belongs_to_principal(&*runtime, &id, &principal_id, &stamp).await?;
            }
            id
        } else if let Some(label) = args.label {
            resolve_id_by_label(&*runtime, &label, &principal_id, &ctx.acting_for()).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for Cron action history"
            ));
        };

        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let owner = peko_subject::PrincipalId(principal_id);
        let runs = runtime.run_history(&owner, &job_id, limit).await?;
        Ok(json!({
            "job_id": job_id,
            "count": runs.len(),
            "runs": runs,
        }))
    }
}
