//! `peko_plan` built-in tool surface — 7 tools that wrap the
//! [`peko_plan::PlanPort`] trait.
//!
//! Mirrors the `tasks/` module shape: one file per tool, a shared
//! [`SharedPlanPort`] type alias, a `missing_principal_error` helper,
//! and an in-memory [`TestPlanPort`] fixture gated under `#[cfg(test)]`.
//!
//! The seven tools cover every [`peko_plan::PlanPort`] action an
//! LLM-driven root agent should be able to invoke:
//!
//! | Tool | Port method |
//! |---|---|
//! | [`PePlanCreateTool`] | [`peko_plan::PlanPort::create`] |
//! | [`PePlanListTool`] | [`peko_plan::PlanPort::list_for_principal`] |
//! | [`PePlanGetTool`] | [`peko_plan::PlanPort::get_for_principal`] |
//! | [`PePlanMarkStepTool`] | [`peko_plan::PlanPort::mark_node_status`] |
//! | [`PePlanRecordEvidenceTool`] | [`peko_plan::PlanPort::set_node_evidence`] |
//! | [`PePlanAddStepTool`] | [`peko_plan::PlanPort::add_node`] |
//! | [`PePlanCloseTool`] | [`peko_plan::PlanPort::close`] |
//!
//! All seven bind to the same `Arc<dyn PlanPort>` (held on
//! [`crate::principal::Principal`] as `plan_port`, plumbed into
//! [`crate::agents::Agent`] via `with_principal_plan_port`).
//!
//! ## Capability gating
//!
//! Each tool requires its own `tool:<Name>` grant per the F37 funnel's
//! [`crate::extensions::framework::core::tool_registry::is_tool_enabled`]
//! rule. Plan tools are visible to any principal that grants the
//! `tool:PePlan*` set.

pub mod add_step;
pub mod close;
pub mod create;
pub mod get;
pub mod list;
pub mod mark_step;
pub mod record_evidence;

pub use add_step::PePlanAddStepTool;
pub use close::PePlanCloseTool;
pub use create::PePlanCreateTool;
pub use get::PePlanGetTool;
pub use list::PePlanListTool;
pub use mark_step::PePlanMarkStepTool;
pub use record_evidence::PePlanRecordEvidenceTool;

use anyhow::Result as AnyhowResult;
use peko_plan::{NodeId, PlanNodeStatus, PlanPort};
use peko_subject::PrincipalId;
use peko_tools_core::ToolContext;
use std::sync::Arc;

#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use peko_plan::Result;

/// Shared handle threaded through every `PePlan*Tool` constructor.
pub type SharedPlanPort = Arc<dyn PlanPort>;

/// Surface an `anyhow::Error` for tool callers that omit the
/// `principal_id` from [`ToolContext`] — same shape as
/// `tasks::missing_session_error`.
pub fn missing_principal_error() -> anyhow::Error {
    anyhow::anyhow!("peko_plan tool requires a principal context")
}

/// Pull `PrincipalId` out of a [`ToolContext`], returning
/// `missing_principal_error()` if absent.
///
/// `ToolContext::principal_id` is `Option<String>` (the wire form
/// shared with the F37 funnel). Wrap into the newtype
/// [`PrincipalId`] here so the [`peko_plan::PlanPort`] method
/// signatures match without forcing every tool to repeat the
/// constructor call.
pub fn require_principal_id(ctx: &ToolContext) -> AnyhowResult<PrincipalId> {
    ctx.principal_id
        .clone()
        .ok_or_else(missing_principal_error)
        .map(PrincipalId)
}

/// Soft-error JSON shape used by `get` / `mark_step` /
/// `record_evidence` / `add_step` when the targeted plan or node is
/// not found. Mirrors `tasks::update::{"error": "Todo not found"}`.
pub fn not_found_error(kind: &str, id: &str) -> serde_json::Value {
    serde_json::json!({
        "error": format!("{kind} not found"),
        "id": id,
    })
}

/// Parse the structured `PlanNodeStatus` from the JSON a tool receives.
/// `blocked` and `failed` need reason + timestamp; the other variants
/// take no extra payload.
pub fn parse_status_param(s: &str) -> AnyhowResult<PlanNodeStatus> {
    use chrono::Utc;
    match s {
        "pending" => Ok(PlanNodeStatus::Pending),
        "in_progress" => Ok(PlanNodeStatus::InProgress),
        "completed" => Ok(PlanNodeStatus::Completed {
            completed_at: Utc::now(),
        }),
        "blocked" => Ok(PlanNodeStatus::Blocked {
            reason: "set by tool".to_string(),
            since: Utc::now(),
        }),
        "failed" => Ok(PlanNodeStatus::Failed {
            reason: "set by tool".to_string(),
            last_attempt_at: Utc::now(),
        }),
        other => Err(anyhow::anyhow!("unknown plan node status: {other}")),
    }
}

/// Resolve a `NodeId` from a string the LLM supplied. User-supplied
/// ids must round-trip through [`NodeId::parse`]; fresh ids are
/// generated when the field is absent.
pub fn resolve_node_id(s: Option<&str>) -> AnyhowResult<NodeId> {
    match s {
        Some(raw) => NodeId::parse(raw).map_err(|e| anyhow::anyhow!("{e}")),
        None => Ok(NodeId::generate()),
    }
}

// ---------------------------------------------------------------------------
// Test fixture — in-memory PlanPort. Mirrors TestTodoRuntime shape.
// ---------------------------------------------------------------------------

/// In-memory [`PlanPort`] for tests. Mirrors the production
/// `PlanStorage` semantics: auto-assigned `plan_id`, monotonic
/// `updated_at`, the same `AlreadyClosed` / `PrincipalMismatch` /
/// `InvalidNodeId` error surface. Used by per-tool unit tests.
#[cfg(test)]
pub struct TestPlanPort {
    plans: std::sync::Mutex<std::collections::HashMap<String, PlanRecord>>,
    /// Monotonic counter for plan ids (no random component — tests
    /// are deterministic; the production `rand_u32` + splitmix64
    /// path stays in `PlanStorage`).
    next_plan_seq: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
use peko_plan::{PlanError, PlanNode, PlanRecord};

#[cfg(test)]
impl TestPlanPort {
    #[must_use]
    pub fn new() -> Self {
        Self {
            plans: std::sync::Mutex::new(std::collections::HashMap::new()),
            next_plan_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn next_plan_id(&self) -> String {
        let n = self
            .next_plan_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("plan_{n:08x}")
    }
}

#[cfg(test)]
impl Default for TestPlanPort {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[cfg(test)]
#[async_trait]
impl PlanPort for TestPlanPort {
    async fn get(&self, plan_id: &str) -> Result<Option<PlanRecord>> {
        Ok(self.plans.lock().unwrap().get(plan_id).cloned())
    }

    async fn get_for_principal(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
    ) -> Result<PlanRecord> {
        let plans = self.plans.lock().unwrap();
        let rec = plans
            .get(plan_id)
            .cloned()
            .ok_or(PlanError::NotFound)?;
        if &rec.principal_id != principal_id {
            return Err(PlanError::PrincipalMismatch {
                expected: rec.principal_id.0.clone(),
                got: principal_id.0.clone(),
            });
        }
        Ok(rec)
    }

    async fn list_for_principal(
        &self,
        principal_id: &PrincipalId,
    ) -> Result<Vec<PlanRecord>> {
        Ok(self
            .plans
            .lock()
            .unwrap()
            .values()
            .filter(|r| &r.principal_id == principal_id)
            .cloned()
            .collect())
    }

    async fn current_focus(
        &self,
        principal_id: &PrincipalId,
    ) -> Result<Option<PlanRecord>> {
        let plans = self.plans.lock().unwrap();
        Ok(plans
            .values()
            .filter(|r| &r.principal_id == principal_id && r.closed.is_none())
            .filter(|r| r.nodes.iter().any(|n| matches!(n.status, PlanNodeStatus::InProgress)))
            .max_by_key(|r| r.updated_at)
            .cloned())
    }

    async fn load_resumable(
        &self,
        principal_id: &PrincipalId,
    ) -> Result<Vec<PlanRecord>> {
        let plans = self.plans.lock().unwrap();
        Ok(plans
            .values()
            .filter(|r| &r.principal_id == principal_id && r.closed.is_none())
            .filter(|r| {
                r.nodes.iter().any(|n| {
                    !matches!(
                        n.status,
                        PlanNodeStatus::Completed { .. }
                    )
                })
            })
            .cloned()
            .collect())
    }

    async fn create(
        &self,
        principal_id: PrincipalId,
        title: String,
        nodes: Vec<PlanNode>,
    ) -> Result<PlanRecord> {
        let id = self.next_plan_id();
        let ts = now();
        let rec = PlanRecord {
            plan_id: id.clone(),
            principal_id,
            schema_version: peko_plan::PLAN_SCHEMA_VERSION,
            title,
            created_at: ts,
            updated_at: ts,
            nodes,
            closed: None,
        };
        self.plans.lock().unwrap().insert(id, rec.clone());
        Ok(rec)
    }

    async fn close(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        reason: String,
    ) -> Result<()> {
        let mut plans = self.plans.lock().unwrap();
        let rec = plans.get_mut(plan_id).ok_or(PlanError::NotFound)?;
        if &rec.principal_id != principal_id {
            return Err(PlanError::PrincipalMismatch {
                expected: rec.principal_id.0.clone(),
                got: principal_id.0.clone(),
            });
        }
        if rec.closed.is_some() {
            return Err(PlanError::AlreadyClosed);
        }
        rec.closed = Some(peko_plan::ClosedState::new(reason));
        rec.updated_at = now();
        Ok(())
    }

    async fn mark_node_status(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        status: PlanNodeStatus,
    ) -> Result<PlanRecord> {
        let mut plans = self.plans.lock().unwrap();
        let rec = plans.get_mut(plan_id).ok_or(PlanError::NotFound)?;
        if &rec.principal_id != principal_id {
            return Err(PlanError::PrincipalMismatch {
                expected: rec.principal_id.0.clone(),
                got: principal_id.0.clone(),
            });
        }
        let node = rec
            .nodes
            .iter_mut()
            .find(|n| &n.node_id == node_id)
            .ok_or_else(|| {
                PlanError::InvalidNodeId(format!(
                    "node {node_id} not found in plan {}",
                    rec.plan_id
                ))
            })?;
        node.status = status.clone();
        node.updated_at = now();
        node.blocked_reason = match &status {
            PlanNodeStatus::Blocked { reason, .. } => Some(reason.clone()),
            _ => None,
        };
        rec.updated_at = now();
        Ok(rec.clone())
    }

    async fn set_node_evidence(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        evidence: peko_plan::NodeEvidence,
    ) -> Result<PlanRecord> {
        let mut plans = self.plans.lock().unwrap();
        let rec = plans.get_mut(plan_id).ok_or(PlanError::NotFound)?;
        if &rec.principal_id != principal_id {
            return Err(PlanError::PrincipalMismatch {
                expected: rec.principal_id.0.clone(),
                got: principal_id.0.clone(),
            });
        }
        let node = rec
            .nodes
            .iter_mut()
            .find(|n| &n.node_id == node_id)
            .ok_or_else(|| {
                PlanError::InvalidNodeId(format!(
                    "node {node_id} not found in plan {}",
                    rec.plan_id
                ))
            })?;
        node.evidence = Some(evidence);
        node.updated_at = now();
        rec.updated_at = now();
        Ok(rec.clone())
    }

    async fn add_node(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node: PlanNode,
    ) -> Result<PlanRecord> {
        let mut plans = self.plans.lock().unwrap();
        let rec = plans.get_mut(plan_id).ok_or(PlanError::NotFound)?;
        if &rec.principal_id != principal_id {
            return Err(PlanError::PrincipalMismatch {
                expected: rec.principal_id.0.clone(),
                got: principal_id.0.clone(),
            });
        }
        if rec.closed.is_some() {
            return Err(PlanError::AlreadyClosed);
        }
        if rec.nodes.iter().any(|n| n.node_id == node.node_id) {
            return Err(PlanError::InvalidNodeId(format!(
                "node {} already exists in plan {}",
                node.node_id.as_str(),
                rec.plan_id
            )));
        }
        rec.nodes.push(node);
        rec.updated_at = now();
        Ok(rec.clone())
    }
}