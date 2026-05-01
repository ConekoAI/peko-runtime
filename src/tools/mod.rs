//! Tools for agents
//!
//! This module is organized into four layers:
//!
//! 1. **`core`** — Fundamental traits and types (`Tool`, `ToolContext`, `ToolResult`, `ToolError`)
//! 2. **`framework`** — Reusable execution frameworks (`async_executor`, `universal` protocol, `shared` utilities)
//! 3. **`registry`** — Tool discovery, factory, and registration (`factory`, `builtin` registrar)
//! 4. **`builtin`** — Built-in tool implementations (filesystem, shell, cron, session, messaging, task management)
//!
//! Heavy tools (web_search, fetch, http, browser, memory) are provided via external MCP servers
//! or the extension system.

pub mod builtin;
pub mod core;
pub mod framework;
pub mod registry;

// Re-exports from core for convenience — these are the most commonly imported types.
pub use core::{AbortSignal, Tool, ToolContext, ToolError, ToolResult, ToolWithContext};

// Re-exports from builtin for convenience.
pub use builtin::{
    A2aSendTool, AgentSpawnTool, ChannelType, CronTool, GlobTool, GrepTool, MessageConfig,
    MessageResult, MessageTool, ReadFileTool, SessionCache, SessionInfo, SessionIntrospector,
    SessionIntrospectionRegistry, SessionTool, ShellTool, StrReplaceFileTool, TaskTool,
    WriteFileTool,
};


// Re-exports from registry
pub use registry::{
    BuiltinToolRegistrar, BuiltinToolRegistrarConfig, McpDiscoveryResult, McpFactoryConfig,
    ToolCreationResult, ToolFactory, ToolFactoryConfig,
};

// Async tool framework re-exports (commonly used across the codebase)
pub use framework::{
    AsyncExecutor, AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent,
    AsyncTaskEntry, AsyncTaskEventBus, AsyncTaskId, AsyncTaskReceipt, AsyncTaskRegistry,
    AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig, CallbackDelivery, ChannelDelivery,
    DefaultResultFormatter, DeliveryTarget, FormatterRegistry, QueueDelivery, ResultDelivery,
    ResultFormatter, SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    TaskFileRecord, TaskFileWriter, WaitResult,
    build_completion_event, cancel_task_across_all_registries, find_run_across_all_registries,
    find_task_across_all_registries, get_or_create_registry_for_agent,
    list_all_runs_across_all_registries, list_all_tasks_across_all_registries,
};
