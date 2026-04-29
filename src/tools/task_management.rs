//! Universal Task Management Tools
//!
//! Provides `task_status` and `task_list` — generic tools for querying
//! ANY async task regardless of tool type. These replace the subagent-specific
//! `agent_spawn_status` and `agent_spawn_list` tools.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::tools::async_executor::{
    find_task_across_all_registries, list_all_tasks_across_all_registries,
    SharedAsyncTaskRegistry, TaskView,
};
use crate::tools::Tool;

/// Tool for checking the status of any async task by task_id.
pub struct TaskStatusTool {
    registry: Option<SharedAsyncTaskRegistry>,
}

impl TaskStatusTool {
    /// Create a status tool bound to a specific agent's registry.
    #[must_use]
    pub fn with_registry(registry: SharedAsyncTaskRegistry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    /// Create a global status tool that searches across all agent registries.
    #[must_use]
    pub fn global() -> Self {
        Self { registry: None }
    }

    /// Look up a task by ID, either in the bound registry or across all registries.
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
}

#[async_trait]
impl Tool for TaskStatusTool {
    fn name(&self) -> &'static str {
        "task_status"
    }

    fn description(&self) -> String {
        r"Check the status of any async task by its task_id.

Works for ALL async tasks: shell, grep, agent_spawn, a2a_send, etc.

Parameters:
- task_id: The task ID from the async receipt (required)

Returns the current status, result (if complete), timing, and tool name."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID from the async receipt (e.g., 'shell:abc-123', 'agent_spawn:xyz-789')"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'task_id' parameter"))?;

        match self.lookup_task(task_id).await {
            Some(task) => {
                let mut response = json!({
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
                    let ts: chrono::DateTime<chrono::Utc> = completed_at;
                    response["completed_at"] = json!(ts.to_rfc3339());
                }

                if let Some(ref result) = task.result {
                    response["result"] = result.clone();
                }

                if let Some(duration) = task.duration() {
                    let secs: i64 = duration.num_seconds();
                    response["duration_seconds"] = json!(secs);
                }

                Ok(response)
            }
            None => Ok(json!({
                "error": "Task not found",
                "task_id": task_id
            })),
        }
    }
}

/// Tool for listing async tasks.
pub struct TaskListTool {
    registry: Option<SharedAsyncTaskRegistry>,
}

impl TaskListTool {
    /// Create a list tool bound to a specific agent's registry.
    #[must_use]
    pub fn with_registry(registry: SharedAsyncTaskRegistry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    /// Create a global list tool that searches across all agent registries.
    #[must_use]
    pub fn global() -> Self {
        Self { registry: None }
    }

    /// List tasks, either from the bound registry or across all registries.
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
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &'static str {
        "task_list"
    }

    fn description(&self) -> String {
        r"List async tasks for the current session or across all sessions.

Parameters:
- status_filter: Filter by status — 'pending', 'running', 'completed', 'failed', 'cancelled', 'timed_out' (optional)
- tool_filter: Filter by tool name — 'shell', 'agent_spawn', etc. (optional)

Returns a list of tasks with their status, tool name, and timing."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "description": "Filter by status: pending, running, completed, failed, cancelled, timed_out"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Filter by tool name: shell, agent_spawn, a2a_send, etc."
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let status_filter = params.get("status_filter").and_then(|v| v.as_str());
        let tool_filter = params.get("tool_filter").and_then(|v| v.as_str());

        let tasks: Vec<TaskView> = self.list_tasks(status_filter, tool_filter).await;

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

        let active_count = task_jsons
            .iter()
            .filter(|t| {
                let s = t["status"].as_str().unwrap_or("");
                s != "completed" && s != "failed" && s != "cancelled" && s != "timed_out"
            })
            .count();

        Ok(json!({
            "total": task_jsons.len(),
            "active": active_count,
            "tasks": task_jsons
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::async_executor::{AsyncTaskEntry, AsyncTaskStatus, AsyncToolConfig};

    #[tokio::test]
    async fn test_task_status_tool_global_not_found() {
        let tool = TaskStatusTool::global();
        let result = tool
            .execute(json!({"task_id": "nonexistent:task"}))
            .await
            .unwrap();
        assert_eq!(result["error"], "Task not found");
        assert_eq!(result["task_id"], "nonexistent:task");
    }

    #[tokio::test]
    async fn test_task_list_tool_global_empty() {
        let tool = TaskListTool::global();
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["active"], 0);
    }

    #[tokio::test]
    async fn test_task_status_tool_with_registry() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::tools::async_executor::AsyncTaskRegistry::new(),
        ));

        // Insert a generic (non-subagent) task
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

        let tool = TaskStatusTool::with_registry(registry);
        let result = tool
            .execute(json!({"task_id": "shell:test-123"}))
            .await
            .unwrap();

        assert_eq!(result["task_id"], "shell:test-123");
        assert_eq!(result["tool_name"], "shell");
        assert_eq!(result["status"], "pending");
        assert_eq!(result["metadata_type"], "none");
    }

    #[tokio::test]
    async fn test_task_list_tool_with_registry_filters() {
        let registry = Arc::new(tokio::sync::RwLock::new(
            crate::tools::async_executor::AsyncTaskRegistry::new(),
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
                result: crate::tools::traits::ToolResult::success(json!({"done": true})),
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

        let tool = TaskListTool::with_registry(registry);

        // No filter
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["active"], 1);

        // Filter by tool
        let result = tool.execute(json!({"tool_filter": "shell"})).await.unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["tool_name"], "shell");

        // Filter by status
        let result = tool
            .execute(json!({"status_filter": "completed"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["status"], "completed");
    }
}
