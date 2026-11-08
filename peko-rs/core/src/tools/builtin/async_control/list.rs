//! Async action list tool — list background tasks in this runtime's scope.

use async_trait::async_trait;
use serde_json::json;

use peko_tools_core::traits::Tool;

use crate::tools::builtin::async_control::build_list_response;
use crate::tools::builtin::async_control::{AsyncTaskHelper, SharedAsyncRuntime};

/// List async tasks in this runtime's scope.
pub struct AsyncListAction {
    helper: AsyncTaskHelper,
}

impl AsyncListAction {
    /// Create a tool bound to a specific runtime.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            helper: AsyncTaskHelper::new(runtime),
        }
    }
}

#[async_trait]
impl Tool for AsyncListAction {
    fn name(&self) -> &'static str {
        "Async"
    }

    fn description(&self) -> String {
        r"Your background tasks, optionally filtered. Returns `{total, active, tasks}`.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "enum": ["pending", "running", "completed", "failed", "cancelled", "timed_out"],
                    "description": "Only tasks in this state"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Only tasks of this tool"
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
