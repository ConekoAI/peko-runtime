//! Tools for agents
//!
//! This module is organized into two layers (Phase 15 deleted the third):
//!
//! 1. **`registry`** — Tool discovery, factory, and registration (`factory`)
//! 2. **`builtin`** — Built-in tool implementations that still live in root
//!    (Bash, Agent, Skill, agent_catalog, tool_search, async_list/output/
//!    status/stop shims with framework-internal test fixtures). All other
//!    built-in tools (filesystem, cron, session, planning todos,
//!    AsyncSpawn, …) now live in `peko_tools_builtin` and consumers import
//!    them directly via `peko_tools_builtin::*`.
//!
//! Phase 15 deleted the `core` sub-shim (the `Tool`/`ToolContext`/`ToolResult`
//! traits + `AbortSignal` + bridge helpers + `ToolInterruptNotice` + `ToolError`).
//! Callers must import these directly from `peko_tools_core`.
//!
//! Phase 18 deleted the per-file re-export shims in `tools/builtin/` for
//! `cron`, `cron_create`, `cron_delete`, `cron_list`, `fs`, `session`,
//! `task_common`, `task_create`, `task_get`, `task_list`, `task_update`,
//! and `async_spawn`. It also deleted the convenience re-exports at this
//! module level (`pub use builtin::{...}` + `pub use registry::{...}`) per
//! the cleanup invariant (root must not `pub use peko_*::*`).
//!
//! Previously, this module also contained `framework` (async_executor, universal protocol,
//! shared utilities). These have been migrated to `src/extensions/` (Issue 014).
//!
//! Heavy tools (web_search, fetch, http, browser, memory) are provided via external MCP servers
//! or the extension system.

pub mod builtin;
pub mod registry;
