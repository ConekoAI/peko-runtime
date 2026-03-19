//! Agent runtime and multi-agent management (Stateless Architecture)
#![allow(deprecated)]
//!
//! This module provides:
//! - Single agent runtime (Agent struct)
//! - Stateless agent management (StatelessAgentManager)
//! - Configuration registry (ConfigRegistry)
//! - Stateless execution service (StatelessAgentService)
//! - Subagent spawning and management

// Single agent runtime
mod agent;
pub use agent::Agent;

// Stateless manager (primary architecture)
pub mod stateless_manager;
pub use stateless_manager::{StatelessAgentManager, StatelessManagerEvent};

// State management for stateless architecture
pub mod config_registry;
pub mod stateless_service;

pub use config_registry::{AgentConfigEntry, ConfigRegistry};
pub use stateless_service::{
    ExecutionContext, ExecutionRequest, ExecutionResult, StatelessAgentService,
};

// Lifecycle management (tracks active executions only)
pub mod lifecycle;
pub use lifecycle::{ExecutionRecord, LifecycleManager};

// Legacy components (deprecated, will be removed in future release)
#[deprecated(since = "0.2.0", note = "Use StatelessAgentManager instead")]
pub mod manager;
#[deprecated(since = "0.2.0", note = "Use StatelessAgentManager instead")]
pub use manager::AgentManager;

#[deprecated(
    since = "0.2.0",
    note = "Stateless architecture does not use agent pools"
)]
pub mod pool;
#[deprecated(
    since = "0.2.0",
    note = "Stateless architecture does not use agent pools"
)]
pub use pool::{AgentHandle, AgentPool, PoolConfig};

pub mod registry;
pub use registry::{CapabilityRecord, LocalRegistry};

// Manager submodules
pub mod commands;
pub mod context;
pub mod types;

// Subagent support
pub mod announcement_service;
pub mod subagent_announce;
pub mod subagent_executor;
pub mod subagent_registry;

// Async tool framework
pub mod async_tool_framework;
pub use async_tool_framework::{
    AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent, AsyncTaskEventBus,
    AsyncTaskReceipt, AsyncTaskRegistry, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig,
    CallbackDelivery, ChannelDelivery, DeliveryTarget, QueueDelivery, ResultDelivery,
    SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    UnifiedAsyncExecutor, WaitResult,
};

// Deprecated: AsyncTool trait is deprecated, use UnifiedAsyncExecutor directly
#[allow(deprecated)]
pub use async_tool_framework::AsyncTool;

// Re-export manager components for convenience
pub use commands::command_handler_loop;
pub use context::{AgentContext, AgentRegistryView, CapabilityIndex};
pub use types::{AgentInfo, IdentityInfo, ManagerEvent};

// Re-export subagent components
pub use announcement_service::{AnnouncementService, ChannelAnnouncementService};
pub use subagent_announce::{
    announce_to_parent, build_subagent_system_prompt, build_subagent_task_message,
    format_announcement, handle_cleanup, on_subagent_complete,
};
pub use subagent_executor::{
    AnnouncementReceiver, AnnouncementSender, BackgroundTaskManager, CompletedRun, ExecutionConfig,
    SubagentExecutor,
};
pub use subagent_registry::{
    create_shared_registry, SharedSubagentRegistry, SubagentRegistry, SubagentResult, SubagentRun,
    SubagentStatus,
};

#[cfg(test)]
mod tests;
