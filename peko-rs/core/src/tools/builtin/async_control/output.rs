//! Async action output tool — read the result of a background task.
//!
//! Optionally blocks until the task reaches a terminal state via
//! `AsyncRuntime::wait_for_completion`. The per-runtime adapter in
//! the framework host threads the actual `AsyncExecutor` through
//! `wait_for_completion` so blocking reads work uniformly.

use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

use peko_tools_core::traits::Tool;

use crate::tools::builtin::async_control::{
    build_output_response, AsyncTaskHelper, SharedAsyncRuntime,
};

/// Read the output of an async task.
pub struct AsyncOutputAction {
    helper: AsyncTaskHelper,
}

impl AsyncOutputAction {
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
impl Tool for AsyncOutputAction {
    fn name(&self) -> &'static str {
        "Async"
    }

    fn description(&self) -> String {
        r"Read a task's result. `block: true` waits for the task to end. Returns `{task_id, status, is_terminal, result?, completed_at?, elapsed_seconds?}`."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "task_id from the receipt"
                },
                "block": {
                    "type": "boolean",
                    "description": "Wait for the task to end",
                    "default": false
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "How long block waits, in ms (default 300000); the task keeps running"
                },
                "tail_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Only the last N lines",
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
            .ok_or_else(|| anyhow::anyhow!("Async action output requires 'task_id'"))?;
        let block = params
            .get("block")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timeout_ms = params
            .get("timeout_ms")
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
                // §4.1: a still-running task reports the tail of its live
                // progress buffer, so a polling agent sees what the task
                // has produced instead of a bare status string.
                return Ok(json!({
                    "task_id": task_id,
                    "status": task.status,
                    "is_terminal": false,
                    "result": null,
                    "partial_output": task.partial_output,
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
