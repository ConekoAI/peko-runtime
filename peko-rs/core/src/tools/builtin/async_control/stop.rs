//! Async action stop tool — cancel a background task.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::tools::builtin::async_control::{
    build_cancel_response, AsyncTaskHelper, SharedAsyncRuntime,
};

/// Cancel an async task.
pub struct AsyncStopAction {
    helper: AsyncTaskHelper,
}

impl AsyncStopAction {
    /// Create a tool bound to a specific runtime.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            helper: AsyncTaskHelper::new(runtime),
        }
    }
}

#[async_trait]
impl Tool for AsyncStopAction {
    fn name(&self) -> &'static str {
        "Async"
    }

    fn description(&self) -> String {
        r"Cancel a pending or running task. Stopping a task that already ended is a no-op (`success: true, already_terminal: true`), not an error."
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
            .ok_or_else(|| anyhow::anyhow!("Async action stop requires 'task_id'"))?;

        let result = self.helper.cancel_task(task_id).await;
        Ok(build_cancel_response(result, task_id))
    }
}
