//! Common types shared across CLI and API
//!
//! This module provides data structures that represent entities
//! in the Pekobot system, used by both CLI commands and API routes.

pub mod agent;
pub mod team;

pub use agent::{AgentCreationResult, AgentInfo, AgentRenameResult, AgentSummary};
pub use team::{TeamCreationResult, TeamDeletionResult, TeamInfo, TeamMetadata};
