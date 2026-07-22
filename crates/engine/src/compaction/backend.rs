//! `CompactorBackend` — narrow trait port so the lifted
//! `CompactionOrchestrator` (Phase 9b.N.4, `crates/engine/src/compaction_orchestrator.rs`)
//! can talk to the root-owned `BackgroundCompactor` without depending
//! on root.
//!
//! # Trait port rationale
//!
//! The orchestrator's pre-lift shape was:
//!
//! ```ignore
//! pub struct CompactionOrchestrator {
//!     background_compactor: BackgroundCompactor,
//!     ...
//! }
//! ```
//!
//! After the lift, root still owns `BackgroundCompactor` (it depends
//! on the concrete `crate::providers::Provider`, the `Compactor` LLM
//! summarization helper, and `crate::quota::QuotaScope`). The
//! orchestrator instead holds a `Box<dyn CompactorBackend>`, which
//! keeps the trait surface minimal and matches the trait-port
//! pattern established by Phase 9b.N.1 (`AsyncCompletionLike`),
//! 9b.N.2 (`ToolFunnel`), and 9b.N.3 (`SessionView`).
//!
//! # Methods
//!
//! - [`should_request`](CompactorBackend::should_request) — sync-ish
//!   check that gates whether to fire the background worker. Maps to
//!   `should_auto_compact(...)` (dual threshold: ratio-based +
//!   reserved headroom) AND the worker's own cooldown/quota state.
//!   Returning `true` here means "you may submit a request; the
//!   worker may still skip with [`CompactionResponse::Skipped`] if
//!   its internal state disallows it".
//!
//! - [`request`](CompactorBackend::request) — async submission that
//!   enqueues a [`CompactionRequest`] onto the worker's `mpsc` and
//!   returns a `oneshot::Receiver<CompactionResponse>`. The
//!   orchestrator polls this receiver with a 100ms timeout per
//!   loop iteration so the agentic loop never blocks on the
//!   summarization LLM call.
//!
//! Both methods are `async` (not just `&self`) for symmetry with the
//! orchestrator's pre-lift call sites — `should_request` reads the
//! worker's `Arc<Mutex<WorkerState>>` which may need an async lock
//! under contention (the worker takes the lock to update the
//! `is_compacting` flag), and `request` is naturally async because
//! the `mpsc::Sender::send` future is async.
//!
//! # Lifetime / object safety
//!
//! The trait is object-safe (`Box<dyn CompactorBackend>` is the
//! orchestrator field) — no `Self` in return position, no generic
//! methods. `Send + Sync + 'static` keeps the trait compatible with
//! the orchestrator's `&mut self` access pattern when called from
//! inside the agentic loop.

use crate::compaction::types::{CompactionConfig, CompactionRequest, CompactionResponse};
use anyhow::Result;
use tokio::sync::oneshot;

/// Narrow trait port that lets the lifted [`super::CompactionOrchestrator`]
/// (`crates/engine/src/compaction_orchestrator.rs`) talk to root's
/// `BackgroundCompactor` without holding a direct reference.
///
/// Implemented in root by `BackgroundCompactor`
/// (`src/engine/compaction_backend_compat.rs` — the orphan-rule-friendly
/// home for the impl). The trait disappears when a later phase lifts
/// `BackgroundCompactor` itself into `peko-engine` (only blocked by the
/// concrete `Provider` lift, deferred per Phase 6's note).
#[async_trait::async_trait]
pub trait CompactorBackend: Send + Sync + 'static {
    /// Cheap predicate: should we consider compacting now?
    ///
    /// Mirrors the pre-lift `BackgroundCompactor::should_request`
    /// signature exactly:
    ///
    /// ```ignore
    /// async fn should_request(
    ///     &self,
    ///     estimated_tokens: usize,
    ///     context_window: usize,
    ///     config: &CompactionConfig,
    /// ) -> bool;
    /// ```
    ///
    /// Implementations SHOULD consult both the dual-threshold check
    /// (`should_auto_compact`) AND the worker-side cooldown/quota
    /// state so the orchestrator never has to reason about worker
    /// state directly. `true` means "submit a request"; the worker
    /// may still come back with [`CompactionResponse::Skipped`] if a
    /// concurrent call tripped it first.
    async fn should_request(
        &self,
        estimated_tokens: usize,
        context_window: usize,
        config: &CompactionConfig,
    ) -> bool;

    /// Submit a compaction request and receive the outcome via a
    /// oneshot.
    ///
    /// Returns `Ok(oneshot::Receiver<CompactionResponse>)` on enqueue
    /// success. The orchestrator polls the receiver with a 100ms
    /// timeout per loop iteration so the agentic loop is never
    /// blocked on the summarization LLM call. Returns `Err(_)` if
    /// the worker's `mpsc` channel is closed (daemon shutdown or
    /// worker task panic).
    async fn request(
        &self,
        request: CompactionRequest,
    ) -> Result<oneshot::Receiver<CompactionResponse>>;
}
