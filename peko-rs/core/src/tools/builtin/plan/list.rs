//! `PePlanList` — list every plan owned by the current principal.

use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{require_principal_id, SharedPlanPort};

/// List all plans owned by the calling principal.
pub struct PePlanListTool {
    plan_port: SharedPlanPort,
}

impl PePlanListTool {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PePlanListTool {
    fn name(&self) -> &'static str {
        "PePlanList"
    }

    fn description(&self) -> String {
        r"List every plan owned by the current principal.

Returns `{ count: N, plans: [ ...PlanRecord ] }`. Plans are sorted
by created_at ascending (matches PlanStorage::list_for_principal)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
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
        _params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let principal_id = require_principal_id(ctx)?;
        let plans = self.plan_port.list_for_principal(&principal_id).await?;
        Ok(json!({
            "count": plans.len(),
            "plans": plans,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::{TestPlanPort, PePlanCreateTool};
    use peko_plan::NodeId;
    use peko_plan::PlanNode;
    use peko_plan::PlanNodeStatus;
    use peko_plan::PlanPort;
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with_principal(id: peko_subject::PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "PePlanList")
            .with_principal_id(id.0)
    }

    #[tokio::test]
    async fn list_returns_only_current_principals_plans() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p1 = peko_subject::PrincipalId::generate();
        let p2 = peko_subject::PrincipalId::generate();
        // Pre-populate: 2 for p1, 1 for p2.
        let now = chrono::Utc::now();
        let mk = |step: &str| PlanNode {
            node_id: NodeId::generate(),
            step: step.to_string(),
            status: PlanNodeStatus::Pending,
            depends_on: vec![],
            evidence: None,
            blocked_reason: None,
            created_at: now,
            updated_at: now,
        };
        port.create(p1.clone(), "p1a".into(), vec![mk("a")])
            .await
            .unwrap();
        port.create(p1.clone(), "p1b".into(), vec![mk("b")])
            .await
            .unwrap();
        port.create(p2.clone(), "p2a".into(), vec![mk("c")])
            .await
            .unwrap();

        let tool = PePlanListTool::new(port);
        let r1 = tool
            .execute_with_context(json!({}), &ctx_with_principal(p1.clone()))
            .await
            .unwrap();
        assert_eq!(r1["count"], 2);
        let r2 = tool
            .execute_with_context(json!({}), &ctx_with_principal(p2.clone()))
            .await
            .unwrap();
        assert_eq!(r2["count"], 1);
    }

    #[tokio::test]
    async fn list_requires_principal_context() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let tool = PePlanListTool::new(port);
        let ctx = ToolContext::for_hook_run("run", "tc", "PePlanList");
        let r = tool.execute_with_context(json!({}), &ctx).await;
        assert!(r.is_err());
        // Reference the create tool to keep the import alive when only this test runs.
        let _ = PePlanCreateTool::new(std::sync::Arc::new(TestPlanPort::new()));
    }
}