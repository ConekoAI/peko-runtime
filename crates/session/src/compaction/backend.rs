//! `CompactorBackend` — narrow trait port so the lifted
//! `CompactionOrchestrator` (`peko-engine::compaction_orchestrator`)
//! can talk to the persistence-owned `BackgroundCompactor` without
//! depending on `peko-session`.
//!
//! Phase 7 moved the trait from `peko-engine::compaction::backend`
//! into this crate (`peko-session::compaction::backend`) so the
//! canonical home matches the data layout's owner. `peko-engine`
//! re-exports the trait via `peko_engine::CompactorBackend` so the
//! orchestrator's import paths keep compiling.
//!
//! # Trait port rationale
//!
//! The orchestrator's shape is:
//!
//! ```ignore
//! pub struct CompactionOrchestrator {
//!     backend: Box<dyn CompactorBackend>,
//!     ...
//! }
//! ```
//!
//! The trait stays minimal — `should_request` + `request` — matching
//! the trait-port pattern established by Phase 9b.N.1
//! (`AsyncCompletionLike`), 9b.N.2 (`ToolFunnel`), 9b.N.3
//! (`SessionView`), 9b.N.5a (`AgentView`), and 9b.N.5b.7
//! (`ProviderView`).
//!
//! # Methods
//!
//! - [`should_request`] — sync-ish check that gates whether to fire
//!   the background worker. Returns `true` to mean "you may submit a
//!   request; the worker may still skip with
//!   [`CompactionResponse::Skipped`] if its internal state disallows
//!   it".
//!
//! - [`request`] — async submission that enqueues a
//!   [`CompactionRequest`] onto the worker's `mpsc` and returns a
//!   `oneshot::Receiver<CompactionResponse>`. The orchestrator polls
//!   this receiver with a 100ms timeout per loop iteration so the
//!   agentic loop never blocks on the summarization LLM call.
//!
//! Both methods are `async` (not just `&self`) for symmetry with the
//! orchestrator's pre-lift call sites — `should_request` reads the
//! worker's `Arc<Mutex<WorkerState>>` which may need an async lock
//! under contention, and `request` is naturally async because the
//! `mpsc::Sender::send` future is async.
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

/// Narrow trait port that lets the [`super::CompactionOrchestrator`]
/// (`peko-engine::compaction_orchestrator`) talk to the
/// `BackgroundCompactor` here in `peko-session` without holding a
/// direct reference.
#[async_trait::async_trait]
pub trait CompactorBackend: Send + Sync + 'static {
    /// Cheap predicate: should we consider compacting now?
    ///
    /// Mirrors the pre-Phase-7 `BackgroundCompactor::should_request`
    /// signature. Implementations SHOULD consult both the
    /// dual-threshold check (`should_auto_compact`) AND the
    /// worker-side cooldown/quota state so the orchestrator never
    /// has to reason about worker state directly. `true` means
    /// "submit a request"; the worker may still come back with
    /// [`CompactionResponse::Skipped`] if a concurrent call tripped
    /// it first.
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
