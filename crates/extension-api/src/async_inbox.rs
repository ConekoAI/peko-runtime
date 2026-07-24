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

use crate::AsyncTaskStatus;

/// One inbox item yielded by [`AsyncInboxLike::drain_all`].
///
/// Mirrors `peko_extension_host::InboxItem`'s two relevant variants.
/// Other variants (`Provider`, `ExtensionSignal`) are kept
/// host-side; the agentic loop only ever sees `Completion` and
/// `Steering`.
#[derive(Debug, Clone)]
pub enum AsyncInboxItem {
    /// A completed async task (returned by `AsyncSpawnTool`).
    Completion(CompletionEnvelope),
    /// A steering message pushed by an extension or runtime.
    Steering(SteeringEnvelope),
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

/// Narrow view of a per-session async inbox.
///
/// Implementors must be `Send + Sync` so the loop can hold
/// `Arc<dyn AsyncInboxLike>` across `.await` points.
///
/// The trait exposes exactly the surface the loop needs: drain
/// everything in one batch, once per iteration. Drain-order
/// preservation is the implementor's responsibility (FIFO insertion
/// order is the host's contract).
#[async_trait::async_trait]
pub trait AsyncInboxLike: Send + Sync + 'static {
    /// Drain all pending items. Called once per agentic-loop
    /// iteration; events arriving mid-iteration wait for the next
    /// one.
    async fn drain_all(&self) -> Vec<AsyncInboxItem>;
}
