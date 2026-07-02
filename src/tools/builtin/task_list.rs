//! TaskList tool — list planning todos for the current session.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::session::todos::TodoStorage;
use crate::tools::builtin::task_common::{missing_session_error, require_session_id};
use crate::tools::core::{Tool, ToolContext};

/// List planning todos for the current session.
pub struct TaskListTool {
    storage: Arc<TodoStorage>,
}

impl TaskListTool {
    /// Create a tool bound to the given todo storage.
    #[must_use]
    pub fn new(storage: Arc<TodoStorage>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &'static str {
        "TaskList"
    }

    fn description(&self) -> String {
        r"List planning todos for the current session.

Parameters:
- status_filter: string? — if provided, only return todos with this status
  ('pending', 'in_progress', or 'completed')

Returns an array of todo objects."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "Optional status filter."
                }
            },
            "required": []
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

        let status_filter = match params.get("status_filter") {
            Some(v) => Some(crate::tools::builtin::task_common::parse_status_param(v)?),
            None => None,
        };

        let todos = self.storage.list_todos(&session_id, status_filter).await?;
        Ok(serde_json::to_value(todos)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::todos::TodoStatus;
    use crate::tools::builtin::task_create::TaskCreateTool;
    use crate::tools::core::ToolContext;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_task_list_filtered() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let create = TaskCreateTool::new(storage.clone());
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");

        let _a = create
            .execute_with_context(json!({"subject": "A"}), &ctx)
            .await
            .unwrap();
        let b = create
            .execute_with_context(json!({"subject": "B"}), &ctx)
            .await
            .unwrap();
        storage
            .update_todo(
                ctx.session_id.as_ref().unwrap(),
                b["taskId"].as_str().unwrap(),
                Some(TodoStatus::InProgress),
                None,
            )
            .await
            .unwrap();

        let tool = TaskListTool::new(storage);
        let all = tool.execute_with_context(json!({}), &ctx).await.unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);

        let pending = tool
            .execute_with_context(json!({"status_filter": "pending"}), &ctx)
            .await
            .unwrap();
        assert_eq!(pending.as_array().unwrap().len(), 1);
        assert_eq!(pending[0]["subject"], "A");
    }

    #[tokio::test]
    async fn test_task_list_empty() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let tool = TaskListTool::new(storage);
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskList")
            .with_session_id("agent:test:cli:empty");

        let result = tool.execute_with_context(json!({}), &ctx).await.unwrap();
        assert!(result.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_task_list_no_session() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let tool = TaskListTool::new(storage);
        let ctx = ToolContext::for_hook_run("run", "tc", "TaskList");
        let result = tool.execute_with_context(json!({}), &ctx).await;
        assert!(result.is_err());
    }
}
