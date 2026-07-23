//! Compatibility shim: implements `peko_engine::AsyncInboxLike` for
//! root's `Arc<peko_extension_host::SessionInbox>` so the lifted
//! `AgenticLoop` (Phase 9b.N.5b,
//! `crates/engine/src/agentic_loop.rs`) can drain async completions +
//! steering messages without holding a direct borrow of
//! `peko_extension_host::SessionInbox`.
//!
//! # Why a wrapper?
//!
//! `peko_extension_host::SessionInbox` is a foreign type (lives in
//! the workspace crate, not in root), and `AsyncInboxLike` is a
//! foreign trait (lives in `peko_engine`). Rust's orphan rule forbids
//! `impl ForeignTrait for ForeignType` in root. The wrapper struct
//! `AsyncInboxAdapter` is root-local so the impl is legal here.
//!
//! The orphan-rule pressure also prevents moving the impl into
//! `peko_extension_host` directly: that crate doesn't depend on
//! `peko_engine`, so it can't see the `AsyncInboxLike` trait. Phase
//! 16 deletes this shim once the trait ownership settles (either the
//! trait moves into `peko_extension_host`, or `peko_engine` adds a
//! blanket `Arc<T>` impl and `peko_extension_host` implements the
//! inner trait — see Phase 16's plan).
//!
//! # Phase 2 simplification
//!
//! Pre-Phase-2, root carried a field-identical clone of
//! `peko_extension_host::{CompletionEvent, SteeringMessage, InboxItem}`.
//! The wrapper's `drain_all` body had to convert field-by-field from
//! root's `InboxItem` to `peko_engine::AsyncInboxItem`. After Phase
//! 2, root's `InboxItem` IS `peko_extension_host::InboxItem` (a
//! single canonical type); the conversion becomes a direct pattern
//! match — no field copy.
//!
//! # Trait port rationale
//!
//! `AsyncInboxLike` (defined in `peko_engine::async_inbox`) is a
//! narrow 1-method trait port: `drain_all() -> Vec<AsyncInboxItem>`.
//! The loop pattern-matches the two relevant variants:
//!
//! - `InboxItem::Completion(e)` → completion events consumed by
//!   `build_async_completion_message` (which already takes the
//!   `AsyncCompletionLike` trait; `peko_extension_host::CompletionEvent`
//!   implements it in `crates/engine/src/async_completion.rs`).
//! - `InboxItem::Steering(m)` → pushed onto the message stream verbatim.

use std::sync::Arc;

use peko_engine::{AsyncInboxItem, AsyncInboxLike};
use peko_extension_host::{InboxItem, SessionInbox};

/// Root-side adapter that converts the host-owned `Arc<SessionInbox>`
/// into the engine-facing `AsyncInboxLike` trait. The loop stores
/// `Arc<dyn AsyncInboxLike + ...>` and never sees `SessionInbox`
/// directly.
pub struct AsyncInboxAdapter {
    inner: Arc<SessionInbox>,
}

impl AsyncInboxAdapter {
    /// Wrap an `Arc<peko_extension_host::SessionInbox>` so the lifted
    /// `AgenticLoop` can consume it through the `AsyncInboxLike`
    /// trait port.
    #[must_use]
    pub fn new(inner: Arc<SessionInbox>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl AsyncInboxLike for AsyncInboxAdapter {
    async fn drain_all(&self) -> Vec<AsyncInboxItem> {
        // Phase 2 simplification: `peko_extension_host::InboxItem` is
        // the single canonical item type. The two relevant variants
        // map 1:1 onto `peko_engine::AsyncInboxItem` (which itself
        // holds `peko_extension_host::CompletionEvent` /
        // `peko_extension_host::SteeringMessage`). The previous
        // field-by-field conversion went away because the structs are
        // now the same type.
        self.inner
            .drain_all()
            .await
            .into_iter()
            .map(|item| match item {
                InboxItem::Completion(e) => AsyncInboxItem::Completion(e),
                InboxItem::Steering(m) => AsyncInboxItem::Steering(m),
            })
            .collect()
    }
}
