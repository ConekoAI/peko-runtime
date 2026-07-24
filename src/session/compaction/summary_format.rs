//! Re-export shim — Phase 7.2 lifted the file-ops accumulator into
//! `peko-session::compaction::summary_format`. Pre-Phase-7 import
//! paths continue to compile via this shim.
//!
//! Module removed in Phase 7.4 when `src/session/` is deleted.

pub use peko_session::compaction::summary_format::*;