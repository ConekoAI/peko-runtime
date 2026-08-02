//! Phase F2 foldback: `CompletionEvent`, `SteeringMessage`, `ApprovalEvent`,
//! and `InboxItem` moved here from `peko-extension-host::inbox` (deleted).
//! Engine depends on `peko-extension-api` but not on root; these types are
//! needed in `peko_engine::async_completion` and were previously reachable
//! via the deleted `peko-extension-host` sat.
//!
//! The implementation stays in root at
//! `crate::extensions::framework::inbox::SessionInbox` (under foldback).
//! This file only carries the data types + the `From` conversions
//! between native `InboxItem` values and the API crate's envelope
//! forms (`AsyncInboxItem::Completion(CompletionEnvelope)` /
//! `::Steering(SteeringEnvelope)` /
//! `::Approval(ApprovalEnvelope)`).

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Event pushed to the inbox when an async task reaches a terminal
/// state. The agentic loop drains these at iteration start and
/// synthesizes a single user-role message containing all of them.
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub task_id: String,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: crate::AsyncTaskStatus,
    pub completed_at: DateTime<Utc>,
    pub output_path: std::path::PathBuf,
    pub parent_session_key: String,
}

/// User-supplied message queued for delivery to a session at the
/// start of the next agentic loop iteration.
#[derive(Debug, Clone)]
pub struct SteeringMessage {
    pub id: Uuid,
    pub content: String,
    pub queued_at: DateTime<Utc>,
}

impl SteeringMessage {
    /// Construct a steering message with a freshly generated id and
    /// the current UTC timestamp.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            content: content.into(),
            queued_at: Utc::now(),
        }
    }
}

/// A decision on a pending self-modification request (ADR-045
/// PR #4). Pushed by the daemon's `ApprovalHandler` after the user
/// runs `peko pending decide --grant|--deny`.
///
/// `parent_session_key` is the principal_id of the principal that
/// originated the request — the same key the agentic loop uses to
/// filter inbox drains, so the decision lands back on the session
/// that originally called `peko_self`.
#[derive(Debug, Clone)]
pub struct ApprovalEvent {
    pub request_id: Uuid,
    /// Short, stable label of the op (e.g. `"GrantCapability:fs:read"`).
    pub op_label: String,
    pub decision: crate::ApprovalDecision,
    /// Per-op result payload. `serde_json::Value::Null` on a deny /
    /// NotImplementedYet stub.
    pub op_result: serde_json::Value,
    pub decided_at: DateTime<Utc>,
    pub parent_session_key: String,
}

impl ApprovalEvent {
    /// Construct an ApprovalEvent with a freshly-generated `request_id`
    /// (the caller passes the queue-assigned UUID, not the constructor).
    #[must_use]
    pub fn new(
        request_id: Uuid,
        op_label: impl Into<String>,
        decision: crate::ApprovalDecision,
        op_result: serde_json::Value,
        parent_session_key: impl Into<String>,
    ) -> Self {
        Self {
            request_id,
            op_label: op_label.into(),
            decision,
            op_result,
            decided_at: Utc::now(),
            parent_session_key: parent_session_key.into(),
        }
    }
}

/// Item carried in a session inbox. Either a user steering message,
/// a completion event from a background async task, or an approval
/// decision from the self-modification gate.
#[derive(Debug, Clone)]
pub enum InboxItem {
    Steering(SteeringMessage),
    Completion(CompletionEvent),
    Approval(ApprovalEvent),
}

impl From<CompletionEvent> for InboxItem {
    fn from(e: CompletionEvent) -> Self {
        InboxItem::Completion(e)
    }
}

impl From<SteeringMessage> for InboxItem {
    fn from(m: SteeringMessage) -> Self {
        InboxItem::Steering(m)
    }
}

impl From<ApprovalEvent> for InboxItem {
    fn from(a: ApprovalEvent) -> Self {
        InboxItem::Approval(a)
    }
}

impl From<CompletionEvent> for crate::CompletionEnvelope {
    fn from(e: CompletionEvent) -> Self {
        crate::CompletionEnvelope {
            task_id: e.task_id,
            tool_name: e.tool_name,
            result: e.result,
            status: e.status,
            completed_at: e.completed_at,
            output_path: e.output_path,
            parent_session_key: e.parent_session_key,
        }
    }
}

impl From<SteeringMessage> for crate::SteeringEnvelope {
    fn from(m: SteeringMessage) -> Self {
        crate::SteeringEnvelope {
            id: m.id,
            content: m.content,
            queued_at: m.queued_at,
        }
    }
}

impl From<ApprovalEvent> for crate::ApprovalEnvelope {
    fn from(a: ApprovalEvent) -> Self {
        crate::ApprovalEnvelope {
            request_id: a.request_id,
            op_label: a.op_label,
            decision: a.decision,
            op_result: a.op_result,
            decided_at: a.decided_at,
            parent_session_key: a.parent_session_key,
        }
    }
}

impl From<CompletionEvent> for crate::AsyncInboxItem {
    fn from(e: CompletionEvent) -> Self {
        crate::AsyncInboxItem::Completion(crate::CompletionEnvelope::from(e))
    }
}

impl From<SteeringMessage> for crate::AsyncInboxItem {
    fn from(m: SteeringMessage) -> Self {
        crate::AsyncInboxItem::Steering(crate::SteeringEnvelope::from(m))
    }
}

impl From<ApprovalEvent> for crate::AsyncInboxItem {
    fn from(a: ApprovalEvent) -> Self {
        crate::AsyncInboxItem::Approval(crate::ApprovalEnvelope::from(a))
    }
}

impl From<InboxItem> for crate::AsyncInboxItem {
    fn from(item: InboxItem) -> Self {
        match item {
            InboxItem::Completion(e) => e.into(),
            InboxItem::Steering(m) => m.into(),
            InboxItem::Approval(a) => a.into(),
        }
    }
}
