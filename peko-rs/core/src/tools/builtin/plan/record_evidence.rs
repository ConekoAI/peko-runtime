//! `PlanRecordEvidence` — record per-node evidence (output, artifacts, decided_by).

use async_trait::async_trait;
use peko_plan::{NodeEvidence, NodeId};
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{require_principal_id, SharedPlanPort};

/// Record per-node evidence: a free-form output string, an array of
/// artifact paths, and an optional `decided_by` attribution.
pub struct PlanRecordEvidenceTool {
    plan_port: SharedPlanPort,
}

impl PlanRecordEvidenceTool {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PlanRecordEvidenceTool {
    fn name(&self) -> &'static str {
        "PlanRecordEvidence"
    }

    fn description(&self) -> String {
        r"Record per-node evidence on a plan step.

Parameters:
- planId: string (required)
- nodeId: string (required)
- output: string (required) — short summary of what the node produced
- artifacts: array<string>? — file paths or external references
- decidedBy: string? — attribution (e.g., 'root-agent', 'subagent:researcher')

Returns the updated PlanRecord. Soft-errors when the node is not
found."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "planId":     { "type": "string" },
                "nodeId":     { "type": "string" },
                "output":     { "type": "string", "description": "Short outcome summary." },
                "artifacts":  {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional file paths / external refs."
                },
                "decidedBy":  {
                    "type": "string",
                    "description": "Optional attribution string."
                }
            },
            "required": ["planId","nodeId","output"]
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
            .ok_or_else(|| anyhow::anyhow!("PlanRecordEvidence requires 'planId'"))?
            .to_string();
        let node_id_str = params
            .get("nodeId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("PlanRecordEvidence requires 'nodeId'"))?
            .to_string();
        let output = params
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("PlanRecordEvidence requires 'output'"))?
            .to_string();
        let artifacts = params
            .get("artifacts")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let decided_by = params
            .get("decidedBy")
            .and_then(|v| v.as_str())
            .map(String::from);
        let node_id = NodeId::parse(&node_id_str).map_err(|e| anyhow::anyhow!("{e}"))?;
        let evidence = NodeEvidence {
            output,
            artifacts,
            decided_by,
        };
        match self
            .plan_port
            .set_node_evidence(&plan_id, &principal_id, &node_id, evidence)
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
    use crate::tools::builtin::plan::{PlanCreateTool, TestPlanPort};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with(id: peko_subject::PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "PlanRecordEvidence")
            .with_principal_id(id.0)
    }

    #[tokio::test]
    async fn record_evidence_happy_path() {
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
        let node_id = created["nodes"][0]["nodeId"].as_str().unwrap().to_string();
        let tool = PlanRecordEvidenceTool::new(port);
        let updated = tool
            .execute_with_context(
                json!({
                    "planId": plan_id,
                    "nodeId": node_id,
                    "output": "compiled cleanly",
                    "artifacts": ["/tmp/build.log"],
                    "decidedBy": "root-agent"
                }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        let evidence = &updated["nodes"][0]["evidence"];
        assert_eq!(evidence["output"], "compiled cleanly");
        assert_eq!(evidence["artifacts"][0], "/tmp/build.log");
        assert_eq!(evidence["decidedBy"], "root-agent");
    }

    #[tokio::test]
    async fn record_evidence_soft_errors_on_unknown_node() {
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
        let tool = PlanRecordEvidenceTool::new(port);
        let res = tool
            .execute_with_context(
                json!({
                    "planId": plan_id,
                    "nodeId": "node_zzzzzzzz",
                    "output": "x"
                }),
                &ctx_with(p),
            )
            .await
            .unwrap();
        assert_eq!(res["error"], "Node not found");
    }
}