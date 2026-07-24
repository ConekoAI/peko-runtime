//! `BackgroundCompactorFactory` — trait port for the
//! `BackgroundCompactor` construction site.
//!
//! Phase 7 promotes this trait from `peko-engine::compaction::factory`
//! into `peko-session::compaction::factory`. The loop is in
//! `peko-engine` (Phase 9b.N.5b.8+); the factory captures the
//! `Arc<Provider>` and other daemon-side wiring at construction time
//! so the loop never imports root-only types.
//!
//! # Why a factory, not just a `Box<dyn CompactorBackend>` parameter
//!
//! `BackgroundCompactor::new` requires a concrete `Arc<Provider>`
//! plus the loop's stored `quota_meter` / `peer_meter`. The loop
//! calls it once per `run_inner_with_meter` invocation. If we made
//! the loop accept the already-constructed
//! `Arc<dyn CompactorBackend>` directly, callers would have to
//! construct a fresh `BackgroundCompactor` every run AND match the
//! loop's `quota_meter` / `peer_meter` swap behaviour (F19/F20) at
//! every call site — that's much more code than a factory seam.
//!
//! The factory takes the loop's two stored meters (which are
//! `peko_quota::QuotaMeter` workspace types — NOT root-only) and
//! returns a `Box<dyn CompactorBackend>`. The impl captures the
//! inner provider from its own state (built when the factory itself
//! was constructed at root).

use crate::compaction::CompactorBackend;
use peko_quota::QuotaMeter;
use std::sync::Arc;

/// Trait port for the loop's compactor construction site.
///
/// The loop calls `factory.build(meter, peer_meter)` once per
/// `run_inner_with_meter` to get a fresh `CompactorBackend`. The
/// root impl (in `src/engine/background_compactor_factory_compat.rs`)
/// captures the inner `Arc<Provider>` at factory construction time
/// and feeds it into the new `BackgroundCompactor` along with the
/// supplied meters.
///
/// `meter` is the loop's stored principal quota meter (F19).
/// `peer_meter` is the optional per-peer meter (F20); `None` falls
/// back to a single-meter configuration.
pub trait BackgroundCompactorFactory: Send + Sync + 'static {
    /// Build a fresh `CompactorBackend` configured with the supplied
    /// meters.
    fn build(
        &self,
        meter: Arc<QuotaMeter>,
        peer_meter: Option<Arc<QuotaMeter>>,
    ) -> Box<dyn CompactorBackend>;
}
