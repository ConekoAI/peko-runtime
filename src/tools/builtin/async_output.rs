//! AsyncOutput tool — read the result of a background task.
//!
//! Part of the Async* family that replaces the single `task` tool.
//! Requires an `AsyncExecutor` for blocking waits.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use crate::extensions::framework::async_exec::executor::AsyncExecutor;
use crate::tools::builtin::async_common::{build_output_response, AsyncTaskHelper};
use crate::tools::core::Tool;

/// Read the output of an async task.
pub struct AsyncOutputTool {
    helper: AsyncTaskHelper,
    executor: Option<Arc<AsyncExecutor>>,
}

impl AsyncOutputTool {
    /// Create a tool without an executor (cannot block; returns current state).
    #[must_use]
    pub fn global() -> Self {
        Self {
            helper: AsyncTaskHelper::global(),
            executor: None,
        }
    }

    /// Create a tool bound to a specific registry (cannot block).
    #[must_use]
    pub fn with_registry(
        registry: crate::extensions::framework::async_exec::executor::SharedAsyncTaskRegistry,
    ) -> Self {
        Self {
            helper: AsyncTaskHelper::with_registry(registry),
            executor: None,
        }
    }

    /// Create a tool with an executor for blocking output reads.
    #[must_use]
    pub fn with_executor(executor: Arc<AsyncExecutor>) -> Self {
        Self {
            helper: AsyncTaskHelper::global(),
            executor: Some(executor),
        }
    }

    /// Create a tool with both a registry and an executor.
    #[must_use]
    pub fn with_registry_and_executor(
        registry: crate::extensions::framework::async_exec::executor::SharedAsyncTaskRegistry,
        executor: Arc<AsyncExecutor>,
    ) -> Self {
        Self {
            helper: AsyncTaskHelper::with_registry(registry),
            executor: Some(executor),
        }
    }
}

#[async_trait]
impl Tool for AsyncOutputTool {
    fn name(&self) -> &'static str {
        "AsyncOutput"
    }

    fn description(&self) -> String {
        r"Read the output of a background async task.

Works for ALL async tasks: Bash, Agent, Read, etc.

Parameters:
- task_id: string (required) — the task ID from the async receipt
- block: boolean (default false) — if true, wait until the task reaches a terminal state
- timeout: integer? — milliseconds to wait when block=true (default 5 minutes)
- tail_lines: integer (default 0) — if >0, return only the last N lines

Returns: { task_id, status, is_terminal, result?, completed_at?, elapsed_seconds? }"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID from the async receipt (e.g., 'Bash:abc-123')"
                },
                "block": {
                    "type": "boolean",
                    "description": "If true, wait until the task reaches a terminal state before returning.",
                    "default": false
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional timeout in milliseconds when block=true (default 5 minutes)."
                },
                "tail_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "If >0, return only the last N lines of output.",
                    "default": 0
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("AsyncOutput requires 'task_id'"))?;
        let block = params
            .get("block")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timeout_ms = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(5 * 60 * 1000);
        let tail_lines = params
            .get("tail_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // blocking reads require an executor; check early so the missing-
        // executor error surfaces even if the task_id does not exist.
        if block && self.executor.is_none() {
            return Ok(json!({
                "error": "AsyncOutput cannot block without an AsyncExecutor",
                "task_id": task_id,
            }));
        }

        let task = match self.helper.lookup_task(task_id).await {
            Some(t) => t,
            None => {
                return Ok(json!({
                    "error": "Task not found",
                    "task_id": task_id,
                }));
            }
        };

        if !task.is_terminal() {
            if !block {
                return Ok(json!({
                    "task_id": task_id,
                    "status": task.status.as_str(),
                    "is_terminal": false,
                    "result": null,
                }));
            }
            let executor = self.executor.as_ref().expect("checked above");
            let timeout = Duration::from_millis(timeout_ms);
            let _ = executor
                .wait_for_completion(&task_id.to_string(), timeout)
                .await;
            let task = match self.helper.lookup_task(task_id).await {
                Some(t) => t,
                None => {
                    return Ok(json!({
                        "error": "Task not found",
                        "task_id": task_id,
                    }));
                }
            };
            return Ok(build_output_response(&task, tail_lines));
        }

        Ok(build_output_response(&task, tail_lines))
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

    fn make_tool_with_registry(
        registry: crate::extensions::framework::async_exec::executor::SharedAsyncTaskRegistry,
    ) -> (AsyncOutputTool, Arc<AsyncExecutor>) {
        let executor = Arc::new(AsyncExecutor::new());
        let tool = AsyncOutputTool::with_registry_and_executor(registry, executor.clone());
        (tool, executor)
    }

    #[tokio::test]
    async fn test_async_output_missing_executor_returns_error() {
        let tool = AsyncOutputTool::global();
        let result = tool
            .execute(json!({"task_id": "Bash:x", "block": true}))
            .await
            .unwrap();
        assert_eq!(
            result["error"],
            "AsyncOutput cannot block without an AsyncExecutor"
        );
    }

    #[tokio::test]
    async fn test_async_output_tail_lines_string_result() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let result_value = json!("line1\nline2\nline3\nline4\nline5");
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "Bash:string-result".to_string(),
                "Bash".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry(registry);
        let result = tool
            .execute(json!({
                "task_id": "Bash:string-result",
                "tail_lines": 2
            }))
            .await
            .unwrap();

        assert_eq!(result["result"], "line4\nline5");
    }

    #[tokio::test]
    async fn test_async_output_tail_lines_object_with_stdout() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let result_value = json!({
            "stdout": "line1\nline2\nline3",
            "exit_code": 0
        });
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "Bash:obj-result".to_string(),
                "Bash".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry(registry);
        let result = tool
            .execute(json!({
                "task_id": "Bash:obj-result",
                "tail_lines": 2
            }))
            .await
            .unwrap();

        assert_eq!(result["result"]["stdout"], "line2\nline3");
        assert_eq!(result["result"]["exit_code"], 0);
    }

    #[tokio::test]
    async fn test_async_output_tail_lines_unknown_shape_passthrough() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let result_value = json!({"count": 42});
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "Bash:unknown-shape".to_string(),
                "Bash".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry(registry);
        let result = tool
            .execute(json!({
                "task_id": "Bash:unknown-shape",
                "tail_lines": 10
            }))
            .await
            .unwrap();

        assert_eq!(result["result"], json!({"count": 42}));
    }

    #[tokio::test]
    async fn test_async_output_tail_lines_zero_passthrough() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let result_value = json!("line1\nline2\nline3\nline4\nline5");
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "Bash:zero".to_string(),
                "Bash".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry(registry);
        let result = tool.execute(json!({"task_id": "Bash:zero"})).await.unwrap();

        assert_eq!(result["result"], "line1\nline2\nline3\nline4\nline5");
    }
}
