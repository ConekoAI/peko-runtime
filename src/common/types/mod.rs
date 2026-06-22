//! Common types shared across CLI and API
//!
//! This module provides data structures that represent entities
//! in the Pekobot system, used by both CLI commands and API routes.
//!
//! The `src/types/` directory was merged into this module in issue #31e.

pub mod agent;
pub mod agent_legacy;
pub mod config;
pub mod membership;
pub mod message;
pub mod provider;
pub mod task;
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
