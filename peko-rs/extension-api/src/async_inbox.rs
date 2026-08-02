//! `AsyncInboxLike` — narrow trait port for the agentic loop's async
//! inbox, plus the envelope types it carries.
//!
//! Phase 7 promotes this from a `peko-engine` definition to the
//! `peko-extension-api` crate so that `peko-session` (which owns
//! the daemon-global `InboxRegistry`) can hold
//! `Arc<dyn AsyncInboxLike>` without importing either
//! `peko-engine` (a forbidden direction) or
//! `peko-extension-host` (a forbidden direction). The host's
//! concrete [`SessionInbox`](peko_extension_host::SessionInbox)
//! implements this trait by converting its native
//! [`CompletionEvent`](peko_extension_host::CompletionEvent) /
//! [`SteeringMessage`](peko_extension_host::SteeringMessage) values
//! into the envelopes defined here.
//!
//! The engine's `agentic_loop.rs` consumes `AsyncInboxItem`s
//! through this trait; the conversion to envelope form is invisible
//! to it. The envelopes mirror the host types' fields so the loop's
//! downstream message synthesis keeps working without changes.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AsyncTaskStatus;

/// One inbox item yielded by [`AsyncInboxLike::drain_all`].
///
/// Mirrors `peko_extension_host::InboxItem`'s three relevant variants.
/// Other variants (`Provider`, `ExtensionSignal`) are kept
/// host-side; the agentic loop only ever sees `Completion`,
/// `Steering`, and `Approval`.
#[derive(Debug, Clone)]
pub enum AsyncInboxItem {
    /// A completed async task (returned by `AsyncSpawnTool`).
    Completion(CompletionEnvelope),
    /// A steering message pushed by an extension or runtime.
    Steering(SteeringEnvelope),
    /// A decision on a pending self-modification request
    /// (ADR-045 PR #4). Pushed by the daemon's `ApprovalHandler`
    /// after the user runs `peko pending decide --grant|--deny`.
    Approval(ApprovalEnvelope),
}

/// Envelope form of a `peko_extension_host::CompletionEvent`.
///
/// Carries exactly the fields the agentic loop reads; the host's
/// richer struct is wrapped at the trait impl boundary so this API
/// crate does not depend on `peko-extension-host`.
#[derive(Debug, Clone)]
pub struct CompletionEnvelope {
    pub task_id: String,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: AsyncTaskStatus,
    pub completed_at: DateTime<Utc>,
    pub output_path: PathBuf,
    pub parent_session_key: String,
}

/// Envelope form of a `peko_extension_host::SteeringMessage`.
#[derive(Debug, Clone)]
pub struct SteeringEnvelope {
    pub id: uuid::Uuid,
    pub content: String,
    pub queued_at: DateTime<Utc>,
}

/// Envelope form of `peko_extension_host::ApprovalEvent` (ADR-045
/// PR #4). Carries the per-op result so the agent's next iteration
/// can render "Approval <uuid>: approved — granted fs:read" as a
/// user-role message and continue.
///
/// `parent_session_key` is the principal_id of the principal that
/// originated the request. The agentic loop filters on it the same
/// way it filters completion events.
#[derive(Debug, Clone)]
pub struct ApprovalEnvelope {
    pub request_id: uuid::Uuid,
    /// Short, stable label of the op (e.g. `"GrantCapability:fs:read"`).
    /// Mirrors `peko_core::daemon::api::SelfModifyOp::label()`.
    pub op_label: String,
    pub decision: ApprovalDecision,
    /// Per-op result. `Value::Null` on a deny / on a NotImplementedYet
    /// stub (the engine returns `ExecuteError::OpFailed` for those
    /// rather than an op_result payload).
    pub op_result: serde_json::Value,
    pub decided_at: DateTime<Utc>,
    pub parent_session_key: String,
}

/// Tagged enum mirror of `ApprovalDecisionPayload` (wire) and
/// `Decision` (engine-side). Kept distinct so this crate stays
/// independent of `peko_core::ipc::packet`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied { reason: String },
}

/// Narrow view of a per-session async inbox.
///
/// Implementors must be `Send + Sync` so the loop can hold
/// `Arc<dyn AsyncInboxLike>` across `.await` points.
///
/// The trait exposes the surface the loop needs: drain everything
/// in one batch, once per iteration. Drain-order preservation is
/// the implementor's responsibility (FIFO insertion order is the
/// host's contract). Producers (extension-host tasks, principal
/// send, etc.) push items through [`AsyncInboxLike::push`] — a
/// default no-op implementation lets test stubs opt out.
#[async_trait::async_trait]
pub trait AsyncInboxLike: Send + Sync + 'static {
    /// Drain all pending items. Called once per agentic-loop
    /// iteration; events arriving mid-iteration wait for the next
    /// one.
    async fn drain_all(&self) -> Vec<AsyncInboxItem>;

    /// Push an item into the inbox. Default is a no-op (test stubs
    /// don't need to retain pushed items). Real implementations
    /// (peko-extension-host's `SessionInbox`) override to append to
    /// their internal buffer.
    async fn push(&self, _item: AsyncInboxItem) {}

    /// Number of pending items waiting to be drained. Default is 0
    /// (test stubs don't track pending state). Real implementations
    /// override so producers / polling tests can observe non-empty
    /// inboxes without forcing a drain.
    async fn len(&self) -> usize {
        0
    }

    /// Convenience: `self.len() == 0`. Mirrors `Vec::is_empty`.
    async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}
