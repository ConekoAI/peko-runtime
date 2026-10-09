//! `Cron action trigger` tool — fire a scheduled job immediately
//!
//! Fires a `CronJob` out of schedule through the [`CronRuntime`] port.
//! The daemon routes the fire through `CronEngine::execute_job_for_id`,
//! so manual fires share the scheduled-fire coalescing rule (a fire
//! against a running job returns the in-flight run id instead of
//! double-firing) and land in the same run history.

use crate::tools::delete::{resolve_id_by_label, verify_id_belongs_to_principal};
use crate::tools::RuntimeBinding;
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `Cron action trigger` tool — fire a scheduled job now
pub struct CronTriggerAction {
    runtime: RuntimeBinding,
}

impl CronTriggerAction {
    /// Bind the action to the runtime its domain tool resolves.
    pub(crate) fn bound(runtime: RuntimeBinding) -> Self {
        Self { runtime }
    }
}

/// `Cron action trigger` tool arguments
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
impl Tool for CronTriggerAction {
    fn name(&self) -> &'static str {
        "Cron"
    }

    fn description(&self) -> String {
        "Fire a scheduled job immediately, out of schedule, by ID (or label). Works even when the job is disabled — use it to verify a freshly-created job's wiring before its first scheduled fire. The run executes in the background; if the job is already running, the fire coalesces into the in-flight run. Check the outcome with Cron action history.".to_string()
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

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "Cron action trigger requires a Principal context; use execute_with_context"
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
            .ok_or_else(|| anyhow::anyhow!("Cron action trigger requires a Principal context"))?
            .clone();

        let runtime = self.runtime.resolve().ok_or_else(|| {
            anyhow::anyhow!(
                "Cron action trigger requires the daemon's cron runtime; not initialized"
            )
        })?;

        let args: CronTriggerArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid Cron action trigger arguments: {e}"))?;

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            verify_id_belongs_to_principal(&*runtime, &id, &principal_id).await?;
            id
        } else if let Some(label) = args.label {
            resolve_id_by_label(&*runtime, &label, &principal_id).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for Cron action trigger"
            ));
        };

        let run_id = runtime.trigger_job(&job_id).await?;
        Ok(json!({
            "triggered": true,
            "job_id": job_id,
            "run_id": run_id,
            "note": "the job is running in the background; check the outcome with Cron action history",
        }))
    }
}
