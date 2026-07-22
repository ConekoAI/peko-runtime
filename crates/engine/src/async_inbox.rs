//! `AsyncInboxLike` — narrow trait port for the agentic-loop's async
//! inbox.
//!
//! Phase 9b.N.5a introduced this trait port to break
//! `agentic_loop.rs`'s direct borrow of
//! `crate::extensions::framework::async_exec::executor::SharedSessionInbox`
//! (487 lines + `Arc<SessionInbox>`). The loop only ever calls
//! `inbox.drain_all().await` once per iteration and pattern-matches the
//! returned `Vec<InboxItem>` into completions + steering. The trait
//! exposes exactly that surface.
//!
//! `AsyncInboxItem` is a small enum that mirrors the two relevant
//! `InboxItem` variants:
//!
//! - `Completion(CompletionEvent)` — completed async tasks, consumed by
//!   `build_async_completion_message` (which already takes
//!   `AsyncCompletionLike`, so `peko_extension_host::CompletionEvent`
//!   slots in directly).
//! - `Steering(SteeringMessage)` — runtime-pushed steering messages,
//!   pushed onto the message stream verbatim.
//!
//! Following the same transient-scaffolding pattern as
//! [[workspace-phase9b-n4-compaction]]'s `CompactorBackend` trait.
//! When the `SessionInbox` itself eventually lifts into
//! `peko-extension-host`, this trait disappears.

use peko_extension_host::{CompletionEvent, SteeringMessage};

/// One inbox item yielded by [`AsyncInboxLike::drain_all`].
///
/// Mirrors `crate::extensions::framework::async_exec::executor::completion_queue::InboxItem`
/// (the two relevant variants). Other variants are kept root-side;
/// the loop never sees them.
#[derive(Debug, Clone)]
pub enum AsyncInboxItem {
    /// A completed async task (returned by `AsyncSpawnTool`).
    Completion(CompletionEvent),
    /// A steering message pushed by an extension or runtime.
    Steering(SteeringMessage),
}

/// Narrow engine-facing view of root's `SharedSessionInbox`
/// (= `Arc<SessionInbox>`).
///
/// Implemented by `Arc<SessionInbox>` via
/// `src/engine/async_inbox_compat.rs` (orphan-rule-friendly — `Arc<T>`
/// blanket impl, matching the `SessionCore` blanket pattern in
/// [[workspace-phase9b-n3-executor]]).
#[async_trait::async_trait]
pub trait AsyncInboxLike: Send + Sync + 'static {
    /// Drain all pending items. Called once per agentic-loop iteration
    /// so events that arrive mid-iteration wait for the next one.
    async fn drain_all(&self) -> Vec<AsyncInboxItem>;
}
