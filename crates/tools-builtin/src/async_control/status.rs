//! AsyncStatus tool — query the status of a background task.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::async_control::{build_status_response, AsyncTaskHelper, SharedAsyncRuntime};

/// Query the status of an async task.
pub struct AsyncStatusTool {
    helper: AsyncTaskHelper,
}

impl AsyncStatusTool {
    /// Create a tool bound to a specific runtime.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            helper: AsyncTaskHelper::new(runtime),
        }
    }
}

#[async_trait]
impl Tool for AsyncStatusTool {
    fn name(&self) -> &'static str {
        "AsyncStatus"
    }

    fn description(&self) -> String {
        r"Check the status of a background async task.

Works for ALL async tasks: Bash, Agent, Read, etc.

Parameters:
- task_id: string (required) — the task ID from the async receipt

Returns: { task_id, tool_name, status, is_terminal, parent_session_key, metadata_type, created_at, label, completed_at?, result?, duration_seconds? }"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID from the async receipt (e.g., 'Bash:abc-123')"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("AsyncStatus requires 'task_id'"))?;

        match self.helper.lookup_task(task_id).await {
            Some(task) => Ok(build_status_response(&task)),
            None => Ok(json!({
                "error": "Task not found",
                "task_id": task_id
            })),
        }
    }
}
