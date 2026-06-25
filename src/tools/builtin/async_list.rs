//! AsyncList tool — list background tasks.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Lists tasks across all agent async registries by default.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::builtin::async_common::{build_list_response, AsyncTaskHelper};
use crate::tools::core::Tool;

/// List async tasks.
pub struct AsyncListTool {
    helper: AsyncTaskHelper,
}

impl AsyncListTool {
    /// Create a tool that lists across all registries.
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
    async fn test_async_list_empty() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let tool = AsyncListTool::with_registry(registry);
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["active"], 0);
    }

    #[tokio::test]
    async fn test_async_list_with_filters() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let mut entry1 = AsyncTaskEntry::new(
                "Bash:test-1".to_string(),
                "Bash".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry1.status = AsyncTaskStatus::Completed {
                result: ToolResult::success(json!({"done": true})),
            };
            reg.register(entry1);

            let entry2 = AsyncTaskEntry::new(
                "Agent:test-2".to_string(),
                "Agent".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry2);
        }

        let tool = AsyncListTool::with_registry(registry);

        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["active"], 1);

        let result = tool.execute(json!({"tool_filter": "Bash"})).await.unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["tool_name"], "Bash");

        let result = tool
            .execute(json!({"status_filter": "completed"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["status"], "completed");
    }
}
