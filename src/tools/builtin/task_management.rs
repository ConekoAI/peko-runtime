//! Universal Task Management Tool
//!
//! Provides `task` — a single tool for managing ANY async task.
//! Replaces `task_status` and `task_list` (Issue 011) with one unified interface.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::extensions::framework::async_exec::executor::{
    cancel_task_across_all_registries, find_task_across_all_registries,
    list_all_tasks_across_all_registries, CancelResult, SharedAsyncTaskRegistry, TaskView,
};
use crate::tools::core::Tool;

// ------------------------------------------------------------------------------
// TaskAction — serde-driven, extensible
// ------------------------------------------------------------------------------

/// Actions supported by the `task` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskAction {
    Status,
    List,
    Cancel,
    Spawn,
    Output,
}

// ------------------------------------------------------------------------------
// TaskTool — unified interface
// ------------------------------------------------------------------------------

/// Unified task management tool.
pub struct TaskTool {
    registry: Option<SharedAsyncTaskRegistry>,
    executor: Option<Arc<crate::extensions::framework::async_exec::executor::AsyncExecutor>>,
    extension_core: Option<std::sync::Weak<crate::extensions::framework::core::ExtensionCore>>,
}

impl TaskTool {
    #[must_use]
    pub fn with_registry(registry: SharedAsyncTaskRegistry) -> Self {
        Self {
            registry: Some(registry),
            executor: None,
            extension_core: None,
        }
    }

    #[must_use]
    pub fn global() -> Self {
        Self {
            registry: None,
            executor: None,
            extension_core: None,
        }
    }

    /// Construct with executor + extension core. Required for `spawn` and
    /// `output` actions; read-only actions (`status`, `list`, `cancel`)
    /// still work without them.
    #[must_use]
    pub fn with_executor_and_core(
        executor: Arc<crate::extensions::framework::async_exec::executor::AsyncExecutor>,
        extension_core: std::sync::Weak<crate::extensions::framework::core::ExtensionCore>,
    ) -> Self {
        Self {
            registry: None,
            executor: Some(executor),
            extension_core: Some(extension_core),
        }
    }

    // ------------------------------------------------------------------
    // Internal helpers — DRY across all actions
    // ------------------------------------------------------------------

    async fn lookup_task(&self, task_id: &str) -> Option<TaskView> {
        match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.get(&task_id.to_string()).map(TaskView::from_entry)
            }
            None => {
                let entry = find_task_across_all_registries(task_id).await?;
                Some(TaskView::from_entry(&entry))
            }
        }
    }

    async fn list_tasks(
        &self,
        status_filter: Option<&str>,
        tool_filter: Option<&str>,
    ) -> Vec<TaskView> {
        let entries = match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.list_tasks(None)
            }
            None => list_all_tasks_across_all_registries().await,
        };

        entries
            .into_iter()
            .map(|e| TaskView::from_entry(&e))
            .filter(|t| {
                status_filter.map_or(true, |f| t.status.as_str() == f)
                    && tool_filter.map_or(true, |f| t.tool_name == f)
            })
            .collect()
    }

    async fn cancel_task(&self, task_id: &str) -> CancelResult {
        match &self.registry {
            Some(registry) => {
                let mut reg = registry.write().await;
                reg.cancel(&task_id.to_string())
            }
            None => cancel_task_across_all_registries(task_id).await,
        }
    }

    // ------------------------------------------------------------------
    // Response builders — keep execute() readable, DRY field mapping
    // ------------------------------------------------------------------

    fn build_status_response(task: &TaskView) -> serde_json::Value {
        let mut base = json!({
            "task_id": task.task_id,
            "tool_name": task.tool_name,
            "status": task.status.as_str(),
            "is_terminal": task.is_terminal(),
            "parent_session_key": task.parent_session_key,
            "metadata_type": task.metadata_type,
            "created_at": task.created_at.to_rfc3339(),
            "label": task.label,
        });

        if let Some(completed_at) = task.completed_at {
            base["completed_at"] = json!(completed_at.to_rfc3339());
        }
        if let Some(ref result) = task.result {
            base["result"] = result.clone();
        }
        if let Some(duration) = task.duration() {
            base["duration_seconds"] = json!(duration.num_seconds());
        }

        base
    }

    fn build_list_response(tasks: Vec<TaskView>) -> serde_json::Value {
        let active_count = tasks.iter().filter(|t| !t.is_terminal()).count();

        let task_jsons: Vec<_> = tasks
            .into_iter()
            .map(|t| {
                json!({
                    "task_id": t.task_id,
                    "tool_name": t.tool_name,
                    "status": t.status.as_str(),
                    "is_terminal": t.is_terminal(),
                    "metadata_type": t.metadata_type,
                    "created_at": t.created_at.to_rfc3339(),
                    "label": t.label,
                })
            })
            .collect();

        json!({
            "total": task_jsons.len(),
            "active": active_count,
            "tasks": task_jsons
        })
    }

    fn build_cancel_response(result: CancelResult, task_id: &str) -> serde_json::Value {
        match result {
            CancelResult::Success { previous } => json!({
                "success": true,
                "task_id": task_id,
                "previous_status": previous,
                "message": "Task cancelled",
            }),
            CancelResult::AlreadyTerminal { previous } => json!({
                "success": false,
                "task_id": task_id,
                "previous_status": previous,
                "message": format!("Task already terminal: {previous}"),
            }),
            CancelResult::NotFound => json!({
                "success": false,
                "task_id": task_id,
                "message": "Task not found",
            }),
        }
    }

    fn build_output_response(task: &TaskView, tail_lines: u64) -> serde_json::Value {
        let mut base = json!({
            "task_id": task.task_id,
            "status": task.status.as_str(),
            "is_terminal": task.is_terminal(),
        });
        if let Some(ref result) = task.result {
            base["result"] = apply_tail_lines(result, tail_lines);
        }
        if let Some(completed_at) = task.completed_at {
            base["completed_at"] = json!(completed_at.to_rfc3339());
        }
        if let Some(duration) = task.duration() {
            base["elapsed_seconds"] = json!(duration.num_seconds());
        }
        base
    }
}

/// Apply `tail_lines` filtering to a tool result value. Returns the
/// filtered value. Recognizes two shapes: a JSON string (truncate lines
/// directly) and a JSON object with a string `stdout` field (truncate
/// that field, leave the rest). Other shapes pass through unchanged.
fn apply_tail_lines(result: &serde_json::Value, tail_lines: u64) -> serde_json::Value {
    if tail_lines == 0 {
        return result.clone();
    }
    let last_n = |s: &str| -> String {
        let mut lines: Vec<&str> = s.lines().collect();
        if lines.len() > tail_lines as usize {
            lines = lines.split_off(lines.len() - tail_lines as usize);
        }
        lines.join("\n")
    };
    if let Some(s) = result.as_str() {
        return serde_json::Value::String(last_n(s));
    }
    if let Some(obj) = result.as_object() {
        if let Some(stdout) = obj.get("stdout").and_then(|v| v.as_str()) {
            let mut new_obj = obj.clone();
            new_obj.insert("stdout".to_string(), serde_json::Value::String(last_n(stdout)));
            return serde_json::Value::Object(new_obj);
        }
    }
    result.clone()
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> String {
        r"Manage async tasks: check status, list tasks, cancel, spawn, or read output.

Works for ALL async tasks: shell, grep, agent_spawn, a2a_send, etc.

Actions:
- status: get one task by id
- list: query tasks (optionally filter by status or tool name)
- cancel: stop a running task
- spawn: invoke any tool asynchronously, returns a task receipt
- output: read a task's output (optionally wait for completion)

Parameters:
- action: 'status', 'list', 'cancel', 'spawn', or 'output' (required)
- task_id: required for 'status', 'cancel', 'output' — the task ID from the receipt
- tool: required for 'spawn' — the tool name to invoke
- params: required for 'spawn' — parameters to pass to the tool
- status_filter: optional for 'list' — filter by status
- tool_filter: optional for 'list' — filter by tool name
- blocking: optional for 'output' — if true, wait until task reaches terminal state
- tail_lines: optional for 'output' — if >0, return only the last N lines

Returns structured data appropriate to the action.
'spawn' and 'output' require TaskTool to be constructed with an AsyncExecutor."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "cancel", "spawn", "output"],
                    "description": "What to do: status, list, cancel, spawn, or output"
                },
                "task_id": {
                    "type": "string",
                    "description": "Required for 'status', 'cancel', 'output'. The task ID from the async receipt (e.g., 'shell:abc-123')"
                },
                "tool": {
                    "type": "string",
                    "description": "Required for 'spawn'. The tool name to invoke (e.g., 'shell', 'fs_write')"
                },
                "params": {
                    "type": "object",
                    "description": "Required for 'spawn'. Parameters to pass to the tool (forwarded verbatim)"
                },
                "status_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': pending, running, completed, failed, cancelled, timed_out"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': shell, agent_spawn, a2a_send, etc."
                },
                "blocking": {
                    "type": "boolean",
                    "description": "Optional for 'output'. If true, wait for the task to reach a terminal state before returning.",
                    "default": false
                },
                "tail_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional for 'output'. If >0, return only the last N lines of output.",
                    "default": 0
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let action: TaskAction = serde_json::from_value(
            params
                .get("action")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?,
        )
        .map_err(|e| anyhow::anyhow!("Invalid action: {e}"))?;

        match action {
            TaskAction::Status => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'status' action requires 'task_id'"))?;

                match self.lookup_task(task_id).await {
                    Some(task) => Ok(Self::build_status_response(&task)),
                    None => Ok(json!({
                        "error": "Task not found",
                        "task_id": task_id
                    })),
                }
            }
            TaskAction::List => {
                let status_filter = params.get("status_filter").and_then(|v| v.as_str());
                let tool_filter = params.get("tool_filter").and_then(|v| v.as_str());
                let tasks = self.list_tasks(status_filter, tool_filter).await;
                Ok(Self::build_list_response(tasks))
            }
            TaskAction::Cancel => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'cancel' action requires 'task_id'"))?;
                let result = self.cancel_task(task_id).await;
                Ok(Self::build_cancel_response(result, task_id))
            }
            TaskAction::Spawn => {
                let executor = match self.executor.as_ref() {
                    Some(e) => e,
                    None => {
                        return Ok(json!({
                            "error": "spawn action requires TaskTool to be constructed with an AsyncExecutor"
                        }));
                    }
                };
                let core_weak = match self.extension_core.as_ref() {
                    Some(w) => w,
                    None => {
                        return Ok(json!({
                            "error": "spawn action requires TaskTool to be constructed with an ExtensionCore"
                        }));
                    }
                };
                let core = match core_weak.upgrade() {
                    Some(c) => c,
                    None => {
                        return Ok(json!({
                            "error": "ExtensionCore has been dropped; cannot spawn"
                        }));
                    }
                };

                let tool_name = params
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'spawn' action requires 'tool'"))?;
                let tool_params = params
                    .get("params")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("'spawn' action requires 'params'"))?;

                let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());
                let session_key = core
                    .current_session_key()
                    .unwrap_or_else(|| "unknown".to_string());

                let config = crate::extensions::framework::async_exec::executor::AsyncToolConfig {
                    // `None` means no timeout: the spawned task runs to
                    // completion or until cancelled via `task cancel`.
                    // The 5-min cap is applied by the router on the
                    // *spawning* call, not on the spawned task's lifetime.
                    timeout_secs: None,
                    ..Default::default()
                };

                // Resolve the tool from the ExtensionCore.
                let tool = match core.get_tool(tool_name).await {
                    Some(t) => t,
                    None => {
                        return Ok(json!({
                            "error": format!("tool '{tool_name}' not found"),
                            "tool_name": tool_name,
                        }));
                    }
                };

                // Clone `tool_params` so we can move one copy into the
                // executor closure (which must be 'static) while still
                // being able to reference the original afterwards.
                let tool_params_for_closure = tool_params.clone();
                let receipt = executor
                    .execute(
                        task_id.clone(),
                        tool_name,
                        tool_params,
                        session_key,
                        config,
                        move || async move { tool.execute(tool_params_for_closure).await },
                    )
                    .await?;

                Ok(json!({
                    "task_id": receipt.task_id,
                    "status": "running",
                    "tool_name": tool_name,
                }))
            }
            TaskAction::Output => {
                let executor = match self.executor.as_ref() {
                    Some(e) => e,
                    None => {
                        return Ok(json!({
                            "error": "output action requires TaskTool to be constructed with an AsyncExecutor"
                        }));
                    }
                };
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'output' action requires 'task_id'"))?;
                let blocking = params
                    .get("blocking")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let tail_lines = params
                    .get("tail_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                // Look up the task
                let task = match self.lookup_task(task_id).await {
                    Some(t) => t,
                    None => {
                        return Ok(json!({
                            "error": "Task not found",
                            "task_id": task_id,
                        }));
                    }
                };

                if !task.is_terminal() {
                    if !blocking {
                        return Ok(json!({
                            "task_id": task_id,
                            "status": task.status.as_str(),
                            "is_terminal": false,
                            "result": null,
                        }));
                    }
                    // blocking=true: wait for completion via executor.
                    let timeout = std::time::Duration::from_secs(300);
                    let _ = executor
                        .wait_for_completion(&task_id.to_string(), timeout)
                        .await;
                    // Re-read after waiting.
                    let task = match self.lookup_task(task_id).await {
                        Some(t) => t,
                        None => {
                            return Ok(json!({
                                "error": "Task not found",
                                "task_id": task_id,
                            }));
                        }
                    };
                    return Ok(Self::build_output_response(&task, tail_lines));
                }

                Ok(Self::build_output_response(&task, tail_lines))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::async_exec::executor::{
        AsyncTaskEntry, AsyncTaskStatus, AsyncToolConfig,
    };

    #[tokio::test]
    async fn test_task_status_not_found() {
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "status", "task_id": "nonexistent:task"}))
            .await
            .unwrap();
        assert_eq!(result["error"], "Task not found");
        assert_eq!(result["task_id"], "nonexistent:task");
    }

    #[tokio::test]
    async fn test_task_list_empty() {
        // Use a fresh isolated registry instead of `TaskTool::global()`,
        // which would walk the global `static OnceLock` registry map that
        // subagent_integration_tests populates and leaves behind. The test
        // is asserting the empty-state behavior of the list action, so the
        // fixture must start at zero — a leaked sibling registry is not
        // a contract violation we want this test to catch.
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let tool = TaskTool::with_registry(registry);
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["active"], 0);
    }

    #[tokio::test]
    async fn test_task_status_with_registry() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let entry = AsyncTaskEntry::new(
                "shell:test-123".to_string(),
                "shell".to_string(),
                json!({"command": "echo hello"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry);
        }

        let tool = TaskTool::with_registry(registry);
        let result = tool
            .execute(json!({"action": "status", "task_id": "shell:test-123"}))
            .await
            .unwrap();

        assert_eq!(result["task_id"], "shell:test-123");
        assert_eq!(result["tool_name"], "shell");
        assert_eq!(result["status"], "pending");
        assert_eq!(result["metadata_type"], "none");
    }

    #[tokio::test]
    async fn test_task_list_with_registry_filters() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let mut entry1 = AsyncTaskEntry::new(
                "shell:test-1".to_string(),
                "shell".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry1.status = AsyncTaskStatus::Completed {
                result: crate::tools::ToolResult::success(json!({"done": true})),
            };
            reg.register(entry1);

            let entry2 = AsyncTaskEntry::new(
                "agent_spawn:test-2".to_string(),
                "agent_spawn".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry2);
        }

        let tool = TaskTool::with_registry(registry);

        // No filter
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["active"], 1);

        // Filter by tool
        let result = tool
            .execute(json!({"action": "list", "tool_filter": "shell"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["tool_name"], "shell");

        // Filter by status
        let result = tool
            .execute(json!({"action": "list", "status_filter": "completed"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["status"], "completed");
    }

    #[tokio::test]
    async fn test_task_cancel_success() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let entry = AsyncTaskEntry::new(
                "shell:cancel-me".to_string(),
                "shell".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry);
        }

        let tool = TaskTool::with_registry(registry);
        let result = tool
            .execute(json!({"action": "cancel", "task_id": "shell:cancel-me"}))
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert_eq!(result["task_id"], "shell:cancel-me");
        assert_eq!(result["previous_status"], "pending");
    }

    #[tokio::test]
    async fn test_task_cancel_already_terminal() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "shell:done".to_string(),
                "shell".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.status = AsyncTaskStatus::Completed {
                result: crate::tools::ToolResult::success(json!({})),
            };
            reg.register(entry);
        }

        let tool = TaskTool::with_registry(registry);
        let result = tool
            .execute(json!({"action": "cancel", "task_id": "shell:done"}))
            .await
            .unwrap();

        assert_eq!(result["success"], false);
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("already terminal"));
    }

    #[tokio::test]
    async fn test_task_cancel_not_found() {
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "cancel", "task_id": "shell:missing"}))
            .await
            .unwrap();

        assert_eq!(result["success"], false);
        assert_eq!(result["message"], "Task not found");
    }

    #[tokio::test]
    async fn test_task_spawn_missing_tool_returns_error() {
        // TaskTool without executor: spawn should error cleanly.
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "spawn", "tool": "definitely_not_a_tool", "params": {}}))
            .await
            .unwrap();
        // Without an executor wired, spawn is unsupported.
        assert_eq!(result["error"], "spawn action requires TaskTool to be constructed with an AsyncExecutor");
    }

    #[tokio::test]
    async fn test_task_output_missing_executor_returns_error() {
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "output", "task_id": "shell:x"}))
            .await
            .unwrap();
        assert_eq!(result["error"], "output action requires TaskTool to be constructed with an AsyncExecutor");
    }

    /// Build a TaskTool wired with a fresh isolated registry AND a
    /// fresh AsyncExecutor. The Output arm requires both (the executor
    /// is checked first and short-circuits with an error otherwise).
    fn make_tool_with_registry_and_executor(
        registry: SharedAsyncTaskRegistry,
    ) -> (TaskTool, Arc<crate::extensions::framework::async_exec::executor::AsyncExecutor>) {
        let executor = Arc::new(
            crate::extensions::framework::async_exec::executor::AsyncExecutor::new(),
        );
        let tool = TaskTool {
            registry: Some(registry),
            executor: Some(executor.clone()),
            extension_core: None,
        };
        (tool, executor)
    }

    #[tokio::test]
    async fn test_task_output_tail_lines_string_result() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let result_value = json!("line1\nline2\nline3\nline4\nline5");
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "shell:string-result".to_string(),
                "shell".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: crate::tools::ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry_and_executor(registry);
        let result = tool
            .execute(json!({
                "action": "output",
                "task_id": "shell:string-result",
                "tail_lines": 2
            }))
            .await
            .unwrap();

        assert_eq!(result["result"], "line4\nline5");
    }

    #[tokio::test]
    async fn test_task_output_tail_lines_object_with_stdout() {
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
                "shell:obj-result".to_string(),
                "shell".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: crate::tools::ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry_and_executor(registry);
        let result = tool
            .execute(json!({
                "action": "output",
                "task_id": "shell:obj-result",
                "tail_lines": 2
            }))
            .await
            .unwrap();

        assert_eq!(result["result"]["stdout"], "line2\nline3");
        assert_eq!(result["result"]["exit_code"], 0);
    }

    #[tokio::test]
    async fn test_task_output_tail_lines_unknown_shape_passthrough() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let result_value = json!({"count": 42});
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "shell:unknown-shape".to_string(),
                "shell".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: crate::tools::ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry_and_executor(registry);
        let result = tool
            .execute(json!({
                "action": "output",
                "task_id": "shell:unknown-shape",
                "tail_lines": 10
            }))
            .await
            .unwrap();

        // Graceful degradation: unrecognized shape passes through
        // unchanged even though tail_lines > 0.
        assert_eq!(result["result"], json!({"count": 42}));
    }

    #[tokio::test]
    async fn test_task_output_tail_lines_zero_passthrough() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extensions::framework::async_exec::executor::AsyncTaskRegistry::new(),
        ));
        let result_value = json!("line1\nline2\nline3\nline4\nline5");
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "shell:zero".to_string(),
                "shell".to_string(),
                json!({"command": "echo"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.set_result(result_value);
            entry.status = AsyncTaskStatus::Completed {
                result: crate::tools::ToolResult::success(json!({})),
            };
            entry.completed_at = Some(chrono::Utc::now());
            reg.register(entry);
        }

        let (tool, _exec) = make_tool_with_registry_and_executor(registry);
        let result = tool
            .execute(json!({
                "action": "output",
                "task_id": "shell:zero"
                // tail_lines omitted → defaults to 0
            }))
            .await
            .unwrap();

        // tail_lines=0 returns the full string unchanged.
        assert_eq!(result["result"], "line1\nline2\nline3\nline4\nline5");
    }
}
