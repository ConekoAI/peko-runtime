//! `Plan action create` — create a new multi-step execution plan.

use async_trait::async_trait;
use peko_plan::{PlanNode, PlanNodeStatus};
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{require_principal_id, resolve_node_id, SharedPlanPort};

/// Create a new plan in the principal's `plans/` directory.
pub struct PlanCreateAction {
    plan_port: SharedPlanPort,
}

impl PlanCreateAction {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PlanCreateAction {
    fn name(&self) -> &'static str {
        "Plan"
    }

    fn description(&self) -> String {
        r"Create a new multi-step execution plan for the current principal.

Use when: starting a non-trivial task that has 3+ distinct steps,
needs to be tracked across turns, or benefits from being resumable.

Parameters:
- title: string (required) — short imperative plan title
- nodes: array (required) — at least one node, each shaped:
    { nodeId: string?, step: string, dependsOn: array<string>?, status: string? }
  - nodeId is auto-assigned if omitted (recommended)
  - dependsOn lists nodeIds of other nodes in this plan; give those
    nodes an explicit nodeId (node_<8 chars a-z0-9>) to reference them
  - status defaults to 'pending'; valid values: pending|in_progress|completed|blocked|failed

Returns the created plan record including its planId and every
auto-assigned node id."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short imperative plan title."
                },
                "nodes": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Initial plan nodes, in insertion order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "nodeId": {
                                "type": "string",
                                "description": "Optional. Auto-assigned if omitted."
                            },
                            "step": {
                                "type": "string",
                                "description": "Imperative description of the step."
                            },
                            "dependsOn": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Node ids this step depends on."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending","in_progress","completed","blocked","failed"],
                                "description": "Optional. Defaults to 'pending'."
                            }
                        },
                        "required": ["step"]
                    }
                }
            },
            "required": ["title", "nodes"]
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
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Plan action create requires 'title'"))?
            .to_string();
        let nodes_json = params
            .get("nodes")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Plan action create requires 'nodes' array"))?;
        let nodes = parse_nodes(nodes_json)?;
        // A dependency on a node outside the plan would never complete,
        // leaving the step permanently not-ready; a cycle deadlocks.
        peko_plan::validate_nodes(&nodes).map_err(|e| anyhow::anyhow!("{e}"))?;
        let record = self.plan_port.create(principal_id, title, nodes).await?;
        Ok(serde_json::to_value(record)?)
    }
}

fn parse_nodes(arr: &[serde_json::Value]) -> anyhow::Result<Vec<PlanNode>> {
    use crate::tools::builtin::plan::{parse_depends_on, parse_status_param};
    use chrono::Utc;
    arr.iter()
        .map(|n| {
            let step = n
                .get("step")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("each node requires 'step'"))?
                .to_string();
            let node_id = resolve_node_id(n.get("nodeId").and_then(|v| v.as_str()))?;
            let depends_on = parse_depends_on(n.get("dependsOn"))?;
            let status = match n.get("status").and_then(|v| v.as_str()) {
                Some(s) => parse_status_param(s)?,
                None => PlanNodeStatus::Pending,
            };
            let now = Utc::now();
            Ok(PlanNode {
                node_id,
                step,
                status,
                depends_on,
                evidence: None,
                blocked_reason: None,
                created_at: now,
                updated_at: now,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::TestPlanPort;
    use peko_plan::PlanPort;
    use peko_subject::PrincipalId;
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with_principal() -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "Plan")
            .with_principal_id(peko_subject::PrincipalId::generate().0)
    }

    #[tokio::test]
    async fn create_plan_basic_happy_path() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let tool = PlanCreateAction::new(port);
        let result = tool
            .execute_with_context(
                json!({
                    "title": "ship v2",
                    "nodes": [
                        { "step": "spec", "nodeId": "node_spec0001" },
                        { "step": "build", "dependsOn": ["node_spec0001"] },
                        { "step": "ship", "status": "pending" }
                    ]
                }),
                &ctx_with_principal(),
            )
            .await
            .unwrap();
        assert_eq!(result["title"], "ship v2");
        assert!(result["planId"].as_str().unwrap().starts_with("plan_"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        // Auto-assigned ids start with `node_`.
        for n in nodes {
            assert!(n["nodeId"].as_str().unwrap().starts_with("node_"));
        }
        assert_eq!(nodes[1]["dependsOn"], json!(["node_spec0001"]));
    }

    /// Dependencies are checked before anything is persisted: a malformed
    /// id, a node outside the plan, or a cycle would leave a step that
    /// starts early or never becomes ready.
    #[tokio::test]
    async fn create_rejects_dependencies_that_could_never_resolve() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let tool = PlanCreateAction::new(port.clone());
        let ctx = ctx_with_principal();
        for (nodes, expected) in [
            (
                json!([{ "step": "a", "dependsOn": ["PHANTOM"] }]),
                "dependsOn",
            ),
            (
                json!([{ "step": "a", "dependsOn": [7] }]),
                "must be node ids",
            ),
            (
                json!([{ "step": "a", "dependsOn": "node_aaaa0001" }]),
                "must be an array",
            ),
            (
                json!([{ "step": "a", "dependsOn": ["node_ghost001"] }]),
                "depends on missing node node_ghost001",
            ),
            (
                json!([
                    { "step": "a", "nodeId": "node_aaaa0001", "dependsOn": ["node_bbbb0001"] },
                    { "step": "b", "nodeId": "node_bbbb0001", "dependsOn": ["node_aaaa0001"] }
                ]),
                "cycle",
            ),
        ] {
            let error = tool
                .execute_with_context(json!({ "title": "t", "nodes": nodes.clone() }), &ctx)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{nodes}: {error}");
        }
        let principal = PrincipalId(ctx.principal_id.clone().unwrap());
        assert!(
            PlanPort::list_for_principal(port.as_ref(), &principal)
                .await
                .unwrap()
                .is_empty(),
            "rejected plans must not be persisted"
        );
    }

    #[tokio::test]
    async fn create_plan_requires_title() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let tool = PlanCreateAction::new(port);
        let result = tool
            .execute_with_context(
                json!({ "nodes": [{ "step": "only" }] }),
                &ctx_with_principal(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_plan_requires_nodes() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let tool = PlanCreateAction::new(port);
        let result = tool
            .execute_with_context(json!({ "title": "x" }), &ctx_with_principal())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_plan_requires_principal_context() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let tool = PlanCreateAction::new(port);
        // No principal_id in ctx.
        let ctx = ToolContext::for_hook_run("run", "tc", "Plan");
        let result = tool
            .execute_with_context(json!({ "title": "x", "nodes": [{ "step": "y" }] }), &ctx)
            .await;
        assert!(result.is_err());
    }
}
