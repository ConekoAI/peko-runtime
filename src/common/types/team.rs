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

/// Team move/rename result
#[derive(Debug, Clone)]
pub struct TeamMoveResult {
    pub old_name: String,
    pub new_name: String,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    pub agents_moved: usize,
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

/// Team export result
#[derive(Debug, Clone)]
pub struct TeamExportResult {
    pub name: String,
    pub output_path: PathBuf,
    pub agent_count: usize,
}

/// Team import result
#[derive(Debug, Clone)]
pub struct TeamImportResult {
    pub name: String,
    pub path: PathBuf,
    pub agents_imported: usize,
}

/// Team extension configuration (extensions.toml)
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamExtConfig {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl TeamExtConfig {
    /// Load from team extensions.toml
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content).unwrap_or_default())
    }

    /// Save to team extensions.toml
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Enable a capability
    pub fn enable(&mut self, capability: &str) {
        if !self.enabled.iter().any(|e| e.eq_ignore_ascii_case(capability)) {
            self.enabled.push(capability.to_string());
        }
        self.disabled.retain(|e| !e.eq_ignore_ascii_case(capability));
    }

    /// Disable a capability
    pub fn disable(&mut self, capability: &str) {
        self.enabled.retain(|e| !e.eq_ignore_ascii_case(capability));
        if !self.disabled.iter().any(|e| e.eq_ignore_ascii_case(capability)) {
            self.disabled.push(capability.to_string());
        }
    }
}
