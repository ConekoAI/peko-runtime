//! Agent-related shared types
//!
//! These types represent agent entities and are used by both
//! CLI commands and API routes for consistent agent data representation.

use crate::types::agent::AgentConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Agent summary for listing operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub name: String,
    pub team: String,
    pub config: AgentConfig,
    pub config_path: PathBuf,
}

/// Agent information with optional session details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub team: String,
    pub config: AgentConfig,
    pub config_path: PathBuf,
    pub sessions_dir: PathBuf,
    pub session_count: usize,
}

/// Agent creation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCreationResult {
    pub name: String,
    pub team: String,
    pub config_path: PathBuf,
    pub provider: String,
}

/// Agent rename/move result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRenameResult {
    pub old_name: String,
    pub new_name: String,
    pub from_team: String,
    pub to_team: String,
    pub new_config_path: PathBuf,
}

/// Agent creation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCreateRequest {
    pub name: String,
    pub team: Option<String>,
    pub provider: String,
    pub model: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub auto_create_team: bool,
    #[serde(default)]
    pub force: bool,
}

impl AgentCreateRequest {
    pub fn new(name: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            team: None,
            provider: provider.into(),
            model: None,
            description: None,
            auto_create_team: true,
            force: false,
        }
    }

    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    #[must_use]
    pub fn with_auto_create_team(mut self, auto: bool) -> Self {
        self.auto_create_team = auto;
        self
    }

    #[must_use]
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

/// Agent deletion options
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentDeleteOptions {
    pub purge_identity: bool,
    pub force: bool,
}

/// Agent deletion result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDeleteResult {
    pub name: String,
    pub team: String,
    pub config_deleted: bool,
    pub sessions_deleted: bool,
}

/// Agent update request
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentUpdateRequest {
    pub image: Option<String>,
    pub team_id: Option<String>,
}

/// Agent export options
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentExportOptions {
    pub output_path: Option<PathBuf>,
    pub include_sessions: bool,
}

/// Agent export result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExportResult {
    pub name: String,
    pub team: String,
    pub output_path: PathBuf,
    pub encrypted: bool,
}

/// Agent import options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentImportOptions {
    pub name: Option<String>,
    pub team: Option<String>,
    pub force: bool,
}

/// Agent import result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentImportResult {
    pub name: String,
    pub team: String,
    pub config_path: PathBuf,
}
