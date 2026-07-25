//! Tools for agents
//!
//! This module is organized into two layers (Phase 15 deleted the third):
//!
//! 1. **`registry`** — Tool discovery, factory, and registration (`factory`)
//! 2. **`builtin`** — Built-in tools. Two kinds of files live here:
//!
//!    a. **Canonical implementations that need root-only deps** —
//!       `bash.rs`, `agent.rs` (`Agent`), `skill/` (the skill tool wrapper),
//!       `agent_catalog.rs`, `tool_search.rs`. These can't lift into
//!       `peko-tools-builtin` because they reach into `ExtensionCore` or
//!       other root types.
//!    b. **Compat shims with hosted tests** — `async_list.rs`,
//!       `async_output.rs`, `async_status.rs`, `async_stop.rs`. Each is
//!       a one-line `pub use peko_tools_builtin::async_control::*`
//!       followed by `#[cfg(test)] mod tests` blocks that exercise the
//!       canonical implementation using `TestAsyncRuntime` /
//!       `TestTaskEntry` from `peko_extension_host` (framework-internal
//!       fixtures). These shims survive because removing them would
//!       delete ~30 working tests; the production code is in
//!       `peko_tools_builtin::async_control::*`.
//!
//!    All other built-in tools (filesystem, cron, session, planning
//!    todos, AsyncSpawn, …) live in `peko_tools_builtin` and consumers
//!    import them directly via `peko_tools_builtin::*`.
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
//! Previously, this module also contained `framework` (async_executor,
//! universal protocol, shared utilities). The `core`/`types`/`manager`/
//! `transport`/`services`/`protocols/shared` parts were lifted into the
//! `peko-extension-host` workspace crate in Phase 8 (PRs #292, #293,
//! follow-ups #294-#297). Root keeps `src/extensions/framework/` as a
//! compat-shim layer (`async_exec/`, `types/`) until Phase 15 deletes it;
//! those shims re-export from `peko_extension_host` and pin type identity
//! via `TypeId::of` assertions — see
//! `extensions/framework/async_exec/executor/completion_queue.rs:78`.
//!
//! Heavy tools (web_search, fetch, http, browser, memory) are provided via external MCP servers
//! or the extension system.

pub mod builtin;
pub mod registry;
