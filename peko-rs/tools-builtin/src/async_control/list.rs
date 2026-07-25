//! AsyncList tool — list background tasks in this runtime's scope.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::async_control::build_list_response;
use crate::async_control::{AsyncTaskHelper, SharedAsyncRuntime};

/// List async tasks in this runtime's scope.
pub struct AsyncListTool {
    helper: AsyncTaskHelper,
}

impl AsyncListTool {
    /// Create a tool bound to a specific runtime.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            helper: AsyncTaskHelper::new(runtime),
        }
    }
}

#[async_trait]
impl Tool for AsyncListTool {
    fn name(&self) -> &'static str {
        "AsyncList"
    }

    fn description(&self) -> String {
        r"List background async tasks.

Works for ALL async tasks: Bash, Agent, Read, etc.

Parameters:
- status_filter: string? — filter by status (pending, running, completed, failed, cancelled, timed_out)
- tool_filter: string? — filter by tool name (e.g., 'Bash', 'Agent')

Returns: { total, active, tasks: [{ task_id, tool_name, status, is_terminal, metadata_type, created_at, label }] }"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "enum": ["pending", "running", "completed", "failed", "cancelled", "timed_out"],
                    "description": "Optional filter by status: pending, running, completed, failed, cancelled, timed_out"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Optional filter by tool name (e.g., 'Bash', 'Agent')"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let status_filter = params.get("status_filter").and_then(|v| v.as_str());
        let tool_filter = params.get("tool_filter").and_then(|v| v.as_str());
        let tasks = self.helper.list_tasks(status_filter, tool_filter).await;
        Ok(build_list_response(tasks))
    }
}
