//! Single-agent execution runtime (Stateless Architecture)
//!
//! This module provides:
//! - Single agent runtime (`Agent` struct) — the core execution engine
//!   used by Principal supervisors and the `Agent` subagent tool
//! - Stateless agent management (`StatelessAgentManager`)
//!
//! Note: after the principal-as-single-actor migration, agent
//! management surface (CRUD, .agent packaging) is gone. The only
//! "agent" concept that survives at the user-facing boundary is a
//! Principal; `Agent` here is the in-process execution primitive
//! that turns an `AGENT.md` prompt into a chat completion.
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

// System prompt generation (absorbed from src/prompt/ in issue #31a)
pub mod prompt;

// Agent configuration types (lifted from src/types/agent.rs in issue #31e)
pub mod agent_config;

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
pub use crate::extensions::framework::async_exec::executor::{
    AsyncExecutor, AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskCompletionEvent,
    AsyncTaskEventBus, AsyncTaskReceipt, AsyncTaskRegistry, AsyncTaskResult, AsyncTaskStatus,
    AsyncToolConfig, CallbackDelivery, ChannelDelivery, DeliveryTarget, QueueDelivery,
    ResultDelivery, SessionMessageType, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry,
    WaitResult,
};

// Re-export types for backward compatibility
pub use types::{AgentInfo, IdentityInfo, ManagerEvent};

// Context for agent execution
pub use context::AgentContext;

#[cfg(test)]
mod tests;
