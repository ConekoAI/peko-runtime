//! TaskUpdate tool — update a planning todo's status or owner.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::session::todos::TodoStorage;
use crate::tools::builtin::task_common::{param_error, require_session_id};
use crate::tools::core::{Tool, ToolContext};

/// Update a planning todo in the current session.
pub struct TaskUpdateTool {
    storage: Arc<TodoStorage>,
}

impl TaskUpdateTool {
    /// Create a tool bound to the given todo storage.
    #[must_use]
    pub fn new(storage: Arc<TodoStorage>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "TaskUpdate"
    }

    fn description(&self) -> String {
        r"Update the status or owner of a planning todo.

Parameters:
- taskId: string (required) — the todo id returned by TaskCreate
- status: string? — new status ('pending', 'in_progress', or 'completed')
- owner: string? — new owner/agent name

Returns the updated todo, or an error if it does not exist."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "taskId": {
                    "type": "string",
                    "description": "The todo id returned by TaskCreate (e.g., 'todo:abc123')."
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "Optional new status."
                },
                "owner": {
                    "type": "string",
                    "description": "Optional new owner/agent name."
                }
            },
            "required": ["taskId"]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Ok(param_error(
            "TaskUpdate requires a session context; use execute_with_context",
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let session_id = require_session_id(ctx)?;

        let task_id = params
            .get("taskId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("TaskUpdate requires 'taskId'"))?;

        let status = match params.get("status") {
            Some(v) => Some(crate::tools::builtin::task_common::parse_status_param(v)?),
            None => None,
        };
        let owner = params
            .get("owner")
            .and_then(|v| v.as_str())
            .map(String::from);

        if status.is_none() && owner.is_none() {
            return Ok(json!({
                "error": "TaskUpdate requires 'status' or 'owner'"
            }));
        }

        match self
            .storage
            .update_todo(&session_id, task_id, status, owner)
            .await?
        {
            Some(todo) => Ok(serde_json::to_value(todo)?),
            None => Ok(json!({"error": "Todo not found", "taskId": task_id})),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::task_create::TaskCreateTool;
    use crate::tools::core::ToolContext;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_task_update_status() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let create = TaskCreateTool::new(storage.clone());
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");
        let created = create
            .execute_with_context(json!({"subject": "S"}), &ctx)
            .await
            .unwrap();

        let tool = TaskUpdateTool::new(storage);
        let result = tool
            .execute_with_context(
                json!({
                    "taskId": created["taskId"],
                    "status": "in_progress",
                    "owner": "claude"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["status"], "in_progress");
        assert_eq!(result["owner"], "claude");
    }

    #[tokio::test]
    async fn test_task_update_missing_requires_mutation() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let tool = TaskUpdateTool::new(storage);
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskUpdate")
            .with_session_id("agent:test:cli:default");
        let result = tool
            .execute_with_context(json!({"taskId": "todo:nope"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["error"], "TaskUpdate requires 'status' or 'owner'");
    }
}
