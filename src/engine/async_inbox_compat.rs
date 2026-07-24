//! Compatibility shim: implements `peko_engine::AsyncInboxLike` for
//! root's `Arc<peko_extension_host::SessionInbox>` so the lifted
//! `AgenticLoop` (Phase 9b.N.5b,
//! `crates/engine/src/agentic_loop.rs`) can drain async completions +
//! steering messages without holding a direct borrow of
//! `peko_extension_host::SessionInbox`.
//!
//! # Why a wrapper?
//!
//! Phase 7 promoted `AsyncInboxLike` into the `peko-extension-api`
//! crate so `peko-session` can hold `Arc<dyn AsyncInboxLike>`
//! without importing `peko-extension-host`. The trait is now
//! foreign to root (lives in `peko-extension-api`), and
//! `SessionInbox` is foreign to root too. Rust's orphan rule
//! forbids `impl ForeignTrait for ForeignType` in root.
//!
//! Note: `peko-extension-host::SessionInbox` already implements
//! `AsyncInboxLike` directly (in `crates/extension-host/src/inbox.rs`).
//! `Arc<SessionInbox>` therefore coerces to `Arc<dyn AsyncInboxLike>`
//! without needing this wrapper. The wrapper exists only to preserve
//! the historical `AsyncInboxAdapter::new(...)` constructor at the
//! three call sites (`src/agents/agent.rs:1578` + two test sites in
//! `src/engine/agentic_loop_compat.rs`) — Phase 16 deletes this
//! shim once callers migrate to direct coercion.
//!
//! # Phase 7 envelope conversion
//!
//! Post-Phase-7, `AsyncInboxItem` carries
//! `peko_extension_api::{CompletionEnvelope, SteeringEnvelope}` (not
//! the host's `CompletionEvent` / `SteeringMessage`). The wrapper
//! constructs the envelopes at the trait impl boundary; the agentic
//! loop downstream sees only envelope forms.

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
        // Phase 7 envelope conversion: `peko_extension_host::InboxItem`
        // holds native `CompletionEvent` / `SteeringMessage`; the API
        // crate's `AsyncInboxItem` holds envelope forms. Wrap each at
        // the trait impl boundary.
        self.inner
            .drain_all()
            .await
            .into_iter()
            .map(|item| match item {
                InboxItem::Completion(e) => {
                    AsyncInboxItem::Completion(peko_extension_api::CompletionEnvelope {
                        task_id: e.task_id,
                        tool_name: e.tool_name,
                        result: e.result,
                        status: e.status,
                        completed_at: e.completed_at,
                        output_path: e.output_path,
                        parent_session_key: e.parent_session_key,
                    })
                }
                InboxItem::Steering(m) => {
                    AsyncInboxItem::Steering(peko_extension_api::SteeringEnvelope {
                        id: m.id,
                        content: m.content,
                        queued_at: m.queued_at,
                    })
                }
            })
            .collect()
    }
}
