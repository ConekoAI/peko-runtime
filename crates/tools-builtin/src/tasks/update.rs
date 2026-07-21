//! TaskUpdate tool — update a planning todo's status or owner.

use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tasks::{
    missing_session_error, parse_status_param, require_session_id, SharedTodoRuntime,
};

/// Update a planning todo in the current session.
pub struct TaskUpdateTool {
    runtime: SharedTodoRuntime,
}

impl TaskUpdateTool {
    /// Create a tool bound to the given todo runtime.
    #[must_use]
    pub fn new(runtime: SharedTodoRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &'static str {
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
            "required": ["taskId"],
            "anyOf": [
                { "required": ["status"] },
                { "required": ["owner"] }
            ]
        })
    }

    /// F33: task-list mutation — opt out of parallel dispatch. See
    /// `TaskCreate::parallelizable` for the rationale.
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(missing_session_error())
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
            Some(v) => Some(parse_status_param(v)?),
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
            .runtime
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
    use crate::tasks::{TaskCreateTool, TestTodoRuntime};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    #[tokio::test]
    async fn test_task_update_status() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let create = TaskCreateTool::new(runtime.clone());
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");
        let created = create
            .execute_with_context(json!({"subject": "S"}), &ctx)
            .await
            .unwrap();

        let tool = TaskUpdateTool::new(runtime);
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
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskUpdateTool::new(runtime);
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskUpdate")
            .with_session_id("agent:test:cli:default");
        let result = tool
            .execute_with_context(json!({"taskId": "todo:nope"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["error"], "TaskUpdate requires 'status' or 'owner'");
    }

    #[tokio::test]
    async fn test_task_update_owner_only() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let create = TaskCreateTool::new(runtime.clone());
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:owner");
        let created = create
            .execute_with_context(json!({"subject": "S"}), &ctx)
            .await
            .unwrap();

        let tool = TaskUpdateTool::new(runtime);
        let result = tool
            .execute_with_context(
                json!({"taskId": created["taskId"], "owner": "claude"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["owner"], "claude");
        assert_eq!(result["status"], "pending");
    }

    #[tokio::test]
    async fn test_task_update_not_found() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskUpdateTool::new(runtime);
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskUpdate")
            .with_session_id("agent:test:cli:nf");
        let result = tool
            .execute_with_context(json!({"taskId": "todo:nope", "status": "completed"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["error"], "Todo not found");
    }

    #[tokio::test]
    async fn test_task_update_invalid_status() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskUpdateTool::new(runtime);
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskUpdate")
            .with_session_id("agent:test:cli:iv");
        let result = tool
            .execute_with_context(json!({"taskId": "todo:nope", "status": "done"}), &ctx)
            .await;
        assert!(result.is_err());
    }
}
