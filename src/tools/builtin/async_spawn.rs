//! `AsyncSpawnTool` — re-export shim.
//!
//! Phase 10c moved the implementation into
//! `peko_tools_builtin::async_control::spawn`. This file is now a thin
//! re-export shim so existing `crate::tools::builtin::AsyncSpawnTool`
//! paths continue to work. The detailed doc lives upstream.

pub use peko_tools_builtin::async_control::AsyncSpawnTool;
