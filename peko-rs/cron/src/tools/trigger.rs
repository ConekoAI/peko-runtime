//! `CronTrigger` tool — fire a scheduled job immediately
//!
//! Fires a `CronJob` out of schedule through the [`CronRuntime`] port.
//! The daemon routes the fire through `CronEngine::execute_job_for_id`,
//! so manual fires share the scheduled-fire coalescing rule (a fire
//! against a running job returns the in-flight run id instead of
//! double-firing) and land in the same run history.

use crate::tools::delete::{resolve_id_by_label, verify_id_belongs_to_principal};
use crate::tools::global_runtime;
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `CronTrigger` tool — fire a scheduled job now
pub struct CronTriggerTool;

impl CronTriggerTool {
    /// Create a new `CronTrigger` tool
    pub fn new() -> Self {
        Self
    }
}

impl Default for CronTriggerTool {
    fn default() -> Self {
        Self::new()
    }
}

/// `CronTrigger` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTriggerArgs {
    /// Job ID to fire
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional label to fire (alternative to `id`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[async_trait]
impl Tool for CronTriggerTool {
    fn name(&self) -> &'static str {
        "CronTrigger"
    }

    fn description(&self) -> String {
        "Fire a scheduled job immediately, out of schedule, by ID (or label). Works even when the job is disabled — use it to verify a freshly-created job's wiring before its first scheduled fire. The run executes in the background; if the job is already running, the fire coalesces into the in-flight run. Check the outcome with CronHistory.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "ID of the scheduled job to fire now"
                },
                "label": {
                    "type": "string",
                    "description": "Label of the scheduled job to fire now (alternative to id)"
                }
            },
            "oneOf": [
                { "required": ["id"] },
                { "required": ["label"] }
            ]
        })
    }

    /// F33: cron DB write — opt out of parallel dispatch. See
    /// `CronCreate::parallelizable` for the rationale.
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "CronTrigger requires a Principal context; use execute_with_context"
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
            .ok_or_else(|| anyhow::anyhow!("CronTrigger requires a Principal context"))?
            .clone();

        let runtime = global_runtime().ok_or_else(|| {
            anyhow::anyhow!("CronTrigger requires the daemon's cron runtime; not initialized")
        })?;

        let args: CronTriggerArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid CronTrigger arguments: {e}"))?;

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            verify_id_belongs_to_principal(&*runtime, &id, &principal_id).await?;
            id
        } else if let Some(label) = args.label {
            resolve_id_by_label(&*runtime, &label, &principal_id).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for CronTrigger"
            ));
        };

        let run_id = runtime.trigger_job(&job_id).await?;
        Ok(json!({
            "triggered": true,
            "job_id": job_id,
            "run_id": run_id,
            "note": "the job is running in the background; check the outcome with CronHistory",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_trigger_tool_name() {
        let tool = CronTriggerTool::new();
        assert_eq!(tool.name(), "CronTrigger");
    }

    #[test]
    fn test_cron_trigger_tool_parameters() {
        let tool = CronTriggerTool::new();
        let params = tool.parameters();
        let branches = params
            .get("oneOf")
            .expect("CronTrigger schema must use oneOf for id-or-label");
        assert_eq!(branches.as_array().unwrap().len(), 2);
    }
}
