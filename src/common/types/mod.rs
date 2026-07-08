//! Common types shared across CLI and API
//!
//! This module provides data structures that represent entities
//! in the Peko system, used by both CLI commands and API routes.
//!
//! The `src/types/` directory was merged into this module in issue #31e.

pub mod a2a;
pub mod agent;
pub mod agent_legacy;
pub mod config;
pub mod extension;
pub mod message;
pub mod output_format;
pub mod provider;
pub mod task;

pub use output_format::OutputFormat;

pub use agent::{
    AgentCreateRequest, AgentCreationResult, AgentDeleteOptions, AgentDeleteResult, AgentInfo,
    AgentRenameResult, AgentSummary, AgentUpdateRequest,
};
