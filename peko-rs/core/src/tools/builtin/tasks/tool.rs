//! Task domain tool. Action handlers remain private to this domain.

use super::{SharedTodoRuntime, TaskCreateAction, TaskGetAction, TaskListAction, TaskUpdateAction};
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::Value;

/// Manage the current session’s todos: create, get, list, or update. Todos track small tasks; use Plan for durable dependency graphs.
pub struct TaskTool {
    create: TaskCreateAction,
    get: TaskGetAction,
    list: TaskListAction,
    update: TaskUpdateAction,
}

impl TaskTool {
    /// Bind the domain tool to its existing runtime.
    pub fn new(runtime: SharedTodoRuntime) -> Self {
        Self {
            create: TaskCreateAction::new(runtime.clone()),
            get: TaskGetAction::new(runtime.clone()),
            list: TaskListAction::new(runtime.clone()),
            update: TaskUpdateAction::new(runtime),
        }
    }

    fn handler(&self, params: &Value) -> anyhow::Result<&dyn Tool> {
        match params.get("action").and_then(Value::as_str) {
            Some("create") => Ok(&self.create),
            Some("get") => Ok(&self.get),
            Some("list") => Ok(&self.list),
            Some("update") => Ok(&self.update),
            _ => anyhow::bail!("Task requires a supported action"),
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "Task"
    }
    fn description(&self) -> String {
        let mut description = "Manage the current session’s todos: create, get, list, or update. Todos track small tasks; use Plan for durable dependency graphs.".to_string();
        description.push_str(&format!(
            "\n\nAction create:\n{}",
            self.create.description()
        ));
        description.push_str(&format!("\n\nAction get:\n{}", self.get.description()));
        description.push_str(&format!("\n\nAction list:\n{}", self.list.description()));
        description.push_str(&format!(
            "\n\nAction update:\n{}",
            self.update.description()
        ));
        description
    }
    fn parameters(&self) -> Value {
        peko_tools_core::schema::action_schema(&[
            ("create", self.create.parameters()),
            ("get", self.get.parameters()),
            ("list", self.list.parameters()),
            ("update", self.update.parameters()),
        ])
    }
    fn parallelizable(&self) -> bool {
        false
    }
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        self.handler(&params)?.execute(params).await
    }
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        self.handler(&params)?
            .execute_with_context(params, ctx)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::tasks::TestTodoRuntime;
    use serde_json::json;

    /// One todo's life, every action routed through the domain tool.
    #[tokio::test]
    async fn every_action_dispatches_through_the_domain_tool() {
        let tool = TaskTool::new(std::sync::Arc::new(TestTodoRuntime::new()));
        let ctx = ToolContext::for_hook_run("run", "tc", "Task").with_session_id("sess-1");
        let call = |params: Value| {
            let (tool, ctx) = (&tool, &ctx);
            async move { tool.execute_with_context(params, ctx).await.unwrap() }
        };

        let created = call(json!({ "action": "create", "subject": "write tests" })).await;
        let id = created["taskId"].as_str().unwrap().to_string();
        let got = call(json!({ "action": "get", "taskId": id })).await;
        assert_eq!(got["subject"], "write tests");
        let updated =
            call(json!({ "action": "update", "taskId": id, "status": "completed" })).await;
        assert_eq!(updated["status"], "completed");
        let listed = call(json!({ "action": "list", "status_filter": "completed" })).await;
        assert!(listed.to_string().contains(&id), "{listed}");
    }

    #[tokio::test]
    async fn unsupported_actions_and_contextless_calls_are_rejected() {
        let tool = TaskTool::new(std::sync::Arc::new(TestTodoRuntime::new()));
        let ctx = ToolContext::for_hook_run("run", "tc", "Task").with_session_id("sess-1");
        for params in [json!({ "action": "purge" }), json!({})] {
            let error = tool.execute_with_context(params, &ctx).await.unwrap_err();
            assert!(error.to_string().contains("supported action"), "{error}");
        }
        for action in ["create", "get", "list", "update"] {
            assert!(
                tool.execute(json!({ "action": action })).await.is_err(),
                "{action} without a context must fail"
            );
        }
    }
}
