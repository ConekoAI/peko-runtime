//! Agent runtime and multi-agent management (Stateless Architecture)
//!
//! This module provides:
//! - Single agent runtime (Agent struct)
//! - Stateless agent management (StatelessAgentManager)
//! - Stateless execution service (StatelessAgentService)
//! - Subagent spawning and management

// Single agent runtime
mod agent;
pub use agent::Agent;

// Stateless manager (primary architecture)
pub mod stateless_manager;
pub use stateless_manager::{StatelessAgentManager, StatelessManagerEvent};

// Stateless execution service
pub mod stateless_service;
pub use stateless_service::{
    ExecutionContext, ExecutionRequest, ExecutionResult, StatelessAgentService,
};

// Lifecycle management (tracks active executions only)
pub mod lifecycle;
pub use lifecycle::{ExecutionRecord, LifecycleManager};

// Manager submodules
pub mod context;
pub mod types;

// Subagent support
pub mod announcement_service;
pub mod subagent_announce;
pub mod subagent_error;
pub mod subagent_executor;
pub mod subagent_recovery;
pub mod subagent_types;

// Re-export typed spawn error
pub use subagent_error::SpawnError;

// Async tool framework (re-exported from extensions::async_exec)
pub use crate::extension::async_exec::executor::{
    AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent, AsyncTaskEventBus,
    AsyncTaskReceipt, AsyncTaskRegistry, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig,
    CallbackDelivery, ChannelDelivery, DeliveryTarget, QueueDelivery, ResultDelivery,
    SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    AsyncExecutor, WaitResult,
};

// Re-export types for backward compatibility
pub use types::{AgentInfo, IdentityInfo, ManagerEvent};

// Context for agent execution
pub use context::AgentContext;
