//! Agent runtime and multi-agent management
//!
//! This module provides:
//! - Single agent runtime (Agent struct)
//! - Multi-agent coordination (AgentManager)
//! - Agent lifecycle management
//! - Subagent spawning and management

// Single agent runtime
mod agent;
pub use agent::Agent;

// Multi-agent management (merged from manager/)
pub mod manager;
pub use manager::AgentManager;

pub mod pool;
pub use pool::{AgentHandle, AgentPool, PoolConfig};

pub mod lifecycle;
pub use lifecycle::LifecycleManager;

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
    SharedAsyncResultQueueManager, SharedAsyncTaskRegistry, UnifiedAsyncExecutor, WaitResult,
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
