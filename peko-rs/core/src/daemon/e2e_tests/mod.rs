//! Opt-in integration tests for the daemon module.
//!
//! Currently only the layered E2E test that boots a full PekoHub backend and
//! exercises the runtime → tunnel → hub → LLM path. All tests in this submod
//! require `--features test-utils` so the daemon internals can stay
//! `pub(crate)` rather than being inflated to `pub` just to be reachable
//! from a top-level `tests/*.rs` integration harness.
mod tunnel_e2e;
