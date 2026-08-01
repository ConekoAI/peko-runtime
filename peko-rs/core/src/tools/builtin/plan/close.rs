//! `PlanClose` — close a plan (idempotent: second close returns
//! `PlanError::AlreadyClosed`).

use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{require_principal_id, SharedPlanPort};

/// Close an open plan. Records a `ClosedState` with the supplied
/// reason. Second close on the same plan returns
/// `PlanError::AlreadyClosed` (propagated as a hard error so the LLM
/// sees the duplicate-call signal).
pub struct PlanCloseTool {
    plan_port: SharedPlanPort,
}

impl PlanCloseTool {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PlanCloseTool {
    fn name(&self) -> &'static str {
        "PlanClose"
    }

    fn description(&self) -> String {
        r"Close an open plan.

Parameters:
- planId: string (required)
- reason: string (required) — short human-readable note explaining
  why the plan is being closed.

Returns { planId, closedAt, reason } on success. Second close on
the same plan returns Err(PlanError::AlreadyClosed) — the storage
layer treats close as idempotent for state but non-idempotent for
return value so callers can detect duplicate-close races."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "planId": { "type": "string" },
                "reason": { "type": "string", "description": "Short reason for closure." }
            },
            "required": ["planId","reason"]
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
            .ok_or_else(|| anyhow::anyhow!("PlanClose requires 'planId'"))?
            .to_string();
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("PlanClose requires 'reason'"))?
            .to_string();
        self.plan_port
            .close(&plan_id, &principal_id, reason.clone())
            .await?;
        Ok(json!({
            "planId": plan_id,
            "reason": reason,
            "closedAt": chrono::Utc::now(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::{PlanCreateTool, TestPlanPort};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with(id: peko_subject::PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "PlanClose")
            .with_principal_id(id.0)
    }

    #[tokio::test]
    async fn close_happy_path() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PlanCreateTool::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "a" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PlanCloseTool::new(port);
        let res = tool
            .execute_with_context(
                json!({ "planId": plan_id, "reason": "all done" }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        assert_eq!(res["reason"], "all done");
    }

    #[tokio::test]
    async fn close_second_call_errors_already_closed() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let create = PlanCreateTool::new(port.clone());
        let created = create
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "a" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let tool = PlanCloseTool::new(port);
        tool.execute_with_context(
            json!({ "planId": plan_id, "reason": "first" }),
            &ctx_with(p.clone()),
        )
        .await
        .unwrap();
        let res = tool
            .execute_with_context(
                json!({ "planId": plan_id, "reason": "second" }),
                &ctx_with(p),
            )
            .await;
        assert!(res.is_err(), "second close must propagate AlreadyClosed");
    }
}