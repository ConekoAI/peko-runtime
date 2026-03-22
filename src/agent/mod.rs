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

// Pool (used by command handler, deprecated but still needed)
#[deprecated(since = "0.2.0", note = "Stateless architecture does not use agent pools")]
pub mod pool;
#[deprecated(since = "0.2.0", note = "Stateless architecture does not use agent pools")]
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

// Re-export types for backward compatibility
pub use types::{AgentInfo, IdentityInfo, ManagerEvent};

// Context for agent execution
pub use context::AgentContext;
