//! AsyncStatus tool — query the status of a background task.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Searches across all agent async registries by default.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::builtin::async_common::{build_status_response, AsyncTaskHelper};
use crate::tools::core::Tool;

/// Query the status of an async task.
pub struct AsyncStatusTool {
    helper: AsyncTaskHelper,
}

impl AsyncStatusTool {
    /// Create a tool that searches across all registries.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::async_exec::executor::{AsyncTaskEntry, AsyncToolConfig};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_async_status_not_found() {
        let tool = AsyncStatusTool::global();
        let result = tool
            .execute(json!({"task_id": "nonexistent:task"}))
            .await
            .unwrap();
        assert_eq!(result["error"], "Task not found");
        assert_eq!(result["task_id"], "nonexistent:task");
    }

    #[tokio::test]
    async fn test_async_status_with_registry() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let entry = AsyncTaskEntry::new(
                "Bash:test-123".to_string(),
                "Bash".to_string(),
                json!({"command": "echo hello"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry);
        }

        let tool = AsyncStatusTool::with_registry(registry);
        let result = tool
            .execute(json!({"task_id": "Bash:test-123"}))
            .await
            .unwrap();

        assert_eq!(result["task_id"], "Bash:test-123");
        assert_eq!(result["tool_name"], "Bash");
        assert_eq!(result["status"], "pending");
        assert_eq!(result["metadata_type"], "none");
    }
}
