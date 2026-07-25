//! Agent Extension Type Implementation
//!
//! This module contains the Agent adapter for AGENT.md-based extensions.

pub mod adapter;

pub use adapter::{
    load_agents_from_directory, register_agents_with_core, AgentAdapter, DiscoveredAgent,
};
