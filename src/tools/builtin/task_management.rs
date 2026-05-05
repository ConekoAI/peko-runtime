//! Universal Task Management Tool
//!
//! Provides `task` — a single tool for managing ANY async task.
//! Replaces `task_status` and `task_list` (Issue 011) with one unified interface.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::extension::async_exec::executor::{
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
}

// ------------------------------------------------------------------------------
// TaskTool — unified interface
// ------------------------------------------------------------------------------

/// Unified task management tool.
pub struct TaskTool {
    registry: Option<SharedAsyncTaskRegistry>,
}

impl TaskTool {
    #[must_use]
    pub fn with_registry(registry: SharedAsyncTaskRegistry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    #[must_use]
    pub fn global() -> Self {
        Self { registry: None }
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
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> String {
        r"Manage async tasks: check status, list tasks, or cancel a running task.

Works for ALL async tasks: shell, grep, agent_spawn, a2a_send, etc.

Parameters:
- action: 'status', 'list', or 'cancel' (required)
- task_id: Required for 'status' and 'cancel' — the task ID from the async receipt
- status_filter: Optional for 'list' — filter by status
- tool_filter: Optional for 'list' — filter by tool name

Returns structured data appropriate to the action."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "cancel"],
                    "description": "What to do: status (get one task), list (query tasks), cancel (stop a task)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Required for 'status' and 'cancel'. The task ID from the async receipt (e.g., 'shell:abc-123')"
                },
                "status_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': pending, running, completed, failed, cancelled, timed_out"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': shell, agent_spawn, a2a_send, etc."
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::async_exec::executor::{AsyncTaskEntry, AsyncTaskStatus, AsyncToolConfig};

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
        let tool = TaskTool::global();
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["active"], 0);
    }

    #[tokio::test]
    async fn test_task_status_with_registry() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::extension::async_exec::executor::AsyncTaskRegistry::new()
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
            crate::extension::async_exec::executor::AsyncTaskRegistry::new()
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
                result: crate::tools::core::traits::ToolResult::success(json!({"done": true})),
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
            crate::extension::async_exec::executor::AsyncTaskRegistry::new()
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
            crate::extension::async_exec::executor::AsyncTaskRegistry::new()
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
                result: crate::tools::core::traits::ToolResult::success(json!({})),
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
}
