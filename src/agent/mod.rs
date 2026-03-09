//! Agent runtime and multi-agent management
//!
//! This module provides:
//! - Single agent runtime (Agent struct)
//! - Multi-agent coordination (AgentManager)
//! - Agent lifecycle management
//! - Built-in channel implementations

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
pub use registry::{LocalRegistry, CapabilityRecord};

// Channel implementations
pub mod channels;

// Manager submodules
pub mod commands;
pub mod context;
pub mod types;

// Re-export manager components for convenience
pub use commands::command_handler_loop;
pub use context::{AgentContext, AgentRegistryView, CapabilityIndex};
pub use types::{AgentInfo, IdentityInfo, ManagerEvent};
