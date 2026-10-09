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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::TestPlanPort;
    use serde_json::json;

    /// One plan's life, every action routed through the domain tool.
    #[tokio::test]
    async fn every_action_dispatches_through_the_domain_tool() {
        let tool = PlanTool::new(std::sync::Arc::new(TestPlanPort::new()));
        let ctx = ToolContext::for_hook_run("run", "tc", "Plan")
            .with_principal_id(peko_subject::PrincipalId::generate().0);
        let call = |params: Value| {
            let (tool, ctx) = (&tool, &ctx);
            async move {
                tool.execute_with_context(params.clone(), ctx)
                    .await
                    .unwrap_or_else(|e| panic!("{params}: {e}"))
            }
        };

        let created = call(json!({
            "action": "create",
            "title": "ship",
            "nodes": [{ "step": "build", "nodeId": "node_build001" }]
        }))
        .await;
        let plan_id = created["planId"].as_str().unwrap().to_string();
        call(json!({
            "action": "add_step",
            "planId": plan_id,
            "step": "deploy",
            "nodeId": "node_deploy01",
            "dependsOn": ["node_build001"]
        }))
        .await;
        let failed = call(json!({
            "action": "mark_step",
            "planId": plan_id,
            "nodeId": "node_build001",
            "status": "failed",
            "reason": "flaky linker"
        }))
        .await;
        assert_eq!(failed["nodes"][0]["status"]["kind"], "failed");
        assert_eq!(failed["nodes"][0]["status"]["reason"], "flaky linker");
        let blocked = call(json!({
            "action": "mark_step",
            "planId": plan_id,
            "nodeId": "node_deploy01",
            "status": "blocked"
        }))
        .await;
        assert_eq!(blocked["nodes"][1]["status"]["reason"], "set by tool");
        call(json!({
            "action": "record_evidence",
            "planId": plan_id,
            "nodeId": "node_build001",
            "output": "linker log"
        }))
        .await;
        let got = call(json!({ "action": "get", "planId": plan_id })).await;
        assert_eq!(got["nodes"].as_array().unwrap().len(), 2);
        let listed = call(json!({ "action": "list" })).await;
        assert!(listed.to_string().contains(&plan_id), "{listed}");
        call(json!({ "action": "close", "planId": plan_id, "reason": "done" })).await;

        let error = tool
            .execute_with_context(
                json!({
                    "action": "mark_step",
                    "planId": plan_id,
                    "nodeId": "node_build001",
                    "status": "paused"
                }),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown plan node status: paused"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn unsupported_actions_and_contextless_calls_are_rejected() {
        let tool = PlanTool::new(std::sync::Arc::new(TestPlanPort::new()));
        let ctx = ToolContext::for_hook_run("run", "tc", "Plan").with_principal_id("p");
        for params in [json!({ "action": "purge" }), json!({})] {
            let error = tool.execute_with_context(params, &ctx).await.unwrap_err();
            assert!(error.to_string().contains("supported action"), "{error}");
        }
        for action in [
            "create",
            "list",
            "get",
            "add_step",
            "mark_step",
            "record_evidence",
            "close",
        ] {
            let error = tool.execute(json!({ "action": action })).await.unwrap_err();
            assert!(
                error.to_string().contains("principal context"),
                "{action}: {error}"
            );
        }
    }
}
