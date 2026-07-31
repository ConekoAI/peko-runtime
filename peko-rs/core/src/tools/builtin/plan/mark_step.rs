//! `PePlanMarkStep` — flip a node to a new status.

use async_trait::async_trait;
use peko_plan::NodeId;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{parse_status_param, require_principal_id, SharedPlanPort};

/// Update a node's status (`pending` / `in_progress` / `completed` /
/// `blocked` / `failed`). Mirrors `PePlanRecordEvidence` for the
/// status field.
pub struct PePlanMarkStepTool {
    plan_port: SharedPlanPort,
}

impl PePlanMarkStepTool {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PePlanMarkStepTool {
    fn name(&self) -> &'static str {
        "PePlanMarkStep"
    }

    fn description(&self) -> String {
        r"Flip a plan node's status.

Parameters:
- planId: string (required)
- nodeId: string (required)
- status: string (required) — one of pending|in_progress|completed|blocked|failed
- reason: string? — used when status is 'blocked' or 'failed'

Returns the updated PlanRecord. Soft-errors when the node is not
found in the plan."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "planId":  { "type": "string", "description": "Plan id." },
                "nodeId":  { "type": "string", "description": "Node id." },
                "status":  {
                    "type": "string",
                    "enum": ["pending","in_progress","completed","blocked","failed"]
                },
                "reason":  { "type": "string", "description": "Optional reason for blocked/failed." }
            },
            "required": ["planId","nodeId","status"]
        })
    }

    fn parallelizable(&self) -> bool {
        false
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
            .ok_or_else(|| anyhow::anyhow!("PePlanMarkStep requires 'planId'"))?
            .to_string();
        let node_id_str = params
            .get("nodeId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("PePlanMarkStep requires 'nodeId'"))?
            .to_string();
        let status_str = params
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("PePlanMarkStep requires 'status'"))?
            .to_string();
        let reason = params.get("reason").and_then(|v| v.as_str()).map(String::from);
        let node_id = NodeId::parse(&node_id_str).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut status = parse_status_param(&status_str)?;
        if let Some(reason_text) = reason {
            use chrono::Utc;
            status = match status {
                peko_plan::PlanNodeStatus::Blocked { .. } => peko_plan::PlanNodeStatus::Blocked {
                    reason: reason_text,
                    since: Utc::now(),
                },
                peko_plan::PlanNodeStatus::Failed { .. } => peko_plan::PlanNodeStatus::Failed {
                    reason: reason_text,
                    last_attempt_at: Utc::now(),
                },
                other => other,
            };
        }
        match self
            .plan_port
            .mark_node_status(&plan_id, &principal_id, &node_id, status)
            .await
        {
            Ok(rec) => Ok(serde_json::to_value(rec)?),
            Err(peko_plan::PlanError::InvalidNodeId(_)) => Ok(
                crate::tools::builtin::plan::not_found_error("Node", &node_id_str),
            ),
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::{PePlanCreateTool, TestPlanPort};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with(id: peko_subject::PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "PePlanMarkStep")
            .with_principal_id(id.0)
    }

    #[tokio::test]
    async fn mark_step_happy_path_flips_pending_to_in_progress() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PePlanCreateTool::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "a" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let node_id = created["nodes"][0]["nodeId"].as_str().unwrap().to_string();
        let tool = PePlanMarkStepTool::new(port);
        let updated = tool
            .execute_with_context(
                json!({ "planId": plan_id, "nodeId": node_id, "status": "in_progress" }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        assert_eq!(updated["nodes"][0]["status"]["kind"], "in_progress");
    }

    #[tokio::test]
    async fn mark_step_soft_errors_on_unknown_node() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PePlanCreateTool::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "a" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PePlanMarkStepTool::new(port);
        let res = tool
            .execute_with_context(
                json!({
                    "planId": plan_id,
                    "nodeId": "node_00000000",
                    "status": "in_progress"
                }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        assert_eq!(res["error"], "Node not found");
    }

    #[tokio::test]
    async fn mark_step_mirrors_blocked_reason() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PePlanCreateTool::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "a" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let node_id = created["nodes"][0]["nodeId"].as_str().unwrap().to_string();
        let tool = PePlanMarkStepTool::new(port);
        let updated = tool
            .execute_with_context(
                json!({
                    "planId": plan_id,
                    "nodeId": node_id,
                    "status": "blocked",
                    "reason": "waiting on review"
                }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        // `blocked_reason` parallel field mirrors the structured payload.
        assert_eq!(updated["nodes"][0]["blockedReason"], "waiting on review");
    }
}