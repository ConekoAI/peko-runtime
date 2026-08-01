//! Principal-harness-facing port trait for plan DAG storage.
//!
//! [`PlanPort`] is the boundary `peko-rs/core` consumes via
//! `Arc<dyn PlanPort>` (held on the [`Principal`] struct, alongside
//! `memory: Arc<dyn PrincipalMemory>`). The impl is local — both the
//! trait and the concrete [`PlanStorage`] type live in this crate —
//! so the orphan rule is satisfied trivially.
//!
//! # No closures on the trait
//!
//! Closures don't appear on the trait surface, matching the convention
//! of every other port trait in this codebase: [`ProviderView`],
//! [`SessionView`], [`ToolFunnel`], [`AsyncInboxLike`], [`AgentView`],
//! [`BackgroundCompactorFactory`]. The closure-form
//! `PlanStorage::update<F>` lives on the concrete struct for
//! internal use within peko-plan only.
//!
//! [`Principal`]: ../../../../core/src/principal/mod.rs
//! [`ProviderView`]: ../../../../providers/src/provider_view.rs
//! [`SessionView`]: ../../../../session/src/session_core.rs
//! [`ToolFunnel`]: ../../../../extension-api/src/tool_funnel.rs
//! [`AsyncInboxLike`]: ../../../../extension-api/src/async_inbox.rs

use crate::error::{PlanError, Result};
use crate::schema::{NodeEvidence, NodeId, PlanNode, PlanNodeStatus, PlanRecord};
use crate::storage::PlanStorage;
use async_trait::async_trait;
use chrono::Utc;
use peko_subject::PrincipalId;

/// Port trait for the principal harness to reach plan DAG storage.
///
/// `peko-rs/core` holds an `Arc<dyn PlanPort>` on each [`Principal`].
/// The concrete type is [`PlanStorage`] (file-backed JSONL-per-plan).
/// Future impls — in-memory for tests, network-backed for distributed
/// deployments — slot in without rewriting field types.
///
/// ## Method set
///
/// **Reads** (mirror [`PlanStorage`]):
/// - [`get`](Self::get) — fetch by id; `Ok(None)` for absent
/// - [`get_for_principal`](Self::get_for_principal) — fetch by id, asserting the principal matches
/// - [`list_for_principal`](Self::list_for_principal) — all plans owned by a principal
/// - [`current_focus`](Self::current_focus) — most-recently-updated open plan with `InProgress` nodes
/// - [`load_resumable`](Self::load_resumable) — open plans with unresolved nodes
///
/// **Plan-lifecycle writes**:
/// - [`create`](Self::create) — generate `plan_id`, write initial record
/// - [`close`](Self::close) — idempotent; second close returns `PlanError::AlreadyClosed`
///
/// **Node-level mutations** (discrete, what the future plan tool handler needs):
/// - [`mark_node_status`](Self::mark_node_status) — flip a node to a new status
/// - [`set_node_evidence`](Self::set_node_evidence) — record per-node evidence
/// - [`add_node`](Self::add_node) — append a node to an existing plan
///
/// [`Principal`]: ../../../../core/src/principal/mod.rs
#[async_trait]
pub trait PlanPort: Send + Sync + 'static {
    // ----- reads -----

    /// Read a plan by id. Returns `Ok(None)` if the file is absent.
    /// Returns [`PlanError::CorruptRecord`] if the file is present but
    /// not a valid [`PlanRecord`].
    async fn get(&self, plan_id: &str) -> Result<Option<PlanRecord>>;

    /// Read a plan by id, asserting the on-disk `principal_id` matches
    /// the supplied caller. Returns [`PlanError::PrincipalMismatch`]
    /// on mismatch; [`PlanError::NotFound`] on absent file.
    async fn get_for_principal(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
    ) -> Result<PlanRecord>;

    /// All plans owned by `principal_id`. Sort order is `created_at`
    /// ascending.
    async fn list_for_principal(&self, principal_id: &PrincipalId) -> Result<Vec<PlanRecord>>;

    /// The most-recently-updated open plan with at least one
    /// `InProgress` node. `None` when the principal has no active
    /// in-flight work.
    async fn current_focus(&self, principal_id: &PrincipalId) -> Result<Option<PlanRecord>>;

    /// All open plans with at least one unresolved node. This is the
    /// set the runtime re-injects into a fresh session's context
    /// (future PR #4).
    async fn load_resumable(&self, principal_id: &PrincipalId) -> Result<Vec<PlanRecord>>;

    // ----- plan-lifecycle writes -----

    /// Create a new plan. Generates `plan_id` and writes the initial
    /// record atomically.
    ///
    /// **Duplicate `node_id` check:** if `nodes` contains two or more
    /// entries sharing the same `node_id`, returns
    /// [`PlanError::InvalidNodeId`] without writing. Silently producing
    /// a record with shadowed / unreachable nodes would leave the LLM
    /// in an unrecoverable state — `mark_node_status` and
    /// `set_node_evidence` only see the first match, so subsequent
    /// operations on the duplicates would fail without explanation.
    /// Hard-erroring here surfaces the collision to the agent so it
    /// can correct its prompt and retry.
    async fn create(
        &self,
        principal_id: PrincipalId,
        title: String,
        nodes: Vec<PlanNode>,
    ) -> Result<PlanRecord>;

    /// Idempotent close. The second concurrent close returns
    /// [`PlanError::AlreadyClosed`].
    async fn close(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        reason: String,
    ) -> Result<()>;

    // ----- node-level mutations -----

    /// Flip a node's status. Stamps the node's `updated_at`; if the
    /// new status is `Blocked`, mirrors the reason into the parallel
    /// `blocked_reason` field for cheap UI access. Returns
    /// [`PlanError::InvalidNodeId`] if the node isn't present.
    async fn mark_node_status(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        status: PlanNodeStatus,
    ) -> Result<PlanRecord>;

    /// Record per-node evidence. Stamps the node's `updated_at`.
    /// Returns [`PlanError::InvalidNodeId`] if the node isn't present.
    async fn set_node_evidence(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        evidence: NodeEvidence,
    ) -> Result<PlanRecord>;

    /// Append a new node to an existing plan. Returns
    /// [`PlanError::AlreadyClosed`] if the plan has been closed.
    ///
    /// **Idempotent on `node_id` collision:** if the supplied
    /// `node.node_id` is already present in the plan, returns the
    /// existing record unchanged (no error, no overwrite). The LLM
    /// mental model is "I want this step in the plan" — if it's
    /// already there, that's a success. New `step` text on a colliding
    /// id is silently dropped (existing wins).
    async fn add_node(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node: PlanNode,
    ) -> Result<PlanRecord>;
}

// ---------------------------------------------------------------------------
// impl for the file-backed PlanStorage
// ---------------------------------------------------------------------------

#[async_trait]
impl PlanPort for PlanStorage {
    async fn get(&self, plan_id: &str) -> Result<Option<PlanRecord>> {
        PlanStorage::get(self, plan_id).await
    }

    async fn get_for_principal(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
    ) -> Result<PlanRecord> {
        PlanStorage::get_for_principal(self, plan_id, principal_id).await
    }

    async fn list_for_principal(&self, principal_id: &PrincipalId) -> Result<Vec<PlanRecord>> {
        PlanStorage::list_for_principal(self, principal_id).await
    }

    async fn current_focus(&self, principal_id: &PrincipalId) -> Result<Option<PlanRecord>> {
        PlanStorage::current_focus(self, principal_id).await
    }

    async fn load_resumable(&self, principal_id: &PrincipalId) -> Result<Vec<PlanRecord>> {
        PlanStorage::load_resumable(self, principal_id).await
    }

    async fn create(
        &self,
        principal_id: PrincipalId,
        title: String,
        nodes: Vec<PlanNode>,
    ) -> Result<PlanRecord> {
        PlanStorage::create(self, principal_id, title, nodes).await
    }

    async fn close(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        reason: String,
    ) -> Result<()> {
        PlanStorage::close(self, plan_id, principal_id, reason).await
    }

    async fn mark_node_status(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        status: PlanNodeStatus,
    ) -> Result<PlanRecord> {
        let node_id_str = node_id.as_str().to_string();
        self.update(plan_id, principal_id, move |r| {
            let mut updated = r.clone();
            let node = updated
                .nodes
                .iter_mut()
                .find(|n| n.node_id == *node_id)
                .ok_or_else(|| {
                    PlanError::InvalidNodeId(format!(
                        "node {node_id_str} not found in plan {}",
                        updated.plan_id
                    ))
                })?;
            node.status = status;
            node.updated_at = Utc::now();
            // Mirror Blocked into blocked_reason for UI consumers. The
            // cheap parallel field on `PlanNode` mirrors the structured
            // payload on `PlanNodeStatus::Blocked`; we keep them in
            // sync at the only mutation site that produces Blocked.
            node.blocked_reason = match &node.status {
                PlanNodeStatus::Blocked { reason, .. } => Some(reason.clone()),
                _ => None,
            };
            Ok(updated)
        })
        .await
    }

    async fn set_node_evidence(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node_id: &NodeId,
        evidence: NodeEvidence,
    ) -> Result<PlanRecord> {
        let node_id_str = node_id.as_str().to_string();
        self.update(plan_id, principal_id, move |r| {
            let mut updated = r.clone();
            let node = updated
                .nodes
                .iter_mut()
                .find(|n| n.node_id == *node_id)
                .ok_or_else(|| {
                    PlanError::InvalidNodeId(format!(
                        "node {node_id_str} not found in plan {}",
                        updated.plan_id
                    ))
                })?;
            node.evidence = Some(evidence);
            node.updated_at = Utc::now();
            Ok(updated)
        })
        .await
    }

    async fn add_node(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        node: PlanNode,
    ) -> Result<PlanRecord> {
        let new_node_id = node.node_id.clone();
        self.update(plan_id, principal_id, move |r| {
            let updated = r.clone();
            if updated.closed.is_some() {
                return Err(PlanError::AlreadyClosed);
            }
            // Idempotent on `node_id` collision: if the step is already
            // in the plan, return the existing record unchanged. The
            // LLM wanted the step in the plan, and it is — supplying
            // a colliding `nodeId` on retry (or auto-assigned id that
            // happens to land on an existing node) is a normal
            // event, not an error. The LLM-facing tool surfaces this
            // record so the agent can observe the step text didn't
            // change. New `step` text on a colliding id is silently
            // dropped — the existing node wins, by design.
            if updated
                .nodes
                .iter()
                .any(|n| n.node_id == new_node_id)
            {
                return Ok(updated);
            }
            let mut appended = updated;
            appended.nodes.push(node);
            Ok(appended)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests — exercise every port method through `Arc<dyn PlanPort>` to
// prove trait dispatch works (not just the concrete struct).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::PlanNodeStatus;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Build a sample node with a fresh id and the supplied status.
    fn sample_node(step: &str, status: PlanNodeStatus) -> PlanNode {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        PlanNode {
            node_id: NodeId::generate(),
            step: step.to_string(),
            status,
            depends_on: Vec::new(),
            evidence: None,
            blocked_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn sample_pending(step: &str) -> PlanNode {
        sample_node(step, PlanNodeStatus::Pending)
    }

    /// Wrap a fresh `PlanStorage` in `Arc<dyn PlanPort>` with the
    /// plans dir created (so subsequent calls don't race on mkdir).
    async fn port_in_tempdir() -> (TempDir, Arc<dyn PlanPort>) {
        let tmp = TempDir::new().expect("tempdir");
        let storage = PlanStorage::new(tmp.path().join("plans"));
        tokio::fs::create_dir_all(storage.plans_dir())
            .await
            .expect("mkdir");
        let port: Arc<dyn PlanPort> = Arc::new(storage);
        (tmp, port)
    }

    #[tokio::test]
    async fn port_get_returns_some_for_existing_plan() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let created = port
            .create(p.clone(), "ship v2".into(), vec![sample_pending("a")])
            .await
            .expect("create");
        let fetched = port.get(&created.plan_id).await.expect("get");
        assert_eq!(fetched, Some(created));
    }

    #[tokio::test]
    async fn port_get_returns_none_for_missing() {
        let (_tmp, port) = port_in_tempdir().await;
        let fetched = port.get("plan_doesnotexist").await.expect("get");
        assert_eq!(fetched, None);
    }

    #[tokio::test]
    async fn port_get_for_principal_mismatch_errors() {
        let (_tmp, port) = port_in_tempdir().await;
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();
        let created = port
            .create(p1.clone(), "p1 only".into(), vec![sample_pending("a")])
            .await
            .expect("create");
        let err = port
            .get_for_principal(&created.plan_id, &p2)
            .await
            .expect_err("must reject");
        assert!(matches!(err, PlanError::PrincipalMismatch { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn port_list_for_principal_filters() {
        let (_tmp, port) = port_in_tempdir().await;
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();
        port.create(p1.clone(), "p1".into(), vec![sample_pending("a")])
            .await
            .expect("c1");
        port.create(p1.clone(), "p1-2".into(), vec![sample_pending("b")])
            .await
            .expect("c2");
        port.create(p2.clone(), "p2".into(), vec![sample_pending("c")])
            .await
            .expect("c3");
        let p1_list = port.list_for_principal(&p1).await.expect("list p1");
        let p2_list = port.list_for_principal(&p2).await.expect("list p2");
        assert_eq!(p1_list.len(), 2);
        assert_eq!(p2_list.len(), 1);
        assert!(p1_list.iter().all(|r| r.principal_id == p1));
        assert!(p2_list.iter().all(|r| r.principal_id == p2));
    }

    #[tokio::test]
    async fn port_current_focus_returns_in_progress_plan() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let node = sample_pending("only-step");
        let only = port
            .create(p.clone(), "focus".into(), vec![node.clone()])
            .await
            .expect("create");
        // Initially no focus (everything Pending).
        assert!(port.current_focus(&p).await.expect("cf").is_none());
        // Flip to InProgress and re-fetch — current_focus should see it.
        port.mark_node_status(&only.plan_id, &p, &node.node_id, PlanNodeStatus::InProgress)
            .await
            .expect("mark");
        let focus = port.current_focus(&p).await.expect("cf").expect("some");
        assert_eq!(focus.plan_id, only.plan_id);
    }

    #[tokio::test]
    async fn port_load_resumable_filters_open_with_unresolved() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let n1 = sample_pending("a");
        let n2 = sample_pending("b");
        let open = port
            .create(p.clone(), "open".into(), vec![n1.clone(), n2.clone()])
            .await
            .expect("create open");
        let done = port
            .create(p.clone(), "done".into(), vec![sample_pending("d")])
            .await
            .expect("create done");
        // Mark the second plan fully completed then close it.
        port.mark_node_status(
            &done.plan_id,
            &p,
            &done.nodes[0].node_id,
            PlanNodeStatus::Completed {
                completed_at: Utc::now(),
            },
        )
        .await
        .expect("mark done");
        port.close(&done.plan_id, &p, "all done".into())
            .await
            .expect("close");
        // Resume set: only the open plan.
        let resumable = port.load_resumable(&p).await.expect("resumable");
        assert_eq!(resumable.len(), 1);
        assert_eq!(resumable[0].plan_id, open.plan_id);
        // Sanity: open plan still has both nodes unresolved.
        assert!(resumable[0].has_unresolved_nodes());
    }

    /// Hard-error on duplicate initial node ids at the port layer
    /// (mirrors `storage::create_rejects_duplicate_initial_node_ids`).
    /// Complements the idempotent `add_node` collision semantics:
    /// `create` rejects; `add_node` accepts.
    #[tokio::test]
    async fn port_create_rejects_duplicate_node_ids() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let n1 = sample_pending("first");
        let mut n2 = sample_pending("second");
        n2.node_id = n1.node_id.clone();
        let err = port
            .create(p.clone(), "dupe-batch".into(), vec![n1, n2])
            .await
            .expect_err("duplicate initial node ids must surface as InvalidNodeId");
        assert!(matches!(err, PlanError::InvalidNodeId(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn port_create_then_close_round_trip() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let created = port
            .create(p.clone(), "t".into(), vec![sample_pending("a")])
            .await
            .expect("create");
        port.close(&created.plan_id, &p, "done".into())
            .await
            .expect("first close");
        let err = port
            .close(&created.plan_id, &p, "again".into())
            .await
            .expect_err("second close must error");
        assert!(matches!(err, PlanError::AlreadyClosed), "got {err:?}");
    }

    #[tokio::test]
    async fn port_mark_node_status_round_trip() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let node = sample_pending("a");
        let created = port
            .create(p.clone(), "t".into(), vec![node.clone()])
            .await
            .expect("create");
        let updated = port
            .mark_node_status(
                &created.plan_id,
                &p,
                &node.node_id,
                PlanNodeStatus::Blocked {
                    reason: "waiting on review".into(),
                    since: Utc::now(),
                },
            )
            .await
            .expect("mark");
        // Read back through `get_for_principal` (covers both the
        // dispatch path AND the persisted shape).
        let back = port
            .get_for_principal(&created.plan_id, &p)
            .await
            .expect("read back");
        assert_eq!(back.nodes[0].node_id, node.node_id);
        assert!(matches!(back.nodes[0].status, PlanNodeStatus::Blocked { .. }));
        // blocked_reason parallel field is mirrored.
        assert_eq!(
            back.nodes[0].blocked_reason.as_deref(),
            Some("waiting on review")
        );
        // updated_at was stamped.
        assert!(back.updated_at > created.updated_at);
        // Same-plan record is returned.
        assert_eq!(back.plan_id, updated.plan_id);
    }

    #[tokio::test]
    async fn port_set_node_evidence_round_trip() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let node = sample_pending("a");
        let created = port
            .create(p.clone(), "t".into(), vec![node.clone()])
            .await
            .expect("create");
        let evidence = NodeEvidence {
            output: "compiled cleanly".into(),
            artifacts: vec!["/tmp/build.log".into()],
            decided_by: None,
        };
        port.set_node_evidence(&created.plan_id, &p, &node.node_id, evidence.clone())
            .await
            .expect("set evidence");
        let back = port
            .get_for_principal(&created.plan_id, &p)
            .await
            .expect("read back");
        assert_eq!(back.nodes[0].evidence, Some(evidence));
    }

    #[tokio::test]
    async fn port_add_node_appends_and_rejects_closed_plan() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let created = port
            .create(p.clone(), "t".into(), vec![sample_pending("a")])
            .await
            .expect("create");
        let new_node = sample_pending("b");
        // Append succeeds while plan is open.
        port.add_node(&created.plan_id, &p, new_node.clone())
            .await
            .expect("add");
        let back = port
            .get_for_principal(&created.plan_id, &p)
            .await
            .expect("read");
        assert_eq!(back.nodes.len(), 2);
        assert!(back.nodes.iter().any(|n| n.node_id == new_node.node_id));
        // Closing the plan prevents further add_node calls. Duplicate-
        // collision behavior is covered by the dedicated idempotency
        // tests below — keeping this branch focused on closed-plan
        // rejection.
        port.close(&created.plan_id, &p, "done".into())
            .await
            .expect("close");
        let err = port
            .add_node(&created.plan_id, &p, new_node)
            .await
            .expect_err("closed-plan add must error");
        assert!(matches!(err, PlanError::AlreadyClosed), "got {err:?}");
    }

    /// Duplicate `node_id` on `add_node` is idempotent: returns the
    /// existing record unchanged rather than erroring. The LLM mental
    /// model is "I want this step in the plan" — if it's already there,
    /// that's a success, not a failure. (`PlanAddStep` tool relies on
    /// this so retries / auto-assigned id collisions don't surface as
    /// user-visible tool errors.)
    #[tokio::test]
    async fn port_add_node_collision_is_idempotent() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let created = port
            .create(p.clone(), "t".into(), vec![sample_pending("a")])
            .await
            .expect("create");
        let node = sample_pending("first");
        port.add_node(&created.plan_id, &p, node.clone())
            .await
            .expect("first add");
        // Re-attempt with the same `node_id` returns Ok and does NOT
        // push a duplicate row.
        let back = port
            .add_node(&created.plan_id, &p, node.clone())
            .await
            .expect("collision should be Ok, not Err");
        assert_eq!(
            back.nodes.len(),
            2,
            "collision must not push a duplicate row"
        );
        assert!(back.nodes.iter().any(|n| n.node_id == node.node_id));
    }

    /// When a colliding `nodeId` arrives with different `step` text,
    /// the existing node's `step` is preserved (existing wins). The
    /// plan's step text isn't silently mutated by a re-send that
    /// happens to land on a colliding id with different content.
    #[tokio::test]
    async fn port_add_node_collision_preserves_existing_step_text() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let created = port
            .create(p.clone(), "t".into(), vec![sample_pending("a")])
            .await
            .expect("create");
        let original = sample_pending("original-step-text");
        port.add_node(&created.plan_id, &p, original.clone())
            .await
            .expect("first add");
        // Build a colliding node with different `step` text.
        let mut colliding = sample_pending("DIFFERENT-step-text");
        colliding.node_id = original.node_id.clone();
        let back = port
            .add_node(&created.plan_id, &p, colliding)
            .await
            .expect("collision should be Ok");
        // Find the node by id — `step` text should be the original,
        // not the colliding payload.
        let kept = back
            .nodes
            .iter()
            .find(|n| n.node_id == original.node_id)
            .expect("node must still be present");
        assert_eq!(
            kept.step, original.step,
            "existing step wins on collision"
        );
    }

    #[tokio::test]
    async fn port_mark_node_status_unknown_node_errors() {
        let (_tmp, port) = port_in_tempdir().await;
        let p = PrincipalId::generate();
        let created = port
            .create(p.clone(), "t".into(), vec![sample_pending("a")])
            .await
            .expect("create");
        let phantom = NodeId::generate();
        let err = port
            .mark_node_status(&created.plan_id, &p, &phantom, PlanNodeStatus::InProgress)
            .await
            .expect_err("unknown node must error");
        assert!(matches!(err, PlanError::InvalidNodeId(_)), "got {err:?}");
    }
}