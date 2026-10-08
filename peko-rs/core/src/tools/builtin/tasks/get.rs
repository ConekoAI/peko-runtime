//! Task action get tool — fetch a planning todo by id.

use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::tasks::{missing_session_error, require_session_id, SharedTodoRuntime};

/// Read a planning todo from the current session.
pub struct TaskGetAction {
    runtime: SharedTodoRuntime,
}

impl TaskGetAction {
    /// Create a tool bound to the given todo runtime.
    #[must_use]
    pub fn new(runtime: SharedTodoRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for TaskGetAction {
    fn name(&self) -> &'static str {
        "Task"
    }

    fn description(&self) -> String {
        r"Get a planning todo by its taskId.

Parameters:
- taskId: string (required) — the todo id returned by Task action create

Returns the todo, or an error if it does not exist."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "taskId": {
                    "type": "string",
                    "description": "The todo id returned by Task action create (e.g., 'todo:abc123')."
                }
            },
            "required": ["taskId"]
        })
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
            .ok_or_else(|| anyhow::anyhow!("Task action get requires 'taskId'"))?;

        match self.runtime.get_todo(&session_id, task_id).await? {
            Some(todo) => Ok(serde_json::to_value(todo)?),
            None => Ok(json!({"error": "Todo not found", "taskId": task_id})),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::tasks::{TaskCreateAction, TestTodoRuntime};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    #[tokio::test]
    async fn test_task_get_found() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let create = TaskCreateAction::new(runtime.clone());
        let ctx = ToolContext::for_hook_run("run", "tc", "Task")
            .with_session_id("agent:test:cli:default");
        let created = create
            .execute_with_context(json!({"subject": "S"}), &ctx)
            .await
            .unwrap();

        let tool = TaskGetAction::new(runtime);
        let result = tool
            .execute_with_context(json!({"taskId": created["taskId"]}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["subject"], "S");
    }

    #[tokio::test]
    async fn test_task_get_missing() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskGetAction::new(runtime);
        let ctx = ToolContext::for_hook_run("run", "tc", "Task")
            .with_session_id("agent:test:cli:default");
        let result = tool
            .execute_with_context(json!({"taskId": "todo:nope"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["error"], "Todo not found");
    }

    #[tokio::test]
    async fn test_task_get_no_session() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskGetAction::new(runtime);
        let ctx = ToolContext::for_hook_run("run", "tc", "Task");
        let result = tool
            .execute_with_context(json!({"taskId": "todo:nope"}), &ctx)
            .await;
        assert!(result.is_err());
    }
}
