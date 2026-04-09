//! Tools for agents
//!
//! Core tools only. Heavy tools (web_search, fetch, http, browser, memory)
//! have been migrated to standalone MCP servers:
//! - mcp-web: web_search, fetch, http
//! - mcp-browser: browser automation  
//! - mcp-memory: persistent memory storage
//!
//! Custom tools use the Universal Tool Protocol (see `universal` module).

pub mod agent_management;
pub mod agent_spawn;
pub mod apply_patch;
pub mod async_tool;
pub mod context;
pub mod cron_tool;
pub mod factory;
pub mod filesystem;
pub mod glob;
pub mod grep;
pub mod message_tool;
pub mod read_file;
pub mod session_introspection;
pub mod sessions_send;
pub mod shell;
pub mod str_replace_file;
pub mod traits;
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

pub use sessions_send::{SendMode, SessionsSendTool};

pub use agent_management::{AgentInfoTool, AgentsListTool, ManagerCommand};
pub use agent_spawn::{AgentSpawnListTool, AgentSpawnStatusTool, AgentSpawnTool};
pub use apply_patch::{ApplyPatchConfig, ApplyPatchTool};
pub use context::{
    wrap_tool, AbortSignal, AbortableTool, ToolAdapter, ToolContext, ToolWithContext,
};
pub use cron_tool::CronTool;
pub use factory::{McpDiscoveryResult, McpFactoryConfig, ToolFactory, ToolFactoryConfig};
pub use filesystem::FileSystemTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use message_tool::{ChannelType, MessageConfig, MessageResult, MessageTool};
pub use read_file::ReadFileTool;
pub use shell::ShellTool;
pub use session_introspection::{
    AgentSessionRegistry, InMemorySessionRegistry, SessionInfo,
    SessionRegistry as SessionIntrospectionRegistry, SessionStatusTool, SessionsHistoryTool,
    SessionsListTool,
};
pub use str_replace_file::StrReplaceFileTool;
pub use traits::{Tool, ToolError, ToolResult};
pub use write_file::WriteFileTool;

// Async tool trait re-exports
pub use async_tool::{
    into_async_tool, BoxedAsyncTool, SyncToAsyncAdapter, ToolAsyncExt, UnifiedAsyncTool,
};

// Async tool framework re-exports
pub use crate::agent::async_tool_framework::{
    AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent, AsyncTaskEventBus,
    AsyncTaskReceipt, AsyncTaskRegistry, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig,
    CallbackDelivery, ChannelDelivery, DeliveryTarget, QueueDelivery, ResultDelivery,
    SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    UnifiedAsyncExecutor, WaitResult,
};
