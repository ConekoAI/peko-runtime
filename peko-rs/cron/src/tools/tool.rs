//! Cron domain tool. Action handlers remain private to this domain.

use super::{
    CronCreateAction, CronDeleteAction, CronHistoryAction, CronListAction, CronRuntime,
    CronTriggerAction, CronUpdateAction, RuntimeBinding,
};
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::Value;
use std::sync::Arc;

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
    /// Dispatch to the daemon-installed runtime, resolved per call so the
    /// catalog can be installed before daemon startup completes.
    pub fn new() -> Self {
        Self::with_binding(RuntimeBinding::default())
    }

    /// Dispatch to `runtime` instead of the daemon-installed slot.
    pub fn with_runtime(runtime: Arc<dyn CronRuntime>) -> Self {
        Self::with_binding(RuntimeBinding(Some(runtime)))
    }

    fn with_binding(runtime: RuntimeBinding) -> Self {
        Self {
            create: CronCreateAction::bound(runtime.clone()),
            list: CronListAction::bound(runtime.clone()),
            delete: CronDeleteAction::bound(runtime.clone()),
            update: CronUpdateAction::bound(runtime.clone()),
            trigger: CronTriggerAction::bound(runtime.clone()),
            history: CronHistoryAction::bound(runtime),
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
    /// F33: cron DB write — opt out of parallel dispatch. Concurrent
    /// creates with the same job name race on the uniqueness check;
    /// interleaving a create with a delete by id can land half-applied.
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
