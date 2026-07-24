//! Re-export shim — Phase 9b.N.5b.6 lifted `drop_oldest_respecting_pairs`
//! into `peko_engine::compaction::eviction` so the helper lives next to
//! its single consumer (the agentic loop, soon to arrive in
//! `peko-engine`). Phase 7 promoted the helper one step further into
//! `peko_session::compaction::eviction` (the persistence-side owner).
//! Engine re-exports it via `peko_engine::compaction::eviction` and at
//! `peko_engine::drop_oldest_respecting_pairs`; this shim keeps the
//! pre-Phase-7 import path
//! (`crate::session::compaction::eviction::drop_oldest_respecting_pairs`)
//! working until Phase 7.4 deletes `src/session/`.

pub use peko_engine::compaction::drop_oldest_respecting_pairs;
