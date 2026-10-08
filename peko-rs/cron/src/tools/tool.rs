//! Cron domain tool. Action handlers remain private to this domain.

use super::{
    CronCreateAction, CronDeleteAction, CronHistoryAction, CronListAction, CronTriggerAction,
    CronUpdateAction,
};
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::Value;

/// Manage principal-owned scheduled jobs: create, list, delete, update, trigger, or history. Create schedules a message or a tool invocation; identify existing jobs by id or label.
pub struct CronTool {
    create: CronCreateAction,
    list: CronListAction,
    delete: CronDeleteAction,
    update: CronUpdateAction,
    trigger: CronTriggerAction,
    history: CronHistoryAction,
}

impl CronTool {
    /// Bind the domain tool to its existing runtime.
    pub fn new() -> Self {
        Self {
            create: CronCreateAction::new(),
            list: CronListAction::new(),
            delete: CronDeleteAction::new(),
            update: CronUpdateAction::new(),
            trigger: CronTriggerAction::new(),
            history: CronHistoryAction::new(),
        }
    }

    fn handler(&self, params: &Value) -> anyhow::Result<&dyn Tool> {
        match params.get("action").and_then(Value::as_str) {
            Some("create") => Ok(&self.create),
            Some("list") => Ok(&self.list),
            Some("delete") => Ok(&self.delete),
            Some("update") => Ok(&self.update),
            Some("trigger") => Ok(&self.trigger),
            Some("history") => Ok(&self.history),
            _ => anyhow::bail!("Cron requires a supported action"),
        }
    }
}

impl Default for CronTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str {
        "Cron"
    }
    fn description(&self) -> String {
        let mut description = "Manage principal-owned scheduled jobs: create, list, delete, update, trigger, or history. Create schedules a message or a tool invocation; identify existing jobs by id or label.".to_string();
        description.push_str(&format!(
            "\n\nAction create:\n{}",
            self.create.description()
        ));
        description.push_str(&format!("\n\nAction list:\n{}", self.list.description()));
        description.push_str(&format!(
            "\n\nAction delete:\n{}",
            self.delete.description()
        ));
        description.push_str(&format!(
            "\n\nAction update:\n{}",
            self.update.description()
        ));
        description.push_str(&format!(
            "\n\nAction trigger:\n{}",
            self.trigger.description()
        ));
        description.push_str(&format!(
            "\n\nAction history:\n{}",
            self.history.description()
        ));
        description
    }
    fn parameters(&self) -> Value {
        peko_tools_core::schema::action_schema(&[
            ("create", self.create.parameters()),
            ("list", self.list.parameters()),
            ("delete", self.delete.parameters()),
            ("update", self.update.parameters()),
            ("trigger", self.trigger.parameters()),
            ("history", self.history.parameters()),
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
