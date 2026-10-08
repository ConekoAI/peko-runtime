//! Plan domain tool. Action handlers remain private to this domain.

use super::{
    PlanAddStepAction, PlanCloseAction, PlanCreateAction, PlanGetAction, PlanListAction,
    PlanMarkStepAction, PlanRecordEvidenceAction, SharedPlanPort,
};
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::Value;

/// Manage principal-owned durable plans and dependency graphs: create, list, get, add_step, mark_step, record_evidence, or close. Closed plans cannot be modified.
pub struct PlanTool {
    create: PlanCreateAction,
    list: PlanListAction,
    get: PlanGetAction,
    add_step: PlanAddStepAction,
    mark_step: PlanMarkStepAction,
    record_evidence: PlanRecordEvidenceAction,
    close: PlanCloseAction,
}

impl PlanTool {
    /// Bind the domain tool to its existing runtime.
    pub fn new(runtime: SharedPlanPort) -> Self {
        Self {
            create: PlanCreateAction::new(runtime.clone()),
            list: PlanListAction::new(runtime.clone()),
            get: PlanGetAction::new(runtime.clone()),
            add_step: PlanAddStepAction::new(runtime.clone()),
            mark_step: PlanMarkStepAction::new(runtime.clone()),
            record_evidence: PlanRecordEvidenceAction::new(runtime.clone()),
            close: PlanCloseAction::new(runtime),
        }
    }

    fn handler(&self, params: &Value) -> anyhow::Result<&dyn Tool> {
        match params.get("action").and_then(Value::as_str) {
            Some("create") => Ok(&self.create),
            Some("list") => Ok(&self.list),
            Some("get") => Ok(&self.get),
            Some("add_step") => Ok(&self.add_step),
            Some("mark_step") => Ok(&self.mark_step),
            Some("record_evidence") => Ok(&self.record_evidence),
            Some("close") => Ok(&self.close),
            _ => anyhow::bail!("Plan requires a supported action"),
        }
    }
}

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        "Plan"
    }
    fn description(&self) -> String {
        let mut description = "Manage principal-owned durable plans and dependency graphs: create, list, get, add_step, mark_step, record_evidence, or close. Closed plans cannot be modified.".to_string();
        description.push_str(&format!(
            "\n\nAction create:\n{}",
            self.create.description()
        ));
        description.push_str(&format!("\n\nAction list:\n{}", self.list.description()));
        description.push_str(&format!("\n\nAction get:\n{}", self.get.description()));
        description.push_str(&format!(
            "\n\nAction add_step:\n{}",
            self.add_step.description()
        ));
        description.push_str(&format!(
            "\n\nAction mark_step:\n{}",
            self.mark_step.description()
        ));
        description.push_str(&format!(
            "\n\nAction record_evidence:\n{}",
            self.record_evidence.description()
        ));
        description.push_str(&format!("\n\nAction close:\n{}", self.close.description()));
        description
    }
    fn parameters(&self) -> Value {
        peko_tools_core::schema::action_schema(&[
            ("create", self.create.parameters()),
            ("list", self.list.parameters()),
            ("get", self.get.parameters()),
            ("add_step", self.add_step.parameters()),
            ("mark_step", self.mark_step.parameters()),
            ("record_evidence", self.record_evidence.parameters()),
            ("close", self.close.parameters()),
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
