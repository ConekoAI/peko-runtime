//! Session inbox port and completion/steering envelopes.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use peko_tools_core::AsyncTaskStatus;

/// One inbox item yielded by [`AsyncInboxLike::drain_all`].
///
/// Mirrors `host::InboxItem`'s two relevant variants.
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

/// Envelope form of a `host::CompletionEvent`.
///
/// Carries exactly the fields the agentic loop reads; the host's
/// richer struct is wrapped at the trait impl boundary so this API
/// crate does not depend on `host async runtime`.
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

/// Envelope form of a `host::SteeringMessage`.
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
/// The trait exposes the surface the loop needs: drain everything
/// in one batch, once per iteration. Drain-order preservation is
/// the implementor's responsibility (FIFO insertion order is the
/// host's contract). Producers (background tasks, principal
/// send, etc.) push items through [`AsyncInboxLike::push`] — a
/// default no-op implementation lets test stubs opt out.
#[async_trait::async_trait]
pub trait AsyncInboxLike: Send + Sync + 'static {
    /// Drain all pending items. Called once per agentic-loop
    /// iteration; events arriving mid-iteration wait for the next
    /// one.
    async fn drain_all(&self) -> Vec<AsyncInboxItem>;

    /// Drain only the steering messages, leaving completion events
    /// queued. Used by the post-run steering drains so async-task
    /// completions are not silently destroyed (2026-09-27, P0-2).
    ///
    /// The default implementation drains everything and re-pushes
    /// completions — NOT atomic, and lossy for test stubs whose
    /// `push` is a no-op. The production `SessionInbox` overrides
    /// this with an atomic partition.
    async fn drain_steering(&self) -> Vec<SteeringEnvelope> {
        let items = self.drain_all().await;
        let mut steering = Vec::new();
        for item in items {
            match item {
                AsyncInboxItem::Steering(m) => steering.push(m),
                completion @ AsyncInboxItem::Completion(_) => self.push(completion).await,
            }
        }
        steering
    }

    /// Push an item into the inbox. Default is a no-op (test stubs
    /// don't need to retain pushed items). Real implementations
    /// (host async runtime's `SessionInbox`) override to append to
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
