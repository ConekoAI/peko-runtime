//! Compatibility shim: implements `peko_engine::AsyncInboxLike` for
//! root's `SharedSessionInbox` (= `Arc<SessionInbox>`) so the lifted
//! `AgenticLoop` (Phase 9b.N.5b,
//! `crates/engine/src/agentic_loop.rs`) can drain async completions +
//! steering messages without holding a direct borrow of root's
//! `SessionInbox` (487 lines).
//!
//! # Trait port rationale
//!
//! `AsyncInboxLike` (defined in `peko_engine::async_inbox`) is a
//! narrow 1-method trait port: `drain_all() -> Vec<AsyncInboxItem>`.
//!
//! The loop pattern-matches the two relevant `InboxItem` variants:
//!
//! - `InboxItem::Completion(e)` → completion events consumed by
//!   `build_async_completion_message` (which already takes the
//!   `AsyncCompletionLike` trait).
//! - `InboxItem::Steering(m)` → pushed onto the message stream verbatim.
//!
//! Other `InboxItem` variants are kept root-side; the loop never
//! sees them. If a future variant is needed, it goes into
//! `AsyncInboxItem` in `peko-engine` first, then this shim maps the
//! root variant.
//!
//! # Orphan-rule-friendly form
//!
//! `SharedSessionInbox` is `Arc<SessionInbox>` — a foreign type from
//! the orphan-rule perspective. We can't `impl AsyncInboxLike for
//! Arc<SessionInbox>` in `peko-engine` (the orphan rule forbids
//! `impl ForeignTrait for ForeignType<T>` where neither is local).
//!
//! But the impl needs to live in root because the conversion
//! `SessionInbox → AsyncInboxItem` reads `InboxItem` (root-only).
//!
//! Two options:
//!
//! 1. **Move `AsyncInboxItem` mapping into a free fn** — the loop
//!    drains a `Vec<AsyncInboxItem>` and root pre-maps. Cleanest but
//!    requires the loop to see the `Vec<InboxItem>` shape.
//! 2. **Implement on a concrete wrapper struct** — root owns the
//!    wrapper. Same shape as the `SessionCore for Session` blanket in
//!    [[workspace-phase9b-n3-executor]].
//!
//! This file uses option 1 + a thin wrapper: we wrap
//! `SharedSessionInbox` in a local `SharedSessionInboxAdapter` newtype
//! in `src/engine/async_inbox_compat.rs`, and impl `AsyncInboxLike`
//! for it. The loop stores `Arc<SharedSessionInboxAdapter>` (or the
//! trait object) and never sees `SessionInbox`.
//!
//! Module location: rooted at `src/engine/async_inbox_compat.rs` so
//! `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/agent_view_compat.rs` (Phase 9b.N.5a),
//! `src/engine/session_view_compat.rs` (Phase 9b.N.3), and
//! `src/engine/compaction_backend_compat.rs` (Phase 9b.N.4) patterns.

use crate::extensions::framework::async_exec::executor::completion_queue::{
    InboxItem, SharedSessionInbox,
};
use peko_engine::{AsyncInboxItem, AsyncInboxLike};

/// Root-side adapter that converts the root-owned `SharedSessionInbox`
/// into the engine-facing `AsyncInboxLike` trait. The loop stores
/// `Arc<dyn AsyncInboxLike + ...>` (or `Box<dyn AsyncInboxLike>`) and
/// never sees `InboxItem` / `SessionInbox`.
pub struct AsyncInboxAdapter {
    inner: SharedSessionInbox,
}

impl AsyncInboxAdapter {
    /// Wrap a `SharedSessionInbox` so the lifted `AgenticLoop` can
    /// consume it through the `AsyncInboxLike` trait port.
    #[must_use]
    pub fn new(inner: SharedSessionInbox) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl AsyncInboxLike for AsyncInboxAdapter {
    async fn drain_all(&self) -> Vec<AsyncInboxItem> {
        let items = self.inner.drain_all().await;
        items
            .into_iter()
            .filter_map(|item| match item {
                // `completion_queue::CompletionEvent` (root) and
                // `peko_extension_host::CompletionEvent` (workspace
                // crate) have identical field shapes — both are
                // `{ task_id, tool_name, result, status,
                // completed_at, output_path, parent_session_key }`
                // — but they're distinct types because
                // `completion_queue.rs` was never lifted into
                // `peko-extension-host` (deferred per Phase 8's
                // scope-down; the bulk move is gated on Phase 11
                // protocol extraction). Convert field-by-field so the
                // trait port stays clean.
                InboxItem::Completion(e) => Some(AsyncInboxItem::Completion(
                    peko_extension_host::CompletionEvent {
                        task_id: e.task_id,
                        tool_name: e.tool_name,
                        result: e.result,
                        status: e.status,
                        completed_at: e.completed_at,
                        output_path: e.output_path,
                        parent_session_key: e.parent_session_key,
                    },
                )),
                // `completion_queue::SteeringMessage` (root) and
                // `peko_extension_host::SteeringMessage` (workspace
                // crate) have identical shapes — both are
                // `{ id: Uuid, content: String, queued_at:
                // DateTime<Utc> }` — but they're distinct types
                // because `completion_queue.rs` was never lifted into
                // `peko-extension-host`. Convert field-by-field.
                InboxItem::Steering(m) => Some(AsyncInboxItem::Steering(
                    peko_extension_host::SteeringMessage {
                        id: m.id,
                        content: m.content,
                        queued_at: m.queued_at,
                    },
                )),
                // Future variants are silently dropped — the loop never
                // sees them today. If a future variant is needed, add it
                // to `AsyncInboxItem` in `peko_engine::async_inbox` first,
                // then map it here.
            })
            .collect()
    }
}
