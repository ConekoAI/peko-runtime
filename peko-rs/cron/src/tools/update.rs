//! `Cron action update` tool — patch mutable fields of a scheduled job
//!
//! Updates a `CronJob` through the [`CronRuntime`] port set by the
//! daemon at startup, like the sibling cron tools. Covers the two
//! fields an agent legitimately toggles after creation: `enabled`
//! (pause/resume without delete) and `wake_on_completion` (result
//! subscription — whether each fire's outcome steers back into the
//! creating session's inbox (trunk fallback)).

use crate::tools::delete::{resolve_id_by_label, verify_id_belongs_to_principal};
use crate::tools::RuntimeBinding;
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
    /// Create a new `Cron action update` tool
    pub fn new() -> Self {
        Self::bound(RuntimeBinding::default())
    }

    /// Bind the action to the runtime its domain tool resolves.
    pub(crate) fn bound(runtime: RuntimeBinding) -> Self {
        Self { runtime }
    }
}

impl Default for CronUpdateAction {
    fn default() -> Self {
        Self::new()
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
}

#[async_trait]
impl Tool for CronUpdateAction {
    fn name(&self) -> &'static str {
        "Cron"
    }

    fn description(&self) -> String {
        "Update a scheduled job's mutable fields by ID (or label): `enabled` pauses/resumes without deleting (re-enabling resets the failure budget); `wake_on_completion` subscribes or unsubscribes you to the job's result on each fire (tool jobs only — message jobs already land in your session).".to_string()
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
                }
            },
            "allOf": [
                {"anyOf": [{"required": ["id"]}, {"required": ["label"]}]},
                {"anyOf": [{"required": ["enabled"]}, {"required": ["wake_on_completion"]}]}
            ]
        })
    }

    /// F33: cron DB write — opt out of parallel dispatch. See
    /// `Cron action create::parallelizable` for the rationale.
    fn parallelizable(&self) -> bool {
        false
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

        if args.enabled.is_none() && args.wake_on_completion.is_none() {
            return Err(anyhow::anyhow!(
                "Cron action update requires at least one field to change: `enabled` or `wake_on_completion`"
            ));
        }

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            verify_id_belongs_to_principal(&*runtime, &id, &principal_id).await?;
            id
        } else if let Some(label) = args.label {
            resolve_id_by_label(&*runtime, &label, &principal_id).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for Cron action update"
            ));
        };

        runtime
            .update_job(&job_id, args.enabled, args.wake_on_completion)
            .await?;
        Ok(json!({
            "updated": true,
            "job_id": job_id,
            "enabled": args.enabled,
            "wake_on_completion": args.wake_on_completion,
        }))
    }
}
