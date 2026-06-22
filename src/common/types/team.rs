//! Team-related shared types
//!
//! These types represent team entities and are used by both
//! CLI commands and API routes for consistent data representation.

pub use crate::registry::packaging::types::ExtensionRef;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::auth::principal::Principal;

/// Team metadata stored in team.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMetadata {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    /// Host runtime identifier for multi-host awareness (ADR-032)
    #[serde(default)]
    pub host_runtime_id: String,
    /// Owner identity for ownership and permission model (ADR-033, ADR-039).
    ///
    /// Canonical form is `owner = { kind, id }` (a `Principal`).
    #[serde(default)]
    pub owner: Principal,
    /// Explicit permission grants on this team (ADR-033)
    #[serde(default)]
    pub permissions: Vec<crate::auth::ownership::PermissionGrant>,
}


/// Team information for listing and display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamInfo {
    pub name: String,
    pub metadata: TeamMetadata,
    pub agent_count: usize,
    pub members: Vec<String>,
    pub path: PathBuf,
}

/// Team creation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamCreationResult {
    pub metadata: TeamMetadata,
    pub path: PathBuf,
}

/// Team deletion result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamDeletionResult {
    pub name: String,
    pub agents_deleted: usize,
}

/// Team move/rename result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMoveResult {
    pub old_name: String,
    pub new_name: String,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    pub agents_moved: usize,
}

/// Team export result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamExportResult {
    pub name: String,
    pub output_path: PathBuf,
    pub agent_count: usize,
}

/// Team import result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamImportResult {
    pub name: String,
    pub path: PathBuf,
    pub agents_imported: usize,
}

/// Team push result
#[derive(Debug, Clone)]
pub struct TeamPushResult {
    pub name: String,
    pub registry_ref: String,
    pub manifest_name: String,
    pub manifest_version: String,
    pub manifest_digest: String,
    pub kind: String,
    pub layers: usize,
    pub total_size: u64,
}

/// Team pull result
#[derive(Debug, Clone)]
pub struct TeamPullResult {
    pub registry_ref: String,
    pub name: String,
    pub path: PathBuf,
    pub agents_imported: usize,
    pub manifest_name: String,
    pub manifest_version: String,
    pub manifest_digest: String,
    pub manifest_kind: String,
    pub manifest_layers: usize,
    pub manifest_total_size: u64,
    pub extension_refs: Vec<crate::registry::packaging::types::ExtensionRef>,
}

/// Result of ensuring extensions for a pulled team.
#[derive(Debug, Default)]
pub struct TeamExtensionPullResult {
    pub pulled: Vec<String>,
    pub already_present: Vec<String>,
    pub failed: Vec<(String, String)>,
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
        if !self
            .enabled
            .iter()
            .any(|e| e.eq_ignore_ascii_case(capability))
        {
            self.enabled.push(capability.to_string());
        }
        self.disabled
            .retain(|e| !e.eq_ignore_ascii_case(capability));
    }

    /// Disable a capability
    pub fn disable(&mut self, capability: &str) {
        self.enabled.retain(|e| !e.eq_ignore_ascii_case(capability));
        if !self
            .disabled
            .iter()
            .any(|e| e.eq_ignore_ascii_case(capability))
        {
            self.disabled.push(capability.to_string());
        }
    }
}
