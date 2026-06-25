//! TaskCreate tool — create a planning todo.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::session::todos::TodoStorage;
use crate::tools::builtin::task_common::{param_error, require_session_id};
use crate::tools::core::{Tool, ToolContext};

/// Create a planning todo in the current session.
pub struct TaskCreateTool {
    storage: Arc<TodoStorage>,
}

impl TaskCreateTool {
    /// Create a tool bound to the given todo storage.
    #[must_use]
    pub fn new(storage: Arc<TodoStorage>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "TaskCreate"
    }

    fn description(&self) -> String {
        r"Create a planning todo for the current session.

Use when: the user asks to track work, create a checklist, or add a task.
Parameters:
- subject: string (required) — short imperative title
- description: string? — longer details
- activeForm: string? — present-continuous form shown in spinners

Returns the created todo including its taskId."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "subject": {
                    "type": "string",
                    "description": "Short imperative title for the todo."
                },
                "description": {
                    "type": "string",
                    "description": "Optional longer description of the todo."
                },
                "activeForm": {
                    "type": "string",
                    "description": "Optional present-continuous form shown in UI spinners."
                }
            },
            "required": ["subject"]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Ok(param_error(
            "TaskCreate requires a session context; use execute_with_context",
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let session_id = require_session_id(ctx)?;

        let subject = params
            .get("subject")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("TaskCreate requires 'subject'"))?;
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from);
        let active_form = params
            .get("activeForm")
            .and_then(|v| v.as_str())
            .map(String::from);

        let todo = self
            .storage
            .create_todo(&session_id, subject.to_string(), description, active_form)
            .await?;

        Ok(serde_json::to_value(todo)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::ToolContext;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_task_create_basic() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let tool = TaskCreateTool::new(storage);

        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");
        let result = tool
            .execute_with_context(
                json!({
                    "subject": "Write tests",
                    "description": "Add unit tests for TaskCreate"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["subject"], "Write tests");
        assert_eq!(result["status"], "pending");
        assert!(result["taskId"].as_str().unwrap().starts_with("todo:"));
    }

    #[tokio::test]
    async fn test_task_create_requires_subject() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let tool = TaskCreateTool::new(storage);

        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");
        let result = tool.execute_with_context(json!({}), &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_task_create_requires_session() {
        let temp = TempDir::new().unwrap();
        let storage = Arc::new(TodoStorage::new(temp.path().to_path_buf()));
        let tool = TaskCreateTool::new(storage);

        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate");
        let result = tool
            .execute_with_context(json!({"subject": "X"}), &ctx)
            .await;
        assert!(result.is_err());
    }
}
