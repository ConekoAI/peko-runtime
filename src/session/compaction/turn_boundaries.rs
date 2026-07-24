//! Re-export shim — Phase 7.2 lifted the tool-pairing preservation
//! helper into `peko-session::compaction::turn_boundaries`.
//! Pre-Phase-7 import paths continue to compile via this shim.
//!
//! Module removed in Phase 7.4 when `src/session/` is deleted.

pub use peko_session::compaction::turn_boundaries::*;