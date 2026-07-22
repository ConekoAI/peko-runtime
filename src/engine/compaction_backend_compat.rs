//! Compatibility shim: implements `peko_engine::CompactorBackend` for
//! root's `BackgroundCompactor` so the lifted `CompactionOrchestrator`
//! (Phase 9b.N.4, `crates/engine/src/compaction_orchestrator.rs`)
//! can drive the background worker without holding a direct borrow of
//! root's `BackgroundCompactor` / `Compactor` / `Provider`.
//!
//! # Trait port rationale
//!
//! `CompactorBackend` (defined in `peko-engine::compaction::backend`)
//! is a narrow 2-method trait port:
//!
//! 1. `should_request(estimated_tokens, context_window, &config)` —
//!    forwards to `BackgroundCompactor::should_request` (root) which
//!    runs the dual-threshold check (`should_auto_compact`) AND the
//!    worker-side cooldown/quota state.
//!
//! 2. `request(CompactionRequest)` — forwards to
//!    `BackgroundCompactor::request_compaction` (root), wrapping the
//!    public-shape `CompactionRequest` (no `response_tx`) with an
//!    internal-shape `CompactionRequest` (carries the
//!    `oneshot::Sender<CompactionResponse>`) before forwarding through
//!    the worker's `mpsc` channel. Returns the receiver.
//!
//! The impl lives here (not in `peko-engine`) because of the orphan
//! rule: `peko_engine::CompactorBackend` is a foreign trait, and
//! `BackgroundCompactor` is a root-only type. The blanket `impl
//! CompactorBackend for BackgroundCompactor` form is allowed because
//! `BackgroundCompactor` is local to root (see the orphan rule's
//! "local type before any uncovered type parameter" clause).
//!
//! # Trait port lifetime
//!
//! The trait port mirrors the pattern established by Phase 9b.N.1
//! (`AsyncCompletionLike`), 9b.N.2 (`ToolFunnel`), and 9b.N.3
//! (`SessionView`). It disappears when a later phase lifts
//! `BackgroundCompactor` into `peko-engine` — only blocked by the
//! concrete `Provider` lift (deferred per Phase 6's note).
//!
//! Module location: rooted at `src/engine/compaction_backend_compat.rs`
//! so `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/session_view_compat.rs` and
//! `src/engine/extension_core_funnel_compat.rs` patterns.

use crate::session::compaction::background::BackgroundCompactor;
use anyhow::Result;
use peko_engine::compaction::{
    CompactionConfig, CompactionRequest as PublicRequest, CompactionResponse,
};
use peko_engine::CompactorBackend;
use tokio::sync::oneshot;

#[async_trait::async_trait]
impl CompactorBackend for BackgroundCompactor {
    async fn should_request(
        &self,
        estimated_tokens: usize,
        context_window: usize,
        config: &CompactionConfig,
    ) -> bool {
        BackgroundCompactor::should_request(self, estimated_tokens, context_window, config).await
    }

    async fn request(
        &self,
        request: PublicRequest,
    ) -> Result<oneshot::Receiver<CompactionResponse>> {
        // Forward the public-shape fields directly to root's
        // `BackgroundCompactor::request_compaction`, which creates
        // its own `(response_tx, response_rx)` oneshot pair and
        // returns the receiver. The trait port deliberately omits
        // `response_tx` so the lifted orchestrator doesn't have to
        // construct a sender it never uses — the worker hands the
        // receiver back via the returned `oneshot::Receiver`.
        BackgroundCompactor::request_compaction(self, request.messages, request.previous_summary)
            .await
    }
}
