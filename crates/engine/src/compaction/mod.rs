//! Compaction subsystem — Phase 9b.N.4 lift.
//!
//! Phase 9b.N.4 split the previously root-only compaction domain
//! across two crates:
//!
//! - **`peko-engine`** (this module) — pure-data types
//!   ([`CompactionConfig`], [`CompactionEntry`], [`CompactionState`],
//!   [`ContextUsageEstimate`], [`CompactionResult`],
//!   [`CompactionRequest`], [`CompactionResponse`],
//!   [`CompactionQuota`]) and the narrow [`CompactorBackend`] trait
//!   port. These are what the lifted `CompactionOrchestrator`
//!   needs to make decisions and persist results without depending
//!   on root-only types.
//!
//! - **Root** (`src/session/compaction/`) — the `Compactor` (LLM
//!   summarization logic), `BackgroundCompactor` (mpsc worker),
//!   `summary_format` (file-ops accumulator), `turn_boundaries`
//!   (tool-pairing preservation), `eviction`, and `cli`. These
//!   produce/consume the data types above but are root-coupled via
//!   `crate::providers::Provider` + `crate::quota::QuotaScope` and
//!   stay in root until later lifts reduce those couplings.
//!
//! # Why a trait port?
//!
//! The orchestrator (`CompactionOrchestrator` in this crate) needs
//! to ask "should I compact? please compact and give me the result".
//! Before 9b.N.4 it held a concrete `Arc<BackgroundCompactor>`
//! field. After the lift, root still owns the implementation, so the
//! orchestrator gets it through a `Box<dyn CompactorBackend>`. The
//! trait has just two methods:
//!
//! - [`CompactorBackend::should_request`] — cheap synchronous check
//!   the orchestrator calls every iteration to decide whether to
//!   fire the async path. Maps to `should_auto_compact(...)` +
//!   quota/cooldown state on `BackgroundCompactor`.
//! - [`CompactorBackend::request`] — async method that sends the
//!   request through the worker's `mpsc` and waits on a oneshot for
//!   the outcome.
//!
//! The trait port mirrors the trait-port pattern established in
//! 9b.N.1 (`AsyncCompletionLike`), 9b.N.2 (`ToolFunnel`), and
//! 9b.N.3 (`SessionView`). It disappears when a later phase lifts
//! `BackgroundCompactor` itself into `peko-engine` (only blocked by
//! the concrete `Provider` lift, deferred per Phase 6's note).
//!
//! # Module layout
//!
//! ```text
//! crates/engine/src/compaction/
//!   mod.rs           ← this file (re-exports types + backend)
//!   backend.rs       ← CompactorBackend trait
//!   types.rs         ← data structs/enums (CompactionConfig, ...)
//! ```
//!
//! `orchestrator.rs` (the lifted `CompactionOrchestrator`) lives in
//! the parent `crates/engine/src/` directory next to the other
//! engine modules rather than nested under `compaction/` to match
//! the flat layout Phase 9a established.

pub mod backend;
pub mod eviction;
pub mod factory;
pub mod types;

pub use backend::CompactorBackend;
pub use eviction::drop_oldest_respecting_pairs;
pub use factory::BackgroundCompactorFactory;
pub use types::{
    CompactionConfig, CompactionEntry, CompactionQuota, CompactionRequest, CompactionResponse,
    CompactionResponseResult, CompactionResult, CompactionState, ContextUsageEstimate,
};
