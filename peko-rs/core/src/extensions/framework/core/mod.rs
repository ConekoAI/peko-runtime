//! Root-only `framework::core` shim.
//!
//! Phase 8a moved 12 of the 13 files in `framework/core/` into
//! `peko_extension_host::core`. The remaining file, `async_bridge.rs`,
//! imports from `framework/async_exec` (Phase 8b) and is deferred.
//! Callers should import the host-side types from
//! `peko_extension_host::core::*` directly.

// `async_bridge.rs` stays in root until Phase 8b lifts async_exec.
pub mod async_bridge;
pub use async_bridge::ExtensionAsyncAdapter;
