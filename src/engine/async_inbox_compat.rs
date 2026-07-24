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

/// Root-side adapter that converts the host-owned `Arc<SessionInbox>`
/// into the engine-facing `AsyncInboxLike` trait. The loop stores
/// `Arc<dyn AsyncInboxLike + ...>` and never sees `SessionInbox`
/// directly.
pub struct AsyncInboxAdapter {
    inner: Arc<dyn AsyncInboxLike>,
}

impl AsyncInboxAdapter {
    /// Wrap any `Arc<dyn AsyncInboxLike>` so the lifted `AgenticLoop`
    /// can consume it through the engine's `AsyncInboxLike` trait
    /// port. Phase 7 widened the accepted type from `Arc<SessionInbox>`
    /// to the trait object so callers can hand in either a concrete
    /// inbox (via direct coercion from `Arc<SessionInbox>`) or the
    /// trait object they already received from the daemon-global
    /// `InboxRegistry::get_or_create`.
    #[must_use]
    pub fn new(inner: Arc<dyn AsyncInboxLike>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl AsyncInboxLike for AsyncInboxAdapter {
    async fn drain_all(&self) -> Vec<AsyncInboxItem> {
        // Delegate to the trait object. `SessionInbox` already
        // implements `AsyncInboxLike` in `peko-extension-host` and
        // converts its native `InboxItem` to envelope forms at the
        // impl boundary; we just pass through.
        self.inner.drain_all().await
    }
}
