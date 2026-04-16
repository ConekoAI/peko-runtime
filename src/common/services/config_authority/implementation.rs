//! `ConfigAuthority` implementation
//!
//! The main implementation of the `ConfigAuthority` trait.

use super::authority_trait::{ConfigAuthority, ConfigError, ConfigResult};
use super::cache::ConfigCache;
use super::entry::{AgentConfigEntry, ConfigSource};
use super::io::{ApiKeyResolver, ConfigIo};
use crate::common::paths::PathResolver;
use crate::types::agent::AgentConfig;
use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Main implementation of `ConfigAuthority`
///
/// This is the single canonical implementation for agent configuration management.
/// It coordinates between:
/// - `ConfigCache`: In-memory caching
/// - `ConfigIo`: TOML file operations
/// - `ApiKeyResolver`: API key resolution
#[derive(Debug)]
pub struct ConfigAuthorityImpl {
    path_resolver: PathResolver,
    cache: ConfigCache,
    io: ConfigIo,
    api_key_resolver: ApiKeyResolver,
}

impl std::fmt::Display for ConfigAuthorityImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConfigAuthorityImpl")
    }
}

impl ConfigAuthorityImpl {
    /// Create a new `ConfigAuthorityImpl`
    #[must_use] 
    pub fn new(path_resolver: PathResolver) -> Self {
        let config_dir = path_resolver.config_dir().to_path_buf();
        Self {
            path_resolver,
            cache: ConfigCache::new(),
            io: ConfigIo::new(),
            api_key_resolver: ApiKeyResolver::new(config_dir),
        }
    }

    /// Create from existing components (for testing)
    #[allow(dead_code)]
    fn with_components(
        path_resolver: PathResolver,
        cache: ConfigCache,
        io: ConfigIo,
        api_key_resolver: ApiKeyResolver,
    ) -> Self {
        Self {
            path_resolver,
            cache,
            io,
            api_key_resolver,
        }
    }

    /// Get the canonical config path for an agent
    pub fn config_path(&self, agent_name: &str, team: Option<&str>) -> PathBuf {
        self.path_resolver.agent_config(agent_name, team)
    }

    /// Find an agent by searching all teams
    async fn find_agent_in_teams(
        &self,
        agent_name: &str,
    ) -> ConfigResult<Option<(PathBuf, String)>> {
        let teams_dir = self.path_resolver.teams_dir();

        if !teams_dir.exists() {
            return Ok(None);
        }

        let mut entries = match tokio::fs::read_dir(&teams_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ConfigError::Io(e));
            }
        };

        while let Some(entry) = entries.next_entry().await.map_err(ConfigError::Io)? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let team_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            let config_path = self.config_path(agent_name, Some(team_name));
            if config_path.exists() {
                return Ok(Some((config_path, team_name.to_string())));
            }
        }

        Ok(None)
    }
}

#[async_trait]
impl ConfigAuthority for ConfigAuthorityImpl {
    async fn get(
        &self,
        agent_name: &str,
        team: Option<&str>,
    ) -> ConfigResult<Option<AgentConfigEntry>> {
        let team_name = team.unwrap_or("default");

        // Check cache first
        if let Some(entry) = self.cache.get(team_name, agent_name).await {
            debug!(
                "Cache hit for agent '{}' in team '{}'",
                agent_name, team_name
            );
            return Ok(Some(entry));
        }

        // Try to find the agent in the specified team, or search all teams
        let (config_path, found_team) = if team.is_some() {
            let path = self.config_path(agent_name, team);
            if !path.exists() {
                return Ok(None);
            }
            (path, team_name.to_string())
        } else {
            // Search all teams for this agent
            match self.find_agent_in_teams(agent_name).await? {
                Some((path, found_team)) => (path, found_team),
                None => return Ok(None),
            }
        };

        // Load from disk
        let mut config = self.io.load_toml(&config_path).await.map_err(|e| {
            ConfigError::Other(format!(
                "Failed to load config from {}: {}",
                config_path.display(),
                e
            ))
        })?;

        // Resolve API key if not set in config
        self.api_key_resolver.resolve_config_api_key(&mut config);

        let entry = AgentConfigEntry {
            name: agent_name.to_string(),
            team: found_team,
            config,
            config_path,
            source: Some(ConfigSource::Direct {
                reason: "file".to_string(),
            }),
            registered_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        };

        // Cache the result
        self.cache.insert(&entry).await;

        Ok(Some(entry))
    }

    async fn save(
        &self,
        agent_name: &str,
        team: &str,
        config: &AgentConfig,
    ) -> ConfigResult<PathBuf> {
        let config_path = self.config_path(agent_name, Some(team));

        // Save to TOML
        self.io
            .save_toml(&config_path, config)
            .await
            .map_err(|e| ConfigError::Other(format!("Failed to save config: {e}")))?;

        info!(
            "Saved agent '{}' config to team '{}' at {}",
            agent_name,
            team,
            config_path.display()
        );

        // Create entry and cache it
        let entry = AgentConfigEntry {
            name: agent_name.to_string(),
            team: team.to_string(),
            config: config.clone(),
            config_path: config_path.clone(),
            source: Some(ConfigSource::Direct {
                reason: "saved".to_string(),
            }),
            registered_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        };

        self.cache.insert(&entry).await;

        Ok(config_path)
    }

    async fn exists(&self, agent_name: &str, team: Option<&str>) -> ConfigResult<bool> {
        match self.get(agent_name, team).await {
            Ok(Some(_)) => Ok(true),
            Ok(None) => {
                // Double-check by looking at file system
                let config_path = self.config_path(agent_name, team);
                Ok(config_path.exists())
            }
            Err(e) => Err(e),
        }
    }

    async fn list_in_team(&self, team: &str) -> ConfigResult<Vec<AgentConfigEntry>> {
        let agents_dir = self.path_resolver.agents_dir(Some(team));
        let mut agents = Vec::new();

        if !agents_dir.exists() {
            return Ok(agents);
        }

        let mut entries = match tokio::fs::read_dir(&agents_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(agents),
            Err(e) => return Err(ConfigError::Io(e)),
        };

        while let Some(entry) = entries.next_entry().await.map_err(ConfigError::Io)? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let agent_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            match self.get(agent_name, Some(team)).await {
                Ok(Some(config_entry)) => agents.push(config_entry),
                Ok(None) => {
                    warn!(
                        "Agent directory '{}' exists but has no valid config.toml",
                        agent_name
                    );
                }
                Err(e) => {
                    warn!("Failed to load config for agent '{}': {}", agent_name, e);
                }
            }
        }

        Ok(agents)
    }

    async fn list_all(&self) -> ConfigResult<Vec<AgentConfigEntry>> {
        let teams_dir = self.path_resolver.teams_dir();
        let mut all_agents = Vec::new();

        if !teams_dir.exists() {
            return Ok(all_agents);
        }

        let mut entries = match tokio::fs::read_dir(&teams_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(all_agents),
            Err(e) => return Err(ConfigError::Io(e)),
        };

        while let Some(entry) = entries.next_entry().await.map_err(ConfigError::Io)? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let team_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            match self.list_in_team(team_name).await {
                Ok(agents) => all_agents.extend(agents),
                Err(e) => {
                    warn!("Failed to list agents in team '{}': {}", team_name, e);
                }
            }
        }

        Ok(all_agents)
    }

    async fn delete(&self, agent_name: &str, team: &str) -> ConfigResult<bool> {
        let config_path = self.config_path(agent_name, Some(team));

        if !config_path.exists() {
            return Ok(false);
        }

        // Remove from cache
        self.cache.remove(team, agent_name).await;

        // Delete file
        self.io
            .delete(&config_path)
            .await
            .map_err(|e| ConfigError::Other(format!("Failed to delete config: {e}")))?;

        info!("Deleted agent '{}' config from team '{}'", agent_name, team);
        Ok(true)
    }

    async fn clear_cache(&self) {
        self.cache.clear().await;
        debug!("Agent configuration cache cleared");
    }

    async fn invalidate_cache(&self, agent_name: &str, team: &str) {
        self.cache.remove(team, agent_name).await;
        debug!(
            "Cache invalidated for agent '{}' in team '{}'",
            agent_name, team
        );
    }

    fn path_resolver(&self) -> &PathResolver {
        &self.path_resolver
    }
}

impl ConfigAuthorityImpl {
    /// Enable a tool in an agent's config whitelist (synchronous)
    pub fn enable_tool_sync(
        &self,
        agent_name: &str,
        team: &str,
        tool_name: &str,
    ) -> anyhow::Result<()> {
        let config_path = self.config_path(agent_name, Some(team));
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found in team '{team}'");
        }

        let content = std::fs::read_to_string(&config_path)?;
        let mut config: crate::types::agent::AgentConfig = toml::from_str(&content)?;

        let tools = config.tools.get_or_insert_with(Default::default);
        if !tools
            .enabled
            .iter()
            .any(|e| e.eq_ignore_ascii_case(tool_name))
        {
            tools.enabled.push(tool_name.to_string());
        }

        let updated = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, updated)?;
        Ok(())
    }

    /// Disable a tool in an agent's config whitelist (synchronous)
    pub fn disable_tool_sync(
        &self,
        agent_name: &str,
        team: &str,
        tool_name: &str,
    ) -> anyhow::Result<()> {
        let config_path = self.config_path(agent_name, Some(team));
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found in team '{team}'");
        }

        let content = std::fs::read_to_string(&config_path)?;
        let mut config: crate::types::agent::AgentConfig = toml::from_str(&content)?;

        let tools = config.tools.get_or_insert_with(Default::default);
        tools.enabled.retain(|e| !e.eq_ignore_ascii_case(tool_name));

        let updated = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, updated)?;
        Ok(())
    }
}

impl Clone for ConfigAuthorityImpl {
    fn clone(&self) -> Self {
        Self {
            path_resolver: self.path_resolver.clone(),
            cache: ConfigCache::new(), // Fresh cache for cloned instance
            io: self.io.clone(),
            api_key_resolver: self.api_key_resolver.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_resolver(temp_dir: &TempDir) -> PathResolver {
        PathResolver::with_dirs(
            temp_dir.path().to_path_buf(),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        )
    }

    #[tokio::test]
    async fn test_save_and_get() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        let config = AgentConfig::default();

        // Save
        let path = authority
            .save("test-agent", "default", &config)
            .await
            .unwrap();
        assert!(path.exists());

        // Retrieve
        let entry = authority.get("test-agent", Some("default")).await.unwrap();
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.name, "test-agent");
        assert_eq!(entry.team, "default");
    }

    #[tokio::test]
    async fn test_exists() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        // Non-existent
        assert!(!authority
            .exists("nonexistent", Some("default"))
            .await
            .unwrap());

        // Create and check
        let config = AgentConfig::default();
        authority
            .save("existing", "default", &config)
            .await
            .unwrap();
        assert!(authority.exists("existing", Some("default")).await.unwrap());
    }

    #[tokio::test]
    async fn test_list_in_team() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        // Initially empty
        let agents = authority.list_in_team("default").await.unwrap();
        assert!(agents.is_empty());

        // Add some agents
        for i in 0..3 {
            let config = AgentConfig::default();
            authority
                .save(&format!("agent-{i}"), "default", &config)
                .await
                .unwrap();
        }

        let agents = authority.list_in_team("default").await.unwrap();
        assert_eq!(agents.len(), 3);
    }

    #[tokio::test]
    async fn test_delete() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        // Create an agent
        let config = AgentConfig::default();
        authority
            .save("to-delete", "default", &config)
            .await
            .unwrap();

        // Verify exists
        assert!(authority
            .exists("to-delete", Some("default"))
            .await
            .unwrap());

        // Delete
        let deleted = authority.delete("to-delete", "default").await.unwrap();
        assert!(deleted);

        // Verify gone
        assert!(!authority
            .exists("to-delete", Some("default"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_cache_isolation_between_clones() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority1 = ConfigAuthorityImpl::new(resolver.clone());
        let authority2 = authority1.clone();

        let config = AgentConfig::default();
        authority1
            .save("shared-agent", "default", &config)
            .await
            .unwrap();

        // Both should be able to read
        let entry1 = authority1.get("shared-agent", Some("default")).await;
        let entry2 = authority2.get("shared-agent", Some("default")).await;

        assert!(entry1.is_ok());
        assert!(entry2.is_ok());
    }
}
