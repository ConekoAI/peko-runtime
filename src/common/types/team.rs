//! Team-related shared types
//!
//! These types represent team entities and are used by both
//! CLI commands and API routes for consistent data representation.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Team metadata stored in team.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMetadata {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
}

/// Team information for listing and display
#[derive(Debug, Clone)]
pub struct TeamInfo {
    pub name: String,
    pub metadata: Option<TeamMetadata>,
    pub agent_count: usize,
    pub path: PathBuf,
}

/// Team creation result
#[derive(Debug, Clone)]
pub struct TeamCreationResult {
    pub metadata: TeamMetadata,
    pub path: PathBuf,
}

/// Team deletion result
#[derive(Debug, Clone)]
pub struct TeamDeletionResult {
    pub name: String,
    pub agents_deleted: usize,
}

/// Team runtime information (for runtime/deployed teams)
#[derive(Debug, Clone)]
pub struct TeamRuntimeInfo {
    pub id: String,
    pub name: String,
    pub status: TeamRuntimeStatus,
    pub agent_count: usize,
    pub instance_ids: Vec<String>,
    pub created_at: String,
}

/// Team runtime status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamRuntimeStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Error,
}

impl std::fmt::Display for TeamRuntimeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamRuntimeStatus::Starting => write!(f, "starting"),
            TeamRuntimeStatus::Running => write!(f, "running"),
            TeamRuntimeStatus::Stopping => write!(f, "stopping"),
            TeamRuntimeStatus::Stopped => write!(f, "stopped"),
            TeamRuntimeStatus::Error => write!(f, "error"),
        }
    }
}

/// Team deployment request
#[derive(Debug, Clone)]
pub struct TeamDeployRequest {
    pub name: String,
    pub config_source: TeamConfigSource,
}

/// Source of team configuration
#[derive(Debug, Clone)]
pub enum TeamConfigSource {
    /// Load from file path
    FilePath(PathBuf),
    /// Inline configuration
    Inline { agents: Vec<TeamAgentDefinition> },
}

/// Agent definition for team deployment
#[derive(Debug, Clone)]
pub struct TeamAgentDefinition {
    pub name: String,
    pub image: String,
    pub instances: u32,
    pub role: Option<String>,
}

/// Team deployment result
#[derive(Debug, Clone)]
pub struct TeamDeployResult {
    pub id: String,
    pub name: String,
    pub status: String,
    pub agent_count: usize,
    pub instance_ids: Vec<String>,
}

/// Team scale request
#[derive(Debug, Clone)]
pub struct TeamScaleRequest {
    pub team_id: String,
    pub agent_name: String,
    pub instances: u32,
}

/// Team scale result
#[derive(Debug, Clone)]
pub struct TeamScaleResult {
    pub team_id: String,
    pub agent_name: String,
    pub previous_count: u32,
    pub new_count: u32,
    pub added_instance_ids: Vec<String>,
    pub removed_instance_ids: Vec<String>,
}
