//! Re-export shim — Phase 7.2 lifted the persistence-side
//! `Compactor` LLM summarization helper + `load_compaction_config`
//! loader into `peko-session::compaction::compaction_top`. The
//! submodules `background.rs`, `summary_format.rs`,
//! `turn_boundaries.rs`, and `integration_tests.rs` are re-export
//! shims over `peko-session::compaction::{background, summary_format,
//! turn_boundaries, integration_tests}`. `cli.rs` stays in root
//! because it depends on the not-yet-moved
//! `crate::session::unified::Session`.
//!
//! Module removed in Phase 7.4 when `src/session/` is deleted.

pub use peko_engine::compaction::{
    CompactionConfig, CompactionEntry, CompactionQuota, CompactionRequest, CompactionResponse,
    CompactionResponseResult, CompactionResult, CompactionState, CompactorBackend,
    ContextUsageEstimate,
};

pub use peko_session::compaction::{
    classify_message, compute_cumulative_details, drop_oldest_respecting_pairs,
    extract_file_ops_from_messages, find_cut_points, format_summary_with_file_ops, load_compaction_config,
    select_messages_respecting_boundaries, should_auto_compact, BackgroundCompactor,
    CompactionDetails, Compactor, MessageKind,
};

pub mod background;
pub mod cli;
pub mod eviction;
pub mod summary_format;
pub mod turn_boundaries;

#[cfg(test)]
mod integration_tests;