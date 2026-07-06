//! Tools for agents
//!
//! This module is organized into three layers:
//!
//! 1. **`core`** — Fundamental traits and types (`Tool`, `ToolContext`, `ToolResult`, `ToolError`)
//! 2. **`registry`** — Tool discovery, factory, and registration (`factory`)
//! 3. **`builtin`** — Built-in tool implementations (filesystem, Bash, cron, session, messaging, async control, planning todos)
//!
//! Previously, this module also contained `framework` (async_executor, universal protocol,
//! shared utilities). These have been migrated to `src/extensions/` (Issue 014).
//!
//! Heavy tools (web_search, fetch, http, browser, memory) are provided via external MCP servers
//! or the extension system.

pub mod builtin;
pub mod core;
pub mod registry;

// Re-exports from core for convenience — these are the most commonly imported types.
pub use core::{
    bridge_from_cancellation_token, bridge_to_cancellation_token, AbortSignal,
    AbortSignalBridgeGuard, CancellationTokenBridgeGuard, Tool, ToolContext, ToolError,
    ToolInterruptNotice, ToolResult, ToolWithContext,
};

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
