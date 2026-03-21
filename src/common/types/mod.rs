//! Common types shared across CLI and API
//!
//! This module provides data structures that represent entities
//! in the Pekobot system, used by both CLI commands and API routes.

pub mod agent;
pub mod team;

pub use agent::{
    AgentCreateRequest, AgentCreationResult, AgentDeleteOptions, AgentDeleteResult,
    AgentExportOptions, AgentExportResult, AgentImportOptions, AgentImportResult, AgentInfo,
    AgentInitRequest, AgentInitResult, AgentRenameResult, AgentSummary, AgentUpdateRequest,
};
pub use team::{
    TeamAgentDefinition, TeamConfigSource, TeamCreationResult, TeamDeletionResult,
    TeamDeployRequest, TeamDeployResult, TeamInfo, TeamMetadata, TeamRuntimeInfo,
    TeamRuntimeStatus, TeamScaleRequest, TeamScaleResult,
};
