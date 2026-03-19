//! Agent-related shared types
//!
//! These types represent agent entities and are used by both
//! CLI commands and API routes for consistent data representation.

use crate::types::agent::AgentConfig;
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
