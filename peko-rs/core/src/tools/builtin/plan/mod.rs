//! Plan domain actions backed by the principal-owned PlanPort.

mod add_step;
mod close;
mod create;
mod get;
mod list;
mod mark_step;
mod record_evidence;

pub(crate) use add_step::PlanAddStepAction;
pub(crate) use close::PlanCloseAction;
pub(crate) use create::PlanCreateAction;
pub(crate) use get::PlanGetAction;
pub(crate) use list::PlanListAction;
pub(crate) use mark_step::PlanMarkStepAction;
pub(crate) use record_evidence::PlanRecordEvidenceAction;

use anyhow::Result as AnyhowResult;
use peko_plan::{NodeId, PlanNodeStatus, PlanPort};
use peko_subject::PrincipalId;
use peko_tools_core::ToolContext;
use std::sync::Arc;

#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use peko_plan::Result;

/// Shared handle threaded through every `Plan*Tool` constructor.
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
// Test fixture — the production `PlanStorage` in a private tempdir.
// ---------------------------------------------------------------------------

/// Production [`peko_plan::PlanStorage`] rooted in a tempdir that lives as
/// long as the port, so tool tests exercise the real persistence,
/// validation, and ownership rules instead of a hand-maintained mirror.
#[cfg(test)]
pub struct TestPlanPort {
    _dir: tempfile::TempDir,
    storage: peko_plan::PlanStorage,
}

#[cfg(test)]
use peko_plan::{NodeEvidence, PlanNode, PlanRecord};

#[cfg(test)]
impl TestPlanPort {
    #[must_use]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("plan tempdir");
        let storage = peko_plan::PlanStorage::new(dir.path().join("plans"));
        Self { _dir: dir, storage }
    }
}

#[cfg(test)]
impl Default for TestPlanPort {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[async_trait]
impl PlanPort for TestPlanPort {
    async fn get(&self, plan_id: &str) -> Result<Option<PlanRecord>> {
        PlanPort::get(&self.storage, plan_id).await
    }
    async fn get_for_principal(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
    ) -> Result<PlanRecord> {
        PlanPort::get_for_principal(&self.storage, plan_id, principal_id).await
    }
    async fn list_for_principal(&self, principal_id: &PrincipalId) -> Result<Vec<PlanRecord>> {
        PlanPort::list_for_principal(&self.storage, principal_id).await
    }
    async fn current_focus(&self, principal_id: &PrincipalId) -> Result<Option<PlanRecord>> {
        PlanPort::current_focus(&self.storage, principal_id).await
    }
    async fn load_resumable(&self, principal_id: &PrincipalId) -> Result<Vec<PlanRecord>> {
        PlanPort::load_resumable(&self.storage, principal_id).await
    }
    async fn create(
        &self,
        principal_id: PrincipalId,
        title: String,
        nodes: Vec<PlanNode>,
    ) -> Result<PlanRecord> {
        PlanPort::create(&self.storage, principal_id, title, nodes).await
    }
    async fn close(&self, plan_id: &str, principal_id: &PrincipalId, reason: String) -> Result<()> {
        PlanPort::close(&self.storage, plan_id, principal_id, reason).await
    }
    async fn mark_node_status(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        status: PlanNodeStatus,
    ) -> Result<PlanRecord> {
        PlanPort::mark_node_status(&self.storage, plan_id, principal_id, node_id, status).await
    }
    async fn set_node_evidence(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        evidence: NodeEvidence,
    ) -> Result<PlanRecord> {
        PlanPort::set_node_evidence(&self.storage, plan_id, principal_id, node_id, evidence).await
    }
    async fn add_node(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node: PlanNode,
    ) -> Result<PlanRecord> {
        PlanPort::add_node(&self.storage, plan_id, principal_id, node).await
    }
}

mod tool;
pub use tool::PlanTool;
