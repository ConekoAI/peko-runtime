//! Async domain tool. Action handlers remain private to this domain.

use super::{
    AsyncListAction, AsyncOutputAction, AsyncSpawnAction, AsyncStatusAction, AsyncStopAction,
    SharedAsyncRuntime,
};
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::Value;

/// Run tools in the background and manage their receipts: spawn, output, status, list, or stop. Spawn returns task_id; use it for subsequent actions. Completion wakes the spawning session by default.
pub struct AsyncTool {
    spawn: AsyncSpawnAction,
    output: AsyncOutputAction,
    status: AsyncStatusAction,
    list: AsyncListAction,
    stop: AsyncStopAction,
}

impl AsyncTool {
    /// Bind the domain tool to its existing runtime.
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self {
            spawn: AsyncSpawnAction::new(runtime.clone()),
            output: AsyncOutputAction::new(runtime.clone()),
            status: AsyncStatusAction::new(runtime.clone()),
            list: AsyncListAction::new(runtime.clone()),
            stop: AsyncStopAction::new(runtime),
        }
    }

    fn handler(&self, params: &Value) -> anyhow::Result<&dyn Tool> {
        match params.get("action").and_then(Value::as_str) {
            Some("spawn") => Ok(&self.spawn),
            Some("output") => Ok(&self.output),
            Some("status") => Ok(&self.status),
            Some("list") => Ok(&self.list),
            Some("stop") => Ok(&self.stop),
            _ => anyhow::bail!("Async requires a supported action"),
        }
    }
}

#[async_trait]
impl Tool for AsyncTool {
    fn name(&self) -> &str {
        "Async"
    }
    fn description(&self) -> String {
        let mut description = "Run tools in the background and manage their receipts: spawn, output, status, list, or stop. Spawn returns task_id; use it for subsequent actions. Completion wakes the spawning session by default.".to_string();
        description.push_str(&format!("\n\nAction spawn:\n{}", self.spawn.description()));
        description.push_str(&format!(
            "\n\nAction output:\n{}",
            self.output.description()
        ));
        description.push_str(&format!(
            "\n\nAction status:\n{}",
            self.status.description()
        ));
        description.push_str(&format!("\n\nAction list:\n{}", self.list.description()));
        description.push_str(&format!("\n\nAction stop:\n{}", self.stop.description()));
        description
    }
    fn parameters(&self) -> Value {
        peko_tools_core::schema::action_schema(&[
            ("spawn", self.spawn.parameters()),
            ("output", self.output.parameters()),
            ("status", self.status.parameters()),
            ("list", self.list.parameters()),
            ("stop", self.stop.parameters()),
        ])
    }
    fn parallelizable(&self) -> bool {
        true
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
