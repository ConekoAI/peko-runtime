//! `Plan action mark_step` — flip a node to a new status.

use async_trait::async_trait;
use peko_plan::NodeId;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::tools::builtin::plan::{parse_status_param, require_principal_id, SharedPlanPort};

/// Update a node's status (`pending` / `in_progress` / `completed` /
/// `blocked` / `failed`). Mirrors `Plan action record_evidence` for the
/// status field.
pub struct PlanMarkStepAction {
    plan_port: SharedPlanPort,
}

impl PlanMarkStepAction {
    #[must_use]
    pub fn new(plan_port: SharedPlanPort) -> Self {
        Self { plan_port }
    }
}

#[async_trait]
impl Tool for PlanMarkStepAction {
    fn name(&self) -> &'static str {
        "Plan"
    }

    fn description(&self) -> String {
        r"Flip a plan node's status.

Parameters:
- planId: string (required)
- nodeId: string (required)
- status: string (required) — one of pending|in_progress|completed|blocked|failed
- reason: string? — used when status is 'blocked' or 'failed'
- checkAt: string? — when to come back to the step: RFC 3339 or a duration
  from now ('90m', '2d'); '' clears it. Completing a step clears it.

Returns the updated plan. An unknown planId or nodeId is an error."
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
                "reason":  { "type": "string", "description": "Optional reason for blocked/failed." },
                "checkAt": { "type": "string", "description": "When to come back to this step: RFC 3339 time or a duration from now (\"90m\", \"2d\"). Until then a ready or in-progress step is snoozed; once it passes the check-in is due and your heartbeat runs for it. An empty string clears it." }
            },
            "required": ["planId","nodeId","status"]
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
            .ok_or_else(|| anyhow::anyhow!("Plan action mark_step requires 'planId'"))?
            .to_string();
        let node_id_str = params
            .get("nodeId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Plan action mark_step requires 'nodeId'"))?
            .to_string();
        let status_str = params
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Plan action mark_step requires 'status'"))?
            .to_string();
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(String::from);
        let node_id = NodeId::parse(&node_id_str).map_err(|e| anyhow::anyhow!("{e}"))?;
        let check_at = crate::tools::builtin::plan::parse_check_at(params.get("checkAt"))?;
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
        let completed = matches!(status, peko_plan::PlanNodeStatus::Completed { .. });
        let mut record = self
            .plan_port
            .mark_node_status(&plan_id, &principal_id, &node_id, status)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        // A completed step keeps no check-in (the port cleared it).
        let change = match check_at {
            crate::tools::builtin::plan::CheckAtParam::Unchanged => None,
            crate::tools::builtin::plan::CheckAtParam::Clear => Some(None),
            crate::tools::builtin::plan::CheckAtParam::At(at) => Some(Some(at)),
        };
        if let Some(check_at) = change.filter(|_| !completed) {
            record = self
                .plan_port
                .set_node_check_at(&plan_id, &principal_id, &node_id, check_at)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(serde_json::to_value(record)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::plan::{PlanCreateAction, TestPlanPort};
    use peko_tools_core::ToolContext;
    use serde_json::json;

    fn ctx_with(id: peko_subject::PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", "Plan").with_principal_id(id.0)
    }

    #[tokio::test]
    async fn mark_step_happy_path_flips_pending_to_in_progress() {
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
        let node_id = created["nodes"][0]["nodeId"].as_str().unwrap().to_string();
        let tool = PlanMarkStepAction::new(port);
        let updated = tool
            .execute_with_context(
                json!({ "planId": plan_id, "nodeId": node_id, "status": "in_progress" }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        assert_eq!(updated["nodes"][0]["status"]["kind"], "in_progress");
    }

    /// ADR-084: `checkAt` is set at create (relative), rescheduled and
    /// cleared by mark_step, and dropped when the step completes; a
    /// malformed value is an error.
    #[tokio::test]
    async fn check_at_is_set_rescheduled_and_cleared() {
        let port = std::sync::Arc::new(TestPlanPort::new());
        let p = peko_subject::PrincipalId::generate();
        let created = PlanCreateAction::new(port.clone())
            .execute_with_context(
                json!({ "title": "t", "nodes": [{ "step": "follow up", "checkAt": "2d" }] }),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        let check_at = |v: &serde_json::Value| {
            v["nodes"][0]["checkAt"]
                .as_str()
                .map(|s| chrono::DateTime::parse_from_rfc3339(s).unwrap())
        };
        let first = check_at(&created).expect("checkAt set");
        let hours = (first.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_hours();
        assert!((47..=48).contains(&hours), "{hours}");
        let plan_id = created["planId"].as_str().unwrap().to_string();
        let node_id = created["nodes"][0]["nodeId"].as_str().unwrap().to_string();
        let tool = PlanMarkStepAction::new(port);
        let mark = |extra: serde_json::Value| {
            let mut params = json!({ "planId": plan_id, "nodeId": node_id });
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            params
        };

        let moved = tool
            .execute_with_context(
                mark(json!({ "status": "blocked", "reason": "waiting", "checkAt": "2030-01-01T09:00:00Z" })),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            check_at(&moved).unwrap().to_rfc3339(),
            "2030-01-01T09:00:00+00:00"
        );
        let cleared = tool
            .execute_with_context(
                mark(json!({ "status": "blocked", "checkAt": "" })),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        assert!(check_at(&cleared).is_none(), "{cleared}");

        tool.execute_with_context(
            mark(json!({ "status": "pending", "checkAt": "1h" })),
            &ctx_with(p.clone()),
        )
        .await
        .unwrap();
        let done = tool
            .execute_with_context(
                mark(json!({ "status": "completed", "checkAt": "1h" })),
                &ctx_with(p.clone()),
            )
            .await
            .unwrap();
        assert!(check_at(&done).is_none(), "completing clears it: {done}");

        let error = tool
            .execute_with_context(
                mark(json!({ "status": "pending", "checkAt": "soonish" })),
                &ctx_with(p),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("RFC 3339"), "{error}");
    }

    #[tokio::test]
    async fn mark_step_errors_on_unknown_node() {
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
        let tool = PlanMarkStepAction::new(port);
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
            .unwrap_err();
        assert!(res.to_string().contains("not found in plan"), "{res}");
    }

    #[tokio::test]
    async fn mark_step_mirrors_blocked_reason() {
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
        let node_id = created["nodes"][0]["nodeId"].as_str().unwrap().to_string();
        let tool = PlanMarkStepAction::new(port);
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
