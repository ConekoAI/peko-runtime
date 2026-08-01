//! `PlanGet` — fetch a single plan by id (with principal-scope check).

use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{require_principal_id, SharedPlanPort};

/// Fetch a single plan by id, scoped to the calling principal.
pub struct PlanGetTool {
    plan_port: SharedPlanPort,
}

impl PlanGetTool {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PlanGetTool {
    fn name(&self) -> &'static str {
        "PlanGet"
    }

    fn description(&self) -> String {
        r"Fetch a single plan by id, scoped to the calling principal.

Parameters:
- planId: string (required)

Returns the full PlanRecord. Soft-errors with
{ error: 'Plan not found', planId } when the plan does not exist;
returns a hard Err when the plan exists but belongs to a different
principal (PrincipalMismatch — a corruption signal)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "planId": {
                    "type": "string",
                    "description": "Plan id returned by PlanCreate (e.g., 'plan_01abc...')."
                }
            },
            "required": ["planId"]
        })
    }

    fn parallelizable(&self) -> bool {
        true
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
            .ok_or_else(|| anyhow::anyhow!("PlanGet requires 'planId'"))?
            .to_string();
        match self.plan_port.get_for_principal(&plan_id, &principal_id).await {
            Ok(rec) => Ok(serde_json::to_value(rec)?),
            // Soft-error JSON so the LLM can react — mirrors Task*.
            Err(peko_plan::PlanError::NotFound) => Ok(crate::tools::builtin::plan::not_found_error(
                "Plan", &plan_id,
            )),
            // Hard errors propagate so the framework surfaces them as
            // `success=false` with the PlanError::Display message.
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::{PlanCreateTool, TestPlanPort};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with(id: peko_subject::PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "PlanGet")
            .with_principal_id(id.0)
    }

    #[tokio::test]
    async fn get_returns_existing_plan() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PlanCreateTool::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "x", "nodes": [{ "step": "a" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PlanGetTool::new(port);
        let got = tool
            .execute_with_context(json!({ "planId": plan_id }), &ctx_with(p))
            .await
            .unwrap();
        assert_eq!(got["planId"], created["planId"]);
    }

    #[tokio::test]
    async fn get_returns_soft_error_for_missing_plan() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let tool = PlanGetTool::new(port);
        let got = tool
            .execute_with_context(
                json!({ "planId": "plan_doesnotexist" }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        assert_eq!(got["error"], "Plan not found");
        assert_eq!(got["id"], "plan_doesnotexist");
    }

    #[tokio::test]
    async fn get_hard_errors_on_principal_mismatch() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let owner = peko_subject::PrincipalId::generate();
        let other = peko_subject::PrincipalId::generate();
        let create = PlanCreateTool::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "x", "nodes": [{ "step": "a" }] }),
                &ctx_with(owner.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PlanGetTool::new(port);
        let res = tool
            .execute_with_context(json!({ "planId": plan_id }), &ctx_with(other))
            .await;
        assert!(res.is_err(), "PrincipalMismatch must propagate as Err");
    }
}