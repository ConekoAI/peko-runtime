//! Agent Manager - Unified agent lifecycle and coordination
//!
//! Provides decentralized agent management:
//! - Agent lifecycle (spawn, start, stop, restart)
//! - Agent pool management
//! - Discovery (who's available, what can they do)
//! - Context sharing (agents know about each other)
//! - Capability registry (local)
//! - Identity management
//! - Portable agent (export/import)
//!
//! Design principle: Agents coordinate themselves. The manager just provides
//! the context and messaging infrastructure.

#![allow(dead_code)]

pub mod commands;
pub mod context;
pub mod core;
pub mod lifecycle;
pub mod pool;
pub mod registry;
pub mod types;

// Re-exports for convenience
pub use commands::command_handler_loop;
pub use context::{AgentContext, AgentRegistryView, CapabilityIndex};
pub use core::AgentManager;
pub use lifecycle::LifecycleManager;
pub use pool::{AgentHandle, AgentPool, PoolAgentInfo, PoolConfig};
pub use registry::{CapabilityRecord, LocalRegistry, Registry, RegistryEvent};
pub use types::{AgentInfo, IdentityInfo, ManagerEvent};
