//! Core tool parity harness for the Claude Code built-in tool surface.
//!
//! This crate will contain golden-transcript tests that verify peko's
//! built-in tools expose the same names and schemas as Claude Code's
//! core tools. Phase 1 is the scaffold; individual tool parity fixtures
//! land in Phase 2+.

mod common;

/// Smoke test that the core tool parity harness and its supporting files are in place.
#[test]
fn core_tools_harness_compiles() {
    let manifest_dir = std::env!("CARGO_MANIFEST_DIR");
    let catalog = std::path::Path::new(manifest_dir).join("docs/architecture/builtin-tools.md");
    assert!(catalog.exists(), "built-in tools catalog should exist");
}

// Future fixtures (to be added as tools are renamed):
// - Read -> Read schema parity
// - Write -> Write schema parity
// - Edit -> Edit schema parity
// - shell -> Bash schema parity
// - cron -> CronCreate/CronDelete/CronList split
// - Agent -> Agent schema parity
// - task -> Async*/Task* family split
