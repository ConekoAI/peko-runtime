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
pub mod context;
pub mod cron_tool;
pub mod factory;
pub mod filesystem;
pub mod message_tool;
pub mod shell;
pub mod session_introspection;
pub mod sessions_send;
pub mod traits;

/// Universal Tool Protocol - Backend-agnostic tool integration
/// 
/// This module implements the custom tool system using JSON-RPC 2.0 over stdio.
/// It supports reserved parameter injection and works with any language.
pub mod universal;

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
pub use message_tool::{ChannelType, MessageConfig, MessageResult, MessageTool};
pub use shell::ShellTool;
pub use session_introspection::{
    AgentSessionRegistry, InMemorySessionRegistry, SessionInfo,
    SessionRegistry as SessionIntrospectionRegistry, SessionStatusTool, SessionsHistoryTool,
    SessionsListTool,
};
pub use traits::{Tool, ToolError, ToolResult};

// Async tool framework re-exports
pub use crate::agent::async_tool_framework::{
    AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent, AsyncTaskEventBus,
    AsyncTaskReceipt, AsyncTaskRegistry, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig,
    CallbackDelivery, ChannelDelivery, DeliveryTarget, QueueDelivery, ResultDelivery,
    SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    UnifiedAsyncExecutor, WaitResult,
};
