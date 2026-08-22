//! Single-agent execution runtime
//!
//! This module provides:
//! - Single agent runtime (`Agent` struct) — the core execution engine
//!   used by Principal root agents and the `Agent` subagent tool
//!
//! Note: after the principal-as-single-actor migration, agent
//! management surface (CRUD, .agent packaging) is gone. The only
//! "agent" concept that survives at the user-facing boundary is a
//! Principal; `Agent` here is the in-process execution primitive
//! that turns an `AGENT.md` prompt into a chat completion.
//! - Subagent spawning and management
//!
//! Sprint 9 Commit 4: `stateless_service` module retired. The
//! `StatelessAgentService` it provided was the sole
//! `PrincipalMessageService` impl; its only production caller was
//! the chat-gateway adapter framework deleted in Commit 3.

// Single agent runtime
mod agent;
pub use agent::Agent;

// Lifecycle management (tracks active executions only)
pub mod lifecycle;
pub use lifecycle::{ExecutionRecord, LifecycleManager};

// Agent configuration types (lifted from src/types/agent.rs in issue #31e)
pub mod agent_config;

// Subagent support
pub mod subagent_announce;
pub mod subagent_error;
pub mod subagent_executor;
pub mod subagent_recovery;
pub mod subagent_runtime_impl;
pub mod subagent_types;

// Re-export typed spawn error
pub use subagent_error::SpawnError;

#[cfg(test)]
mod tests;
