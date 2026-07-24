//! Re-export shim for the moved integration tests.
//!
//! Phase 7.2 lifted the ADR-022 integration tests into
//! `peko-session::compaction::integration_tests`. Pre-Phase-7 import
//! paths continue to compile via this shim. The peko-session
//! integration tests now run as part of `cargo test -p peko-session`.
//!
//! Module removed in Phase 7.4 when `src/session/` is deleted.

#[cfg(test)]
mod tests {
    // Empty shim — actual test bodies live in peko-session. The
    // legacy `crate::session::compaction::integration_tests::X` paths
    // resolve to `peko_session::compaction::integration_tests::X`
    // through `peko_session::compaction::integration_tests::*` (the
    // outer module is `#[cfg(test)] mod integration_tests;` inside
    // `compaction_top.rs`, with `#[path]` so this file is unnecessary
    // at this layer).
}