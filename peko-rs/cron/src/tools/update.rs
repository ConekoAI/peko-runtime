//! `CronUpdate` tool — patch mutable fields of a scheduled job
//!
//! Updates a `CronJob` through the [`CronRuntime`] port set by the
//! daemon at startup, like the sibling cron tools. Covers the two
//! fields an agent legitimately toggles after creation: `enabled`
//! (pause/resume without delete) and `wake_on_completion` (result
//! subscription — whether each fire's outcome steers back into the
//! principal's trunk inbox).

use crate::tools::delete::{resolve_id_by_label, verify_id_belongs_to_principal};
use crate::tools::global_runtime;
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `CronUpdate` tool — patch a scheduled job's mutable fields
pub struct CronUpdateTool;

impl CronUpdateTool {
    /// Create a new `CronUpdate` tool
    pub fn new() -> Self {
        Self
    }
}

impl Default for CronUpdateTool {
    fn default() -> Self {
        Self::new()
    }
}

/// `CronUpdate` tool arguments
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
    /// Subscribe/unsubscribe the principal's trunk inbox to the job's
    /// result each fire (SpawnTool jobs only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_on_completion: Option<bool>,
}

#[async_trait]
impl Tool for CronUpdateTool {
    fn name(&self) -> &'static str {
        "CronUpdate"
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
                    "description": "Subscribe (true) or unsubscribe (false) the trunk inbox to the job's result each fire. SpawnTool jobs only; message jobs already run in your session."
                }
            }
        })
    }

    /// F33: cron DB write — opt out of parallel dispatch. See
    /// `CronCreate::parallelizable` for the rationale.
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "CronUpdate requires a Principal context; use execute_with_context"
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
            .ok_or_else(|| anyhow::anyhow!("CronUpdate requires a Principal context"))?
            .clone();

        let runtime = global_runtime().ok_or_else(|| {
            anyhow::anyhow!("CronUpdate requires the daemon's cron runtime; not initialized")
        })?;

        let args: CronUpdateArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid CronUpdate arguments: {e}"))?;

        if args.enabled.is_none() && args.wake_on_completion.is_none() {
            return Err(anyhow::anyhow!(
                "CronUpdate requires at least one field to change: `enabled` or `wake_on_completion`"
            ));
        }

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            verify_id_belongs_to_principal(&*runtime, &id, &principal_id).await?;
            id
        } else if let Some(label) = args.label {
            resolve_id_by_label(&*runtime, &label, &principal_id).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for CronUpdate"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_update_tool_name() {
        let tool = CronUpdateTool::new();
        assert_eq!(tool.name(), "CronUpdate");
    }

    #[test]
    fn test_cron_update_tool_parameters() {
        let tool = CronUpdateTool::new();
        let params = tool.parameters();
        let props = params.get("properties").unwrap();
        assert!(props.get("id").is_some());
        assert!(props.get("label").is_some());
        assert!(props.get("enabled").is_some());
        assert!(props.get("wake_on_completion").is_some());
    }

    #[test]
    fn test_cron_update_args_roundtrip() {
        let args: CronUpdateArgs = serde_json::from_value(json!({
            "id": "cron_x",
            "enabled": false,
        }))
        .unwrap();
        assert_eq!(args.id.as_deref(), Some("cron_x"));
        assert_eq!(args.enabled, Some(false));
        assert_eq!(args.wake_on_completion, None);
    }
}
