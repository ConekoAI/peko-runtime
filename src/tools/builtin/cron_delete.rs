//! `CronDelete` tool — cancel scheduled jobs
//!
//! Delegates to the daemon via IPC; the daemon is the source of truth for
//! cron persistence and execution. Label and ID resolution are scoped to
//! the current Principal from the tool execution context.

use crate::ipc::{DaemonClient, ResponsePacket};
use crate::tools::core::exec::ToolContext;
use crate::tools::core::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `CronDelete` tool — cancel scheduled jobs
pub struct CronDeleteTool;

impl CronDeleteTool {
    /// Create a new `CronDelete` tool
    pub fn new() -> Self {
        Self
    }
}

impl Default for CronDeleteTool {
    fn default() -> Self {
        Self::new()
    }
}

/// `CronDelete` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronDeleteArgs {
    /// Job ID to cancel
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Legacy alias for `id` (peko extension)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Optional label to cancel (peko extension; alternative to `id`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[async_trait]
impl Tool for CronDeleteTool {
    fn name(&self) -> &'static str {
        "CronDelete"
    }

    fn description(&self) -> String {
        "Cancel a scheduled job by ID (or by label as a peko extension).".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        // `oneOf` is the right combinator here: callers must supply
        // exactly one of `id` or `label` (and supplying both is a usage
        // error, not a coercion case). `anyOf` would silently accept both,
        // which we want to surface. The previous schema used `anyOf` +
        // nested `required` — that's not a valid JSON Schema combination
        // and several validators reject it.
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "ID of the scheduled job to cancel"
                },
                "label": {
                    "type": "string",
                    "description": "Label of the scheduled job to cancel (peko extension)"
                }
            },
            "oneOf": [
                { "required": ["id"] },
                { "required": ["label"] }
            ]
        })
    }

    /// F33: cron DB write — opt out of parallel dispatch. See
    /// `CronCreate::parallelizable` for the rationale (single-row
    /// delete is atomic but interleaving with a concurrent create or
    /// delete by the same id can race).
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "CronDelete requires a Principal context; use execute_with_context"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value> {
        let principal_name = ctx
            .principal_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("CronDelete requires a Principal context"))?
            .clone();

        let args: CronDeleteArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid CronDelete arguments: {e}"))?;

        let client = DaemonClient::connect().await.map_err(|e| {
            anyhow::anyhow!("Cannot reach daemon for cron operations. Is it running? ({e})")
        })?;

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            Self::verify_id_belongs_to_principal(&client, &id, &principal_name).await?;
            id
        } else if let Some(job_id) = args.job_id.filter(|s| !s.is_empty()) {
            Self::verify_id_belongs_to_principal(&client, &job_id, &principal_name).await?;
            job_id
        } else if let Some(label) = args.label {
            Self::resolve_id_by_label(&client, &label, &principal_name).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for CronDelete"
            ));
        };

        match client.cron_remove(&job_id).await? {
            ResponsePacket::CronRemoved { .. } => Ok(json!({
                "cancelled": true,
                "job_id": job_id,
            })),
            ResponsePacket::Error { message, .. } => {
                Err(anyhow::anyhow!("Failed to cancel job: {message}"))
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }
}

impl CronDeleteTool {
    /// Find a job ID by its label, restricted to the given Principal.
    async fn resolve_id_by_label(
        client: &DaemonClient,
        label: &str,
        principal_name: &str,
    ) -> anyhow::Result<String> {
        match client
            .cron_list(true, Some(principal_name.to_string()))
            .await?
        {
            ResponsePacket::CronList { jobs, .. } => jobs
                .into_iter()
                .find(|j| j.name == label)
                .ok_or_else(|| anyhow::anyhow!("Job with label '{label}' not found"))
                .map(|j| j.id),
            ResponsePacket::Error { message, .. } => {
                Err(anyhow::anyhow!("Failed to list jobs for cancel: {message}"))
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }

    /// Verify that an explicit job ID belongs to the given Principal.
    async fn verify_id_belongs_to_principal(
        client: &DaemonClient,
        job_id: &str,
        principal_name: &str,
    ) -> anyhow::Result<()> {
        match client
            .cron_list(true, Some(principal_name.to_string()))
            .await?
        {
            ResponsePacket::CronList { jobs, .. } => {
                if jobs.into_iter().any(|j| j.id == job_id) {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "Job '{job_id}' not found for Principal '{principal_name}'"
                    ))
                }
            }
            ResponsePacket::Error { message, .. } => {
                Err(anyhow::anyhow!("Failed to list jobs for cancel: {message}"))
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_delete_tool_name() {
        let tool = CronDeleteTool::new();
        assert_eq!(tool.name(), "CronDelete");
    }

    #[test]
    fn test_cron_delete_tool_parameters() {
        let tool = CronDeleteTool::new();
        let params = tool.parameters();
        assert!(params.get("properties").is_some());
        // The schema uses `oneOf` so that callers who supply both `id`
        // and `label` get a validation error instead of silent acceptance.
        let branches = params
            .get("oneOf")
            .expect("CronDelete schema must use oneOf for id-or-label");
        assert!(branches.is_array());
        assert_eq!(branches.as_array().unwrap().len(), 2);
    }
}
