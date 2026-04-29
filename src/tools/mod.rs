//! Tools for agents
//!
//! Core tools only. Heavy tools (web_search, fetch, http, browser, memory)
//! have been migrated to standalone MCP servers:
//! - mcp-web: web_search, fetch, http
//! - mcp-browser: browser automation  
//! - mcp-memory: persistent memory storage
//!
//! Custom tools use the Universal Tool Protocol (see `universal` module).

/// Built-in tool registry for ExtensionCore integration
pub mod builtin_registry;

pub mod a2a_send;
pub mod agent_management;
pub mod agent_spawn;
pub mod async_executor;
pub mod task_management;
pub mod context;
pub mod cron_tool;
pub mod factory;
pub mod glob;
pub mod grep;
pub mod message_tool;
pub mod read_file;
pub mod session_introspection;
pub mod sessions_send;
pub mod shell;
pub mod str_replace_file;
pub mod traits;
// pub mod wrapper;  // Removed in ADR-019 cleanup
pub mod write_file;

/// Universal Tool Protocol - Backend-agnostic tool integration
///
/// This module implements the custom tool system using JSON-RPC 2.0 over stdio.
/// It supports reserved parameter injection and works with any language.
pub mod universal;

/// Shared utilities for tool implementations
///
/// Provides common functionality used by both Universal Tools and MCP tools
/// to avoid code duplication and ensure consistent behavior.
pub mod shared;

pub use a2a_send::A2aSendTool;
pub use sessions_send::SessionsSendTool;

pub use agent_management::{AgentInfoTool, AgentsListTool, ManagerCommand};
pub use agent_spawn::AgentSpawnTool;
pub use task_management::{TaskListTool, TaskStatusTool};
pub use context::{AbortSignal, ToolContext, ToolWithContext};
pub use cron_tool::CronTool;
pub use factory::{McpDiscoveryResult, McpFactoryConfig, ToolFactory, ToolFactoryConfig};
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use message_tool::{ChannelType, MessageConfig, MessageResult, MessageTool};
pub use read_file::ReadFileTool;
pub use session_introspection::{
    SessionCache, SessionInfo, SessionIntrospector,
    SessionRegistry as SessionIntrospectionRegistry, SessionStatusTool, SessionsHistoryTool,
    SessionsListTool,
};

/// Backward-compatible alias for `SessionIntrospector`.
#[deprecated(since = "0.2.0", note = "Use SessionIntrospector instead")]
pub use session_introspection::SessionIntrospector as AgentSessionRegistry;
// Backward compatibility — deprecated, use SessionCache
#[allow(deprecated)]
pub use session_introspection::InMemorySessionRegistry;
pub use shell::ShellTool;
pub use str_replace_file::StrReplaceFileTool;
pub use traits::{Tool, ToolError, ToolResult};
pub use write_file::WriteFileTool;

// Async tool trait re-exports
// Async tool framework re-exports
pub use async_executor::{
    AsyncExecutor, AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent,
    AsyncTaskEntry, AsyncTaskEventBus, AsyncTaskId, AsyncTaskReceipt, AsyncTaskRegistry,
    AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig, CallbackDelivery, ChannelDelivery,
    DefaultResultFormatter, DeliveryTarget, FormatterRegistry, QueueDelivery, ResultDelivery,
    ResultFormatter, SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    TaskFileRecord, TaskFileWriter, WaitResult,
};
