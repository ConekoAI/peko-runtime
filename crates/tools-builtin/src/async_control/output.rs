//! AsyncOutput tool — read the result of a background task.
//!
//! Optionally blocks until the task reaches a terminal state via
//! `AsyncRuntime::wait_for_completion`. The per-runtime adapter in
//! the framework host threads the actual `AsyncExecutor` through
//! `wait_for_completion` so blocking reads work uniformly.

use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

use peko_tools_core::traits::Tool;

use crate::async_control::{build_output_response, AsyncTaskHelper, SharedAsyncRuntime};

/// Read the output of an async task.
pub struct AsyncOutputTool {
    helper: AsyncTaskHelper,
}

impl AsyncOutputTool {
    /// Create a tool bound to a specific runtime.
    ///
    /// Blocking reads are routed through `AsyncRuntime::wait_for_completion`;
    /// the runtime adapter backs that method with the per-agent `AsyncExecutor`.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            helper: AsyncTaskHelper::new(runtime),
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
                    "status": task.status,
                    "is_terminal": false,
                    "result": null,
                }));
            }
            // Block via the runtime's wait_for_completion — the runtime
            // adapter backs this with `AsyncExecutor::wait_for_completion`
            // which threads the underlying registry. Errors here mean
            // the runtime couldn't wait (rare); an Ok return reflects
            // the task's terminal status, whatever it was.
            let _ = self
                .helper
                .runtime_handle()
                .wait_for_completion(task_id, Duration::from_millis(timeout_ms))
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
