//! TaskGet tool — fetch a planning todo by id.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::session::todos::TodoStorage;
use crate::tools::builtin::task_common::{param_error, require_session_id};
use crate::tools::core::{Tool, ToolContext};

/// Read a planning todo from the current session.
pub struct TaskGetTool {
    storage: Arc<TodoStorage>,
}

impl TaskGetTool {
    /// Create a tool bound to the given todo storage.
    #[must_use]
    pub fn new(storage: Arc<TodoStorage>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str {
        "TaskGet"
    }

    fn description(&self) -> String {
        r"Get a planning todo by its taskId.

Parameters:
- taskId: string (required) — the todo id returned by TaskCreate

Returns the todo, or an error if it does not exist."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "taskId": {
                    "type": "string",
                    "description": "The todo id returned by TaskCreate (e.g., 'todo:abc123')."
                }
            },
            "required": ["taskId"]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Ok(param_error(
            "TaskGet requires a session context; use execute_with_context",
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
            .ok_or_else(|| anyhow::anyhow!("TaskGet requires 'taskId'"))?;

        match self.storage.get_todo(&session_id, task_id).await? {
            Some(todo) => Ok(serde_json::to_value(todo)?),
            None => Ok(json!({"error": "Todo not found", "taskId": task_id})),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::ToolContext;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_task_get_found() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let create_tool = crate::tools::builtin::task_create::TaskCreateTool::new(storage.clone());
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");
        let created = create_tool
            .execute_with_context(json!({"subject": "S"}), &ctx)
            .await
            .unwrap();

        let tool = TaskGetTool::new(storage);
        let result = tool
            .execute_with_context(json!({"taskId": created["taskId"]}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["subject"], "S");
    }

    #[tokio::test]
    async fn test_task_get_missing() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let tool = TaskGetTool::new(storage);
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskGet")
            .with_session_id("agent:test:cli:default");
        let result = tool
            .execute_with_context(json!({"taskId": "todo:nope"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["error"], "Todo not found");
    }
}
