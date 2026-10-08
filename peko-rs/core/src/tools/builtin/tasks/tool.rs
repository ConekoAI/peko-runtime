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
