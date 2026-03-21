//! Agent-related shared types
//!
//! These types represent agent entities and are used by both
//! CLI commands and API routes for consistent agent data representation.

use crate::types::agent::AgentConfig;
use std::collections::HashMap;
use std::path::PathBuf;

/// Agent summary for listing operations
#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub name: String,
    pub team: String,
    pub config: AgentConfig,
    pub config_path: PathBuf,
}

/// Agent information with optional session details
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub name: String,
    pub team: String,
    pub config: AgentConfig,
    pub config_path: PathBuf,
    pub sessions_dir: PathBuf,
    pub session_count: usize,
}

/// Agent creation result
#[derive(Debug, Clone)]
pub struct AgentCreationResult {
    pub name: String,
    pub team: String,
    pub config_path: PathBuf,
    pub provider: String,
}

/// Agent rename/move result
#[derive(Debug, Clone)]
pub struct AgentRenameResult {
    pub old_name: String,
    pub new_name: String,
    pub from_team: String,
    pub to_team: String,
    pub new_config_path: PathBuf,
}

/// Agent creation request
#[derive(Debug, Clone)]
pub struct AgentCreateRequest {
    pub name: String,
    pub team: Option<String>,
    pub provider: String,
    pub model: Option<String>,
    pub description: Option<String>,
    pub auto_create_team: bool,
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

    pub fn with_auto_create_team(mut self, auto: bool) -> Self {
        self.auto_create_team = auto;
        self
    }

    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

/// Agent deletion options
#[derive(Debug, Clone, Default)]
pub struct AgentDeleteOptions {
    pub purge_identity: bool,
    pub force: bool,
}

/// Agent deletion result
#[derive(Debug, Clone)]
pub struct AgentDeleteResult {
    pub name: String,
    pub team: String,
    pub config_deleted: bool,
    pub sessions_deleted: bool,
}

/// Agent initialization request
#[derive(Debug, Clone)]
pub struct AgentInitRequest {
    pub path: PathBuf,
    pub name: Option<String>,
    pub provider: String,
    pub model: Option<String>,
    pub force: bool,
}

impl AgentInitRequest {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            name: None,
            provider: "openai".to_string(),
            model: None,
            force: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

/// Agent initialization result
#[derive(Debug, Clone)]
pub struct AgentInitResult {
    pub name: String,
    pub path: PathBuf,
    pub config_path: PathBuf,
    pub provider: String,
}

/// Agent update request
#[derive(Debug, Clone, Default)]
pub struct AgentUpdateRequest {
    pub image: Option<String>,
    pub team_id: Option<String>,
}

/// Agent export options
#[derive(Debug, Clone, Default)]
pub struct AgentExportOptions {
    pub output_path: Option<PathBuf>,
    pub encrypt: bool,
}

/// Agent export result
#[derive(Debug, Clone)]
pub struct AgentExportResult {
    pub name: String,
    pub team: String,
    pub output_path: PathBuf,
    pub encrypted: bool,
}

/// Agent import options
#[derive(Debug, Clone)]
pub struct AgentImportOptions {
    pub name: Option<String>,
    pub team: Option<String>,
}

/// Agent import result
#[derive(Debug, Clone)]
pub struct AgentImportResult {
    pub name: String,
    pub team: String,
    pub config_path: PathBuf,
}
