//! Re-export shim — Phase 9b.N.5b.6 lifted `drop_oldest_respecting_pairs`
//! into `peko_engine::compaction::eviction` so the helper lives next to
//! its single consumer (the agentic loop, soon to arrive in
//! `peko-engine`). Existing call sites that imported via
//! `crate::session::compaction::eviction::drop_oldest_respecting_pairs`
//! keep working through this one-line re-export.

pub use peko_engine::compaction::eviction::drop_oldest_respecting_pairs;
