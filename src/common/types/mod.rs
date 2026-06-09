//! Common types shared across CLI and API
//!
//! This module provides data structures that represent entities
//! in the Pekobot system, used by both CLI commands and API routes.

pub mod agent;
pub mod membership;
pub mod team;

pub use agent::{
    AgentCreateRequest, AgentCreationResult, AgentDeleteOptions, AgentDeleteResult,
    AgentExportOptions, AgentExportResult, AgentImportOptions, AgentImportResult, AgentInfo,
    AgentRenameResult, AgentSummary, AgentUpdateRequest,
};
pub use membership::{
    AgentMembership, AgentMemberships, MembershipRole, TeamJoinResult, TeamLeaveResult, TeamMember,
    TeamMembers,
};
pub use team::{
    TeamCreationResult, TeamDeletionResult, TeamExportResult, TeamExtConfig, TeamImportResult,
    TeamInfo, TeamMetadata, TeamMoveResult,
};
