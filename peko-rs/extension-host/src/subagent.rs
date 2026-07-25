//! `SpawnCleanupPolicy` re-export.
//!
//! The canonical home is `peko_extension_api::subagent::SpawnCleanupPolicy`.
//! This module is a backwards-compat shim that re-exports it so callers
//! that wrote `peko_extension_host::SpawnCleanupPolicy` (the pre-Phase-8b
//! path when the enum was owned by the host) keep compiling.
//!
//! Why the canonical home moved in Phase 8b:
//! the host crate depends on `peko_tools_builtin::async_control::*`
//! (the `AsyncRuntime` port consumed by
//! `async_exec/executor/async_runtime_impl.rs`). `peko_tools_builtin` in
//! turn depended on `peko_extension_host::SpawnCleanupPolicy` via
//! `messaging::dto::SpawnCleanupPolicy`, which created a cycle. Moving
//! the enum into `peko_extension_api` (downstream of both crates)
//! breaks the cycle without forcing the messaging module to import
//! the host crate.

pub use peko_extension_api::subagent::SpawnCleanupPolicy;
