//! Agent Configuration Service
//!
//! Provides unified agent configuration management for both CLI and HTTP API.
//!
//! ## Deprecation Notice
//!
//! This service is now a thin wrapper around `ConfigAuthorityImpl`.
//! New code should use `ConfigAuthority` or `ConfigAuthorityImpl` directly.
//!
//! The canonical TOML location is:
//! `~/.pekobot/teams/{team}/agents/{agent}/config.toml`
//!
//! ## Architecture
//!
//! - Uses `PathResolver` for consistent path resolution
//! - Delegates to `ConfigAuthorityImpl` for all operations
//! - All operations are async and team-aware
//! - Used by both CLI commands and HTTP API routes
//!
//! ## API Key Resolution
//!
//! When an agent config doesn't have a hardcoded API key, the service resolves
//! it dynamically at runtime using this priority:
//! 1. credentials.json (set via `pekobot auth set <provider>`)
//! 2. Environment variable (e.g., KIMI_API_KEY)
//!
//! This allows agent configs to be shared without embedding sensitive credentials.

use crate::common::paths::PathResolver;
use crate::common::services::config_authority::{ConfigAuthority, ConfigAuthorityImpl};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

// Re-export AgentConfigEntry for backward compatibility
pub use crate::common::services::config_authority::AgentConfigEntry;

/// Agent configuration service - thin wrapper around ConfigAuthorityImpl
///
/// This service is kept for backward compatibility. New code should use
/// `ConfigAuthorityImpl` directly.
pub struct AgentConfigService {
    authority: Arc<ConfigAuthorityImpl>,
}

impl std::fmt::Debug for AgentConfigService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfigService")
            .field("authority", &"ConfigAuthorityImpl")
            .finish_non_exhaustive()
    }
}

impl Clone for AgentConfigService {
    fn clone(&self) -> Self {
        Self {
            authority: self.authority.clone(),
        }
    }
}

impl AgentConfigService {
    /// Create a new agent configuration service
    pub fn new(path_resolver: PathResolver) -> Self {
        Self {
            authority: Arc::new(ConfigAuthorityImpl::new(path_resolver)),
        }
    }

    /// Get agent configuration
    ///
    /// If team is None, searches all teams for the agent.
    pub async fn get(
        &self,
        agent_name: &str,
        team: Option<&str>,
    ) -> Result<Option<AgentConfigEntry>> {
        self.authority.get(agent_name, team).await?;
        // Map ConfigError to anyhow::Error if needed
        Ok(self.authority.get(agent_name, team).await?)
    }

    /// Save agent configuration to TOML file
    ///
    /// Creates parent directories if they don't exist.
    /// Updates the cache after saving.
    pub async fn save(
        &self,
        agent_name: &str,
        team: &str,
        config: &crate::types::agent::AgentConfig,
    ) -> Result<PathBuf> {
        Ok(self.authority.save(agent_name, team, config).await?)
    }

    /// Check if an agent exists
    pub async fn exists(&self, agent_name: &str, team: Option<&str>) -> Result<bool> {
        Ok(self.authority.exists(agent_name, team).await?)
    }

    /// List all agents in a team
    pub async fn list_in_team(&self, team: &str) -> Result<Vec<AgentConfigEntry>> {
        Ok(self.authority.list_in_team(team).await?)
    }

    /// List all agents across all teams
    pub async fn list_all(&self) -> Result<Vec<AgentConfigEntry>> {
        Ok(self.authority.list_all().await?)
    }

    /// Delete an agent configuration
    ///
    /// Removes from cache and deletes the config file.
    /// Note: This does NOT delete the agent directory or sessions.
    pub async fn delete(&self, agent_name: &str, team: &str) -> Result<bool> {
        Ok(self.authority.delete(agent_name, team).await?)
    }

    /// Clear the configuration cache
    pub async fn clear_cache(&self) {
        self.authority.clear_cache().await;
    }

    /// Invalidate a specific entry in the cache
    pub async fn invalidate_cache(&self, agent_name: &str, team: &str) {
        self.authority.invalidate_cache(agent_name, team).await;
    }

    /// Get the underlying ConfigAuthority implementation
    ///
    /// This allows accessing additional methods if needed.
    pub fn authority(&self) -> &Arc<ConfigAuthorityImpl> {
        &self.authority
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_agent_config_service_save_and_get() {
        let temp_dir = TempDir::new().unwrap();
        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().to_path_buf(),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let service = AgentConfigService::new(path_resolver);

        // Create a test config
        let config = crate::types::agent::AgentConfig::default();

        // Save it
        let path = service
            .save("test-agent", "default", &config)
            .await
            .unwrap();
        assert!(path.exists());

        // Retrieve it
        let entry = service.get("test-agent", Some("default")).await.unwrap();
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.name, "test-agent");
        assert_eq!(entry.team, "default");
    }

    #[tokio::test]
    async fn test_agent_config_service_exists() {
        let temp_dir = TempDir::new().unwrap();
        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().to_path_buf(),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let service = AgentConfigService::new(path_resolver);

        // Non-existent agent
        assert!(!service
            .exists("nonexistent", Some("default"))
            .await
            .unwrap());

        // Create and check
        let config = crate::types::agent::AgentConfig::default();
        service
            .save("existing", "default", &config)
            .await
            .unwrap();
        assert!(service.exists("existing", Some("default")).await.unwrap());
    }
}
