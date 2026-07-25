//! Tools for agents
//!
//! This module is organized into two layers (Phase 15 deleted the third):
//!
//! 1. **`registry`** — Tool discovery, factory, and registration (`factory`)
//! 2. **`builtin`** — Built-in tool implementations (filesystem, Bash, cron, session, messaging, async control, planning todos)
//!
//! Phase 15 deleted the `core` sub-shim (the `Tool`/`ToolContext`/`ToolResult`
//! traits + `AbortSignal` + bridge helpers + `ToolInterruptNotice` + `ToolError`).
//! Callers must import these directly from `peko_tools_core`. The
//! previously-convenient `crate::tools::Tool` re-exports at this module
//! level have also been deleted per the cleanup invariant (root must not
//! `pub use peko_*::*`).
//!
//! Previously, this module also contained `framework` (async_executor, universal protocol,
//! shared utilities). These have been migrated to `src/extensions/` (Issue 014).
//!
//! Heavy tools (web_search, fetch, http, browser, memory) are provided via external MCP servers
//! or the extension system.

pub mod builtin;
pub mod registry;

// Re-exports from builtin for convenience.
pub use builtin::{
    AgentTool, AsyncListTool, AsyncOutputTool, AsyncSpawnTool, AsyncStatusTool, AsyncStopTool,
    BashTool, CronCreateTool, CronDeleteTool, CronListTool, EditTool, GlobTool, GrepTool, ReadTool,
    SessionCache, SessionInfo, SessionIntrospectionRegistry, SessionIntrospector, SessionTool,
    TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool, WriteTool,
};

// Re-exports from registry
pub use registry::{
    McpDiscoveryResult, McpFactoryConfig, ToolCreationResult, ToolFactory, ToolFactoryConfig,
};
