//! AsyncStop tool — cancel a background task.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::async_control::{build_cancel_response, AsyncTaskHelper, SharedAsyncRuntime};

/// Cancel an async task.
pub struct AsyncStopTool {
    helper: AsyncTaskHelper,
}

impl AsyncStopTool {
    /// Create a tool bound to a specific runtime.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            helper: AsyncTaskHelper::new(runtime),
        }
    }
}

#[async_trait]
impl Tool for AsyncStopTool {
    fn name(&self) -> &'static str {
        "AsyncStop"
    }

    fn description(&self) -> String {
        r"Cancel a running or pending background async task.

Works for ALL async tasks: Bash, Agent, Read, etc.

Parameters:
- task_id: string (required) — the task ID from the async receipt

Returns: { success, task_id, previous_status?, already_terminal, message }

If the task is already in a terminal state (completed / failed /
cancelled / timed_out), the response has `success: true,
already_terminal: true` so the model can treat it as a no-op rather
than an error."
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
            .ok_or_else(|| anyhow::anyhow!("AsyncStop requires 'task_id'"))?;

        let result = self.helper.cancel_task(task_id).await;
        Ok(build_cancel_response(result, task_id))
    }
}
