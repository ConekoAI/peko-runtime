//! Compatibility shim: implements `peko_engine::BackgroundCompactorFactory`
//! via a thin adapter that captures the inner `Arc<Provider>` at factory
//! construction time and rebuilds a `BackgroundCompactor` on every `build(...)`
//! call with the loop's per-call `quota_meter` / `peer_meter`.
//!
//! # Why a factory + adapter
//!
//! Phase 9b.N.5b.7's trait port for the line-892
//! `BackgroundCompactor::new(provider.inner().clone(), ...)` construction
//! site. The loop is moving into `peko-engine` (Phase 9b.N.5b.8) and cannot
//! name the root-only `BackgroundCompactor` type.
//!
//! The factory impl captures the inner `Arc<Provider>` (root-only) at the
//! point the factory is constructed by root code — typically the same call
//! site that hands the `Provider` to the loop. Every subsequent
//! `factory.build(meter, peer_meter)` call rebuilds a fresh
//! `BackgroundCompactor` because the loop constructs a new compactor every
//! `run_inner_with_meter` invocation (each run gets its own worker task).
//!
//! # Trait port rationale
//!
//! `BackgroundCompactorFactory` (defined at
//! `peko_engine::compaction::factory`) takes the loop's two stored meters
//! (F19 principal + F20 optional peer) and returns
//! `Box<dyn CompactorBackend>`. The trait's parameters are workspace types
//! (`peko_quota::QuotaMeter`), not root-only — so the trait definition in
//! `peko-engine` doesn't depend on the root crate.
//!
//! The impl lives here (not in `peko-engine`) because of the orphan rule:
//! `peko_engine::BackgroundCompactorFactory` is a foreign trait, and the
//! factory needs the root-only `Arc<Provider>` + `BackgroundCompactor::new(...)`.
//! We wrap the impl in a concrete adapter type
//! (`BackgroundCompactorFactoryAdapter`) — the orphan rule allows
//! `impl BackgroundCompactorFactory for BackgroundCompactorFactoryAdapter`
//! because the adapter is local to root.
//!
//! # Trait port lifetime
//!
//! Mirrors the pattern established by 9b.N.1 (`AsyncCompletionLike`),
//! 9b.N.2 (`ToolFunnel`), 9b.N.3 (`SessionView`), 9b.N.5a (`AgentView`),
//! 9b.N.5b.7 `ProviderView` (sibling). The trait disappears when a later
//! phase lifts `BackgroundCompactor` into `peko-engine` (blocked on the
//! concrete `Provider` lift, deferred per Phase 6's note).
//!
//! Module location: rooted at `src/engine/background_compactor_factory_compat.rs`
//! so `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/session_view_compat.rs` and
//! `src/engine/extension_core_funnel_compat.rs` patterns.

use crate::session::compaction::background::BackgroundCompactor;
use peko_engine::{BackgroundCompactorFactory, CompactorBackend, ProviderView};
use peko_quota::QuotaMeter;
use std::sync::Arc;

/// Adapter that captures the inner `Arc<dyn ProviderView>` and rebuilds a
/// `BackgroundCompactor` on every `build(...)` call.
///
/// Constructed by root callers (CLI, IPC, subagent executor, tests) at the
/// same time they hand the provider view to the loop. The view is held
/// inside the adapter; the loop owns an `Arc<dyn BackgroundCompactorFactory>`
/// pointing at this adapter.
///
/// Phase 9b.N.5b.9: switched from `Arc<Provider>` (root-only concrete
/// type) to `Arc<dyn ProviderView>` (engine-owned trait object) so the
/// adapter is constructible from the loop's `Arc<dyn ProviderView>`
/// field without needing a separate concrete-Provider clone.
pub struct BackgroundCompactorFactoryAdapter {
    provider: Arc<dyn ProviderView>,
}

impl BackgroundCompactorFactoryAdapter {
    /// Build an adapter that captures the given provider view. The view
    /// is held by `Arc` and used to construct a fresh
    /// `BackgroundCompactor` on every `build(...)` call.
    #[must_use]
    pub fn new(provider: Arc<dyn ProviderView>) -> Self {
        Self { provider }
    }
}

impl BackgroundCompactorFactory for BackgroundCompactorFactoryAdapter {
    fn build(
        &self,
        meter: Arc<QuotaMeter>,
        peer_meter: Option<Arc<QuotaMeter>>,
    ) -> Box<dyn CompactorBackend> {
        Box::new(BackgroundCompactor::new(
            Arc::clone(&self.provider),
            meter,
            peer_meter,
        ))
    }
}
