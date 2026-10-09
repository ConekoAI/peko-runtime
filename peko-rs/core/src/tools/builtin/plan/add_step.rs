//! `Plan action add_step` — append a new node to an existing plan.

use async_trait::async_trait;
use peko_plan::{PlanNode, PlanNodeStatus};
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{
    parse_depends_on, parse_status_param, require_principal_id, resolve_node_id, SharedPlanPort,
};

/// Append a node to an existing plan. Errors when the plan is closed
/// or when the supplied `nodeId` already exists in the plan.
pub struct PlanAddStepAction {
    plan_port: SharedPlanPort,
}

impl PlanAddStepAction {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PlanAddStepAction {
    fn name(&self) -> &'static str {
        "Plan"
    }

    fn description(&self) -> String {
        r"Append a new node to an existing open plan.

Parameters:
- planId: string (required)
- step: string (required) — imperative description of the new step
- nodeId: string? — auto-assigned if omitted
- dependsOn: array<string>? — ids of existing nodes in this plan the new step depends on
- status: string? — defaults to 'pending'

Returns the updated PlanRecord. Errors when the plan is closed or a
dependsOn id is not in the plan. A nodeId that already exists is a
no-op (the existing node is kept), so retries are safe."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "planId":     { "type": "string" },
                "step":       { "type": "string" },
                "nodeId":     { "type": "string", "description": "Optional. Auto-assigned if omitted." },
                "dependsOn":  {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "status":     {
                    "type": "string",
                    "enum": ["pending","in_progress","completed","blocked","failed"]
                }
            },
            "required": ["planId","step"]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(crate::tools::builtin::plan::missing_principal_error())
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let principal_id = require_principal_id(ctx)?;
        let plan_id = params
            .get("planId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Plan action add_step requires 'planId'"))?
            .to_string();
        let step = params
            .get("step")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Plan action add_step requires 'step'"))?
            .to_string();
        let node_id_str_for_error = params
            .get("nodeId")
            .and_then(|v| v.as_str())
            .map(String::from);
        let node_id = resolve_node_id(node_id_str_for_error.as_deref())?;
        let depends_on = parse_depends_on(params.get("dependsOn"))?;
        if !depends_on.is_empty() {
            // Nodes are never removed, so a dependency present now stays
            // present. A missing plan falls through to `add_node`'s error.
            if let Ok(plan) = self
                .plan_port
                .get_for_principal(&plan_id, &principal_id)
                .await
            {
                if let Some(missing) = depends_on
                    .iter()
                    .find(|dep| !plan.nodes.iter().any(|n| &n.node_id == *dep))
                {
                    anyhow::bail!("dependsOn: node {missing} is not in plan {plan_id}");
                }
            }
        }
        let status = match params.get("status").and_then(|v| v.as_str()) {
            Some(s) => parse_status_param(s)?,
            None => PlanNodeStatus::Pending,
        };
        let now = chrono::Utc::now();
        let node = PlanNode {
            node_id: node_id.clone(),
            step,
            status,
            depends_on,
            evidence: None,
            blocked_reason: None,
            created_at: now,
            updated_at: now,
        };
        // For the `InvalidNodeId` soft-error branch we want a useful
        // `id` field on the JSON. Surface the supplied id (or the
        // auto-assigned one if the user didn't pass one) — either
        // way the LLM gets back something to key off of.
        let reported_id = node_id_str_for_error.unwrap_or_else(|| node.node_id.to_string());
        match self.plan_port.add_node(&plan_id, &principal_id, node).await {
            Ok(rec) => Ok(serde_json::to_value(rec)?),
            Err(peko_plan::PlanError::InvalidNodeId(_)) => Ok(
                crate::tools::builtin::plan::not_found_error("Node", &reported_id),
            ),
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::{PlanCloseAction, PlanCreateAction, TestPlanPort};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with(id: peko_subject::PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "Plan").with_principal_id(id.0)
    }

    #[tokio::test]
    async fn add_step_appends_to_open_plan() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PlanCreateAction::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "first" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PlanAddStepAction::new(port);
        let updated = tool
            .execute_with_context(json!({ "planId": plan_id, "step": "second" }), &ctx_with(p))
            .await
            .unwrap();
        assert_eq!(updated["nodes"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    /// A colliding `nodeId` (e.g. a retried call) is idempotent in the
    /// production storage: the existing node wins and nothing is appended.
    async fn add_step_with_existing_node_id_is_idempotent() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PlanCreateAction::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "a", "nodeId": "node_a1b2c3d4" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PlanAddStepAction::new(port);
        let res = tool
            .execute_with_context(
                json!({
                    "planId": plan_id,
                    "step": "dup",
                    "nodeId": "node_a1b2c3d4"
                }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        assert!(res.get("error").is_none(), "{res}");
        let nodes = res["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 1, "nothing appended: {res}");
        assert_eq!(nodes[0]["step"], "a", "existing node wins");
    }

    #[tokio::test]
    async fn add_step_rejects_closed_plan() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PlanCreateAction::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "a" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let closer = PlanCloseAction::new(port.clone());
        closer
            .execute_with_context(
                json!({ "planId": plan_id, "reason": "done" }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let adder = PlanAddStepAction::new(port);
        let res = adder
            .execute_with_context(
                json!({ "planId": plan_id, "step": "too late" }),
                &ctx_with(p),
            )
            .await;
        assert!(res.is_err(), "closed plan must reject add_node");
    }

    #[tokio::test]
    async fn add_step_dependencies_must_be_nodes_of_the_plan() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let created = PlanCreateAction::new(port.clone())
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "first", "nodeId": "node_first001" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PlanAddStepAction::new(port);

        for (deps, expected) in [
            (
                json!(["node_ghost001"]),
                "node node_ghost001 is not in plan",
            ),
            (json!(["first"]), "dependsOn"),
        ] {
            let error = tool
                .execute_with_context(
                    json!({ "planId": plan_id, "step": "second", "dependsOn": deps.clone() }),
                    &ctx_with(p.clone()),
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{deps}: {error}");
        }

        let updated = tool
            .execute_with_context(
                json!({
                    "planId": plan_id,
                    "step": "second",
                    "dependsOn": ["node_first001"],
                    "status": "blocked",
                }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        let nodes = updated["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "only the valid call appended: {updated}");
        assert_eq!(nodes[1]["dependsOn"], json!(["node_first001"]));
        assert_eq!(nodes[1]["status"]["kind"], "blocked");
    }

    #[tokio::test]
    async fn add_step_requires_plan_id_and_step() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let tool = PlanAddStepAction::new(port);
        let ctx = ctx_with(peko_subject::PrincipalId::generate());
        for (params, expected) in [
            (json!({ "step": "s" }), "requires 'planId'"),
            (json!({ "planId": "plan_aaaa0001" }), "requires 'step'"),
        ] {
            let error = tool
                .execute_with_context(params, &ctx)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }
}
