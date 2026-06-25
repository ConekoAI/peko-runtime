//! `CronDelete` tool — cancel scheduled jobs
//!
//! Delegates to the daemon via IPC; the daemon is the source of truth for
//! cron persistence and execution.

use crate::ipc::{DaemonClient, ResponsePacket};
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
            "anyOf": [
                { "required": ["id"] },
                { "required": ["label"] }
            ]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: CronDeleteArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid CronDelete arguments: {e}"))?;

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            id
        } else if let Some(job_id) = args.job_id.filter(|s| !s.is_empty()) {
            job_id
        } else if let Some(label) = args.label {
            Self::resolve_id_by_label(&label).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for CronDelete"
            ));
        };

        let client = DaemonClient::connect().await.map_err(|e| {
            anyhow::anyhow!("Cannot reach daemon for cron operations. Is it running? ({e})")
        })?;

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
    /// Find a job ID by its label
    async fn resolve_id_by_label(label: &str) -> anyhow::Result<String> {
        let client = DaemonClient::connect().await.map_err(|e| {
            anyhow::anyhow!("Cannot reach daemon for cron operations. Is it running? ({e})")
        })?;
        match client.cron_list(true).await? {
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
        assert!(params.get("anyOf").is_some());
    }
}
