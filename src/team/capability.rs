//! Team-Level Capability Management
//!
//! Manages team-wide capability configuration stored in `teams/{team}/capabilities.toml`.
//! Team capabilities provide defaults that agents inherit if they don't override.

use crate::common::paths::PathResolver;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Team-level capability configuration
///
/// Stored in `teams/{team}/capabilities.toml`
/// Provides team-wide defaults that agents inherit.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamCapabilityConfig {
    /// Team-wide enabled capabilities (whitelist)
    /// If empty, falls back to system default
    #[serde(default)]
    pub enabled: Vec<String>,

    /// Team-wide disabled capabilities (cannot be overridden by agents)
    /// This is a deny-list - these capabilities are blocked for all team agents
    #[serde(default)]
    pub disabled: Vec<String>,

    /// Per-capability settings (e.g., rate limits, timeouts)
    #[serde(default)]
    pub settings: HashMap<String, CapabilitySetting>,
}

impl TeamCapabilityConfig {
    /// Load team capability config from team directory
    pub fn load(team_dir: &PathBuf) -> anyhow::Result<Option<Self>> {
        let cap_config_path = team_dir.join("capabilities.toml");

        if !cap_config_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&cap_config_path)?;
        let config: TeamCapabilityConfig = toml::from_str(&content)?;
        Ok(Some(config))
    }

    /// Save team capability config to team directory
    pub fn save(&self, team_dir: &PathBuf) -> anyhow::Result<()> {
        let cap_config_path = team_dir.join("capabilities.toml");
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&cap_config_path, content)?;
        Ok(())
    }

    /// Enable a capability for the team
    pub fn enable(&mut self, cap_name: &str) {
        if !self.enabled.contains(&cap_name.to_string()) {
            self.enabled.push(cap_name.to_string());
        }
        // Remove from disabled if it was there (enable overrides disable)
        self.disabled.retain(|c| c != cap_name);
    }

    /// Disable a capability for the team
    pub fn disable(&mut self, cap_name: &str) {
        if !self.disabled.contains(&cap_name.to_string()) {
            self.disabled.push(cap_name.to_string());
        }
        // Remove from enabled if it was there
        self.enabled.retain(|c| c != cap_name);
    }

    /// Check if a capability is team-wide enabled
    ///
    /// Returns true if:
    /// - The capability is NOT in the disabled list AND
    /// - The enabled list is empty (no restrictions) OR the capability is in the enabled list
    ///
    /// Note: Empty enabled list means "no team-level restrictions" (agents can use their own config).
    /// If you want to restrict, explicitly add to disabled list or leave enabled empty but
    /// rely on agents' own configs.
    #[must_use]
    pub fn is_enabled(&self, cap_name: &str) -> bool {
        if self.disabled.contains(&cap_name.to_string()) {
            return false;
        }
        // Empty enabled list = no team-level restrictions
        self.enabled.is_empty()
            || self
                .enabled
                .iter()
                .any(|c| c.eq_ignore_ascii_case(cap_name))
    }
}

/// Per-capability settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySetting {
    /// Whether this capability is enabled (can be overridden at agent level)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Custom timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Custom rate limit (requests per minute)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u64>,
}

fn default_true() -> bool {
    true
}

/// Team Capability Manager
///
/// Handles team-level capability operations.
pub struct TeamCapabilityManager {
    path_resolver: PathResolver,
}

impl TeamCapabilityManager {
    /// Create a new manager
    #[must_use]
    pub fn new(path_resolver: PathResolver) -> Self {
        Self { path_resolver }
    }

    /// Get the team directory path
    fn team_dir(&self, team: &str) -> PathBuf {
        self.path_resolver.team_dir(team)
    }

    /// Load existing config or create new one
    fn load_or_create(&self, team: &str) -> anyhow::Result<TeamCapabilityConfig> {
        let team_dir = self.team_dir(team);
        match TeamCapabilityConfig::load(&team_dir)? {
            Some(config) => Ok(config),
            None => Ok(TeamCapabilityConfig::default()),
        }
    }

    /// Enable a capability for a team
    pub fn enable(&self, team: &str, cap: &str) -> anyhow::Result<()> {
        let mut config = self.load_or_create(team)?;
        config.enable(cap);
        config.save(&self.team_dir(team))?;
        println!("Enabled '{cap}' for team '{team}'");
        Ok(())
    }

    /// Disable a capability for a team
    pub fn disable(&self, team: &str, cap: &str) -> anyhow::Result<()> {
        let mut config = self.load_or_create(team)?;
        config.disable(cap);
        config.save(&self.team_dir(team))?;
        println!("Disabled '{cap}' for team '{team}'");
        Ok(())
    }

    /// List team capabilities
    pub fn list(&self, team: &str) -> anyhow::Result<Option<TeamCapabilityConfig>> {
        let team_dir = self.team_dir(team);
        TeamCapabilityConfig::load(&team_dir)
    }

    /// Check if a capability is enabled for a team
    pub fn is_enabled(&self, team: &str, cap: &str) -> anyhow::Result<bool> {
        match self.load_or_create(team) {
            Ok(config) => Ok(config.is_enabled(cap)),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enable_disable() {
        let mut config = TeamCapabilityConfig::default();

        // Initially empty config means no team-level restrictions (agents use their own config)
        // is_enabled returns true because empty enabled = "no restrictions"
        assert!(config.is_enabled("shell"));

        // Enable shell
        config.enable("shell");
        assert!(config.is_enabled("shell"));

        // Disable shell
        config.disable("shell");
        assert!(!config.is_enabled("shell"));
    }

    #[test]
    fn test_disable_overrides_enable() {
        let mut config = TeamCapabilityConfig::default();

        config.enable("shell");
        config.disable("shell");

        // Disabled takes precedence
        assert!(!config.is_enabled("shell"));
    }
}
