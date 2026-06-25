//! `CronList` tool — list scheduled jobs
//!
//! Delegates to the daemon via IPC; the daemon is the source of truth for
//! cron persistence and execution.

use crate::ipc::{DaemonClient, ResponsePacket};
use crate::tools::builtin::cron::render_job_list;
use crate::tools::core::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `CronList` tool — list scheduled jobs
pub struct CronListTool;

impl CronListTool {
    /// Create a new `CronList` tool
    pub fn new() -> Self {
        Self
    }
}

impl Default for CronListTool {
    fn default() -> Self {
        Self::new()
    }
}

/// `CronList` tool arguments
///
/// Accepts an empty object; optional filters are peko extensions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronListArgs {
    /// Filter by job status (peko extension)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_filter: Option<String>,
    /// Filter by sub-command / schedule kind (peko extension)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_filter: Option<String>,
}

#[async_trait]
impl Tool for CronListTool {
    fn name(&self) -> &'static str {
        "CronList"
    }

    fn description(&self) -> String {
        "List scheduled jobs stored by the daemon.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "description": "Optional filter by status (peko extension)"
                },
                "kind_filter": {
                    "type": "string",
                    "description": "Optional filter by schedule kind: at, every, cron, idle, event (peko extension)"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let _args: CronListArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid CronList arguments: {e}"))?;

        let client = DaemonClient::connect().await.map_err(|e| {
            anyhow::anyhow!("Cannot reach daemon for cron operations. Is it running? ({e})")
        })?;

        match client.cron_list(true).await? {
            ResponsePacket::CronList { jobs, .. } => Ok(render_job_list(jobs)),
            ResponsePacket::Error { message, .. } => {
                Err(anyhow::anyhow!("Failed to list jobs: {message}"))
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_list_tool_name() {
        let tool = CronListTool::new();
        assert_eq!(tool.name(), "CronList");
    }

    #[test]
    fn test_cron_list_tool_parameters() {
        let tool = CronListTool::new();
        let params = tool.parameters();
        assert!(params.get("properties").is_some());
        assert!(params.get("required").is_none());
    }
}
