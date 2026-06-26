//! Agent-related shared types
//!
//! These types represent agent entities and are used by both
//! CLI commands and API routes for consistent agent data representation.

use crate::agents::agent_config::AgentConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Agent summary for listing operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub name: String,
    pub config: AgentConfig,
    pub config_path: PathBuf,
    pub memberships: Vec<String>,
}

/// Agent information with optional session details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub config: AgentConfig,
    pub config_path: PathBuf,
    pub sessions_dir: PathBuf,
    pub session_count: usize,
    pub memberships: Vec<String>,
    /// Resolved content of the first system prompt file, if configured.
    #[serde(default)]
    pub system_prompt: Option<String>,
}

/// Agent creation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCreationResult {
    pub name: String,
    pub config_path: PathBuf,
    pub provider: String,
}

/// Agent rename/move result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRenameResult {
    pub old_name: String,
    pub new_name: String,
    pub new_config_path: PathBuf,
}

/// Agent creation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCreateRequest {
    pub name: String,
    pub provider: String,
    pub model: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub host_runtime_id: Option<String>,
    /// Owner principal (e.g., `Subject::User("local:{runtime_did}")`).
    #[serde(default)]
    pub owner: Option<crate::auth::Subject>,
}

impl AgentCreateRequest {
    pub fn new(name: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            provider: provider.into(),
            model: None,
            description: None,
            force: false,
            host_runtime_id: None,
            owner: None,
        }
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
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    #[must_use]
    pub fn with_host_runtime_id(mut self, host_runtime_id: impl Into<String>) -> Self {
        self.host_runtime_id = Some(host_runtime_id.into());
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
    pub config_deleted: bool,
    pub sessions_deleted: bool,
}

/// Agent update request
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentUpdateRequest {
    pub image: Option<String>,
    /// Update the model (provider.default_model)
    pub model: Option<String>,
    /// Update the description
    pub description: Option<String>,
    /// Update the system prompt (writes to prompt.system.files)
    pub system_prompt: Option<String>,
    /// Merge arbitrary config values
    pub config: Option<serde_json::Value>,
}

/// Agent export options
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentExportOptions {
    pub output_path: Option<PathBuf>,
    pub include_sessions: bool,
    /// Embed extension packages in an `extensions/` layer (ADR-037).
    /// When true, each enabled non-built-in extension is exported as a
    /// `.ext` package inside the `.agent` archive.
    pub with_extensions: bool,
}

/// Agent export result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExportResult {
    pub name: String,
    pub output_path: PathBuf,
    pub encrypted: bool,
}

/// Agent import options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentImportOptions {
    pub name: Option<String>,
    pub force: bool,
    /// Allow importing an unsigned `.agent` package (issue #14).
    /// See [`crate::registry::packaging::ImportOptions::allow_unsigned`].
    #[serde(default)]
    pub allow_unsigned: bool,
}

/// Agent import result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentImportResult {
    pub name: String,
    pub config_path: PathBuf,
}

/// Agent push result
#[derive(Debug, Clone)]
pub struct AgentPushResult {
    pub local_tag: String,
    pub registry_ref: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub layers: usize,
    pub total_size: u64,
}

/// Agent pull result
#[derive(Debug, Clone)]
pub struct AgentPullResult {
    pub name: String,
    pub version: String,
    pub tag: String,
    pub output_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub manifest_digest: String,
    pub manifest_layers: usize,
    pub manifest_total_size: u64,
    pub extension_results: AgentExtensionPullResult,
}

/// Result of pulling extensions for an agent
#[derive(Debug, Clone, Default)]
pub struct AgentExtensionPullResult {
    pub pulled: Vec<String>,
    pub already_present: Vec<String>,
    pub failed: Vec<(String, String)>,
}
