//! Agent Extension Type Implementation
//!
//! This module contains the Agent adapter for AGENT.md-based extensions.

pub mod adapter;

pub use adapter::{
    load_agents_from_directory, AgentAdapter, DiscoveredAgent, WorkspaceAgentsPromptHandler,
    AGENT_HOOK_PRIORITY,
};
