//! AsyncStop tool — cancel a background task.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Searches across all agent async registries by default.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::builtin::async_common::{build_cancel_response, AsyncTaskHelper};
use crate::tools::core::Tool;

/// Cancel an async task.
pub struct AsyncStopTool {
    helper: AsyncTaskHelper,
}

impl AsyncStopTool {
    /// Create a tool that cancels across all registries.
    #[must_use]
    pub fn global() -> Self {
        Self {
            helper: AsyncTaskHelper::global(),
        }
    }

    /// Create a tool bound to a specific registry.
    #[must_use]
    pub fn with_registry(
        registry: crate::extensions::framework::async_exec::executor::SharedAsyncTaskRegistry,
    ) -> Self {
        Self {
            helper: AsyncTaskHelper::with_registry(registry),
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

Returns: { success, task_id, previous_status?, message }"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::async_exec::executor::{
        AsyncTaskEntry, AsyncTaskStatus, AsyncToolConfig,
    };
    use crate::tools::ToolResult;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_async_stop_success() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let entry = AsyncTaskEntry::new(
                "Bash:cancel-me".to_string(),
                "Bash".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry);
        }

        let tool = AsyncStopTool::with_registry(registry);
        let result = tool
            .execute(json!({"task_id": "Bash:cancel-me"}))
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert_eq!(result["task_id"], "Bash:cancel-me");
        assert_eq!(result["previous_status"], "pending");
    }

    #[tokio::test]
    async fn test_async_stop_already_terminal() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "Bash:done".to_string(),
                "Bash".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.status = AsyncTaskStatus::Completed {
                result: ToolResult::success(json!({})),
            };
            reg.register(entry);
        }

        let tool = AsyncStopTool::with_registry(registry);
        let result = tool.execute(json!({"task_id": "Bash:done"})).await.unwrap();

        assert_eq!(result["success"], false);
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("already terminal"));
    }

    #[tokio::test]
    async fn test_async_stop_not_found() {
        let tool = AsyncStopTool::global();
        let result = tool
            .execute(json!({"task_id": "Bash:missing"}))
            .await
            .unwrap();

        assert_eq!(result["success"], false);
        assert_eq!(result["message"], "Task not found");
    }
}
