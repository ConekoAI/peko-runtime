//! Unified Agent Configuration Entry
//!
//! This module defines the canonical `AgentConfigEntry` type used throughout
//! the agent configuration system. It replaces the duplicate definitions
//! previously found in `AgentConfigService` and `ConfigRegistry`.

use crate::agents::agent_config::AgentConfig;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Source of agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigSource {
    /// Configuration loaded from an image
    Image {
        /// Image reference
        image_ref: String,
        /// Image digest
        image_digest: String,
    },
    /// Configuration created directly (e.g., via CLI or API)
    Direct {
        /// Reason/source of creation
        reason: String,
    },
}

impl ConfigSource {
    /// Get image reference (if from image)
    #[must_use]
    pub fn image_ref(&self) -> Option<&str> {
        match self {
            ConfigSource::Image { image_ref, .. } => Some(image_ref),
            ConfigSource::Direct { .. } => None,
        }
    }

    /// Get image digest (if from image)
    #[must_use]
    pub fn image_digest(&self) -> Option<&str> {
        match self {
            ConfigSource::Image { image_digest, .. } => Some(image_digest),
            ConfigSource::Direct { .. } => None,
        }
    }
}

impl Default for ConfigSource {
    fn default() -> Self {
        ConfigSource::Direct {
            reason: "default".to_string(),
        }
    }
}

/// Unified agent configuration entry
///
/// This is the canonical entry type used by `ConfigAuthority` for all
/// agent configuration operations. It combines configuration data with
/// metadata such as source and timestamps.
///
/// Agents are standalone first-class citizens (ADR-031). They live at
/// `agents/{agent}/config.toml` and team membership is tracked separately
/// via `memberships.toml`. There is no single "team" that an agent belongs to.
///
/// # Fields
/// - `name`: Agent name (globally unique)
/// - `config`: The agent configuration itself
/// - `config_path`: Canonical path to TOML config file
/// - `source`: Optional source (image/direct) - for backward compat
/// - `registered_at`: Optional registration timestamp
/// - `updated_at`: Optional last update timestamp
#[derive(Debug, Clone)]
pub struct AgentConfigEntry {
    /// Agent name
    pub name: String,
    /// Agent configuration
    pub config: AgentConfig,
    /// Config file path (always TOML in canonical location)
    pub config_path: PathBuf,
    /// Source of configuration (optional for backward compatibility)
    pub source: Option<ConfigSource>,
    /// Registration timestamp (optional for legacy entries)
    pub registered_at: Option<DateTime<Utc>>,
    /// Last updated timestamp (optional for legacy entries)
    pub updated_at: Option<DateTime<Utc>>,
}

impl AgentConfigEntry {
    /// Get image reference (backward compatibility)
    #[must_use]
    pub fn image_ref(&self) -> &str {
        self.source
            .as_ref()
            .and_then(|s| s.image_ref())
            .unwrap_or("direct")
    }

    /// Get image digest (backward compatibility)
    #[must_use]
    pub fn image_digest(&self) -> &str {
        self.source
            .as_ref()
            .and_then(|s| s.image_digest())
            .unwrap_or("direct")
    }
}
