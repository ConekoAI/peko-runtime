//! Async action status tool — query the status of a background task.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::tools::builtin::async_control::{
    build_status_response, AsyncTaskHelper, SharedAsyncRuntime,
};

/// Query the status of an async task.
pub struct AsyncStatusAction {
    helper: AsyncTaskHelper,
}

impl AsyncStatusAction {
    /// Create a tool bound to a specific runtime.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            helper: AsyncTaskHelper::new(runtime),
        }
    }
}

#[async_trait]
impl Tool for AsyncStatusAction {
    fn name(&self) -> &'static str {
        "Async"
    }

    fn description(&self) -> String {
        r"A task's state and metadata, without its output or waiting. Returns `{task_id, tool_name, status, is_terminal, label, created_at, completed_at?, result?, duration_seconds?}`."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "task_id from the receipt"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Async action status requires 'task_id'"))?;

        match self.helper.lookup_task(task_id).await {
            Some(task) => Ok(build_status_response(&task)),
            None => Ok(json!({
                "error": "Task not found",
                "task_id": task_id
            })),
        }
    }
}
