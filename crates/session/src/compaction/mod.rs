//! Context compaction — persistence side.
//!
//! Phase 7 moved the persistence impl (`BackgroundCompactor`,
//! `Compactor`, `summary_format`, `turn_boundaries`, `eviction`,
//! `cli`) from root's `src/session/compaction/` into this crate. The
//! orchestration (`CompactionOrchestrator`) stays in
//! `peko-engine::compaction_orchestrator`; the orchestrator holds a
//! `Box<dyn CompactorBackend>` supplied by the daemon (root wires the
//! concrete impl through `src/engine/background_compactor_factory_compat.rs`).
//!
//! ## Module structure
//!
//! - [`types`] — pure-data DTOs (CompactionConfig, CompactionEntry,
//!   CompactionState, ContextUsageEstimate, CompactionResult,
//!   CompactionRequest, CompactionResponse, CompactionQuota,
//!   CompactionResponseResult). These are what the orchestrator needs
//!   for bookkeeping and the trait-port signatures; the
//!   persistence-side `BackgroundCompactor` also consumes them.
//! - [`backend`] — `CompactorBackend` trait port the orchestrator
//!   holds.
//! - [`factory`] — `BackgroundCompactorFactory` trait port the
//!   loop calls per iteration.
//! - `background` (Phase 7.2+) — `BackgroundCompactor` mpsc worker.
//! - `cli` (Phase 7.2+) — CLI / programmatic invocation entry
//!   points.
//! - `eviction` (Phase 7.2+) — front-eviction helper for
//!   `ContextWindowExceeded` recovery.
//! - `summary_format` (Phase 7.2+) — file-ops accumulator used by
//!   `Compactor::compact`.
//! - `turn_boundaries` (Phase 7.2+) — tool-pairing preservation.
//! - `Compactor` (Phase 7.2+) — LLM summarization helper that
//!   produces `CompactionResult`s.
//! - `load_compaction_config` (Phase 7.2+) — TOML loader.
//!
//! Phase 7.1 lands the scaffold + the data types + the trait ports
//! first. Phase 7.2 will move `background.rs`, `cli.rs`,
//! `eviction.rs`, `summary_format.rs`, `turn_boundaries.rs`, and the
//! `Compactor` impl + the `load_compaction_config` loader. Once
//! 7.2 lands, `peko-engine::compaction::*` re-exports everything
//! from here and the legacy `src/session/compaction/` directory is
//! deleted in Phase 7.4.

pub mod backend;
pub mod background;
pub mod compaction_top;
pub mod eviction;
pub mod factory;
pub mod summary_format;
pub mod turn_boundaries;
pub mod types;

#[cfg(test)]
mod integration_tests;

pub use backend::CompactorBackend;
pub use background::{should_auto_compact, BackgroundCompactor};
pub use compaction_top::{load_compaction_config, Compactor};
pub use eviction::drop_oldest_respecting_pairs;
pub use factory::BackgroundCompactorFactory;
pub use summary_format::{
    compute_cumulative_details, extract_file_ops_from_messages, format_summary_with_file_ops,
    CompactionDetails,
};
pub use turn_boundaries::{
    classify_message, find_cut_points, select_messages_respecting_boundaries, MessageKind,
};
pub use types::{
    CompactionConfig, CompactionEntry, CompactionQuota, CompactionRequest, CompactionResponse,
    CompactionResponseResult, CompactionResult, CompactionState, ContextUsageEstimate,
};
