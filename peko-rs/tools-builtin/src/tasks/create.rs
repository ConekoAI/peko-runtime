//! TaskCreate tool — create a planning todo.

use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tasks::{missing_session_error, require_session_id, SharedTodoRuntime};

/// Create a planning todo in the current session.
pub struct TaskCreateTool {
    runtime: SharedTodoRuntime,
}

impl TaskCreateTool {
    /// Create a tool bound to the given todo runtime.
    #[must_use]
    pub fn new(runtime: SharedTodoRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &'static str {
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

    /// F33: task-list mutation — opt out of parallel dispatch. Two
    /// concurrent `TaskCreate` calls in the same batch can race on
    /// task-id assignment if the list is in-memory; mixed
    /// `TaskCreate + TaskUpdate` on the same id races on the list
    /// mutation.
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Production callers always go through `execute_with_context` via
        // `ExtensionCore::invoke_hook`; this branch exists only to satisfy
        // the `Tool` trait's default `execute` method. Returning a regular
        // `anyhow::Error` (instead of a structured JSON blob) keeps the
        // error path consistent with other tools and lets the harness
        // surface it as a proper failure.
        Err(missing_session_error())
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
            .runtime
            .create_todo(&session_id, subject.to_string(), description, active_form)
            .await?;

        Ok(serde_json::to_value(todo)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TestTodoRuntime;
    use peko_tools_core::ToolContext;
    use serde_json::json;

    #[tokio::test]
    async fn test_task_create_basic() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskCreateTool::new(runtime);

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
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskCreateTool::new(runtime);

        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate")
            .with_session_id("agent:test:cli:default");
        let result = tool.execute_with_context(json!({}), &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_task_create_requires_session() {
        let runtime = std::sync::Arc::new(TestTodoRuntime::new());
        let tool = TaskCreateTool::new(runtime);

        let ctx = ToolContext::for_hook_run("run", "tc", "TaskCreate");
        let result = tool
            .execute_with_context(json!({"subject": "X"}), &ctx)
            .await;
        assert!(result.is_err());
    }
}
