//! Compaction subsystem (Phase 7 — orchestration-only facade).
//!
//! Phase 7 moved the persistence side of compaction (data types,
//! trait ports, eviction helper, `BackgroundCompactor` mpsc worker,
//! `Compactor` LLM summarization helper, `summary_format`,
//! `turn_boundaries`, `cli`) from this crate into the new
//! `peko-session` crate. The orchestration stays here:
//!
//! - **`peko-engine::compaction_orchestrator`** —
//!   [`CompactionOrchestrator`](crate::compaction_orchestrator::CompactionOrchestrator)
//!   holds a `Box<dyn CompactorBackend>` supplied by the daemon
//!   (the concrete `BackgroundCompactor` impl lives in
//!   `peko-session`).
//!
//! For backward compatibility, this module re-exports the data
//! types + trait port + eviction helper from `peko-session` so the
//! pre-Phase-7 import paths (`peko_engine::compaction::CompactionConfig`,
//! `peko_engine::compaction::CompactorBackend`, etc.) keep
//! compiling.

pub use peko_session::compaction::{
    drop_oldest_respecting_pairs, BackgroundCompactorFactory, CompactionConfig, CompactionEntry,
    CompactionQuota, CompactionRequest, CompactionResponse, CompactionResponseResult,
    CompactionResult, CompactionState, CompactorBackend, ContextUsageEstimate,
};
