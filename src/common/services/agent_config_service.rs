//! Agent Configuration Service
//!
//! Provides unified agent configuration management for both CLI and HTTP API.
//! This service is the single source of truth for agent configurations,
//! reading from and writing to the canonical TOML location:
//! `~/.pekobot/teams/{team}/agents/{agent}/config.toml`
//!
//! ## Architecture
//!
//! - Uses `PathResolver` for consistent path resolution
//! - In-memory LRU cache for frequently accessed configs
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
use crate::types::agent::AgentConfig;
use crate::types::provider::ProviderType;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Credentials store structure (mirrors src/commands/auth.rs)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Credential {
    provider: String,
    profile: String,
    api_key: String,
    #[allow(dead_code)]
    created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CredentialsStore {
    #[allow(dead_code)]
    version: u32,
    credentials: HashMap<String, Credential>, // key: "provider:profile"
}

/// Agent configuration entry with metadata
#[derive(Debug, Clone)]
pub struct AgentConfigEntry {
    /// Agent name
    pub name: String,
    /// Team name
    pub team: String,
    /// Agent configuration
    pub config: AgentConfig,
    /// Config file path
    pub config_path: PathBuf,
}

/// Unified agent configuration service
///
/// This service provides a single source of truth for agent configurations,
/// replacing the dual system of ConfigRegistry (JSON) and direct TOML file I/O.
pub struct AgentConfigService {
    path_resolver: PathResolver,
    /// In-memory cache of loaded configurations
    cache: RwLock<HashMap<String, AgentConfigEntry>>,
}

impl std::fmt::Debug for AgentConfigService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfigService")
            .field("path_resolver", &self.path_resolver)
            .finish_non_exhaustive()
    }
}

impl Clone for AgentConfigService {
    /// Creates a new instance with an empty cache.
    ///
    /// Note: Unlike typical `Clone` semantics, this does NOT copy the in-memory cache.
    /// Each clone maintains its own independent cache. This is intentional since each
    /// clone is typically used in a different context (CLI vs API).
    fn clone(&self) -> Self {
        Self {
            path_resolver: self.path_resolver.clone(),
            cache: RwLock::new(HashMap::new()),
        }
    }
}

impl AgentConfigService {
    /// Create a new agent configuration service
    pub fn new(path_resolver: PathResolver) -> Self {
        Self {
            path_resolver,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get the canonical config path for an agent
    fn config_path(&self, agent_name: &str, team: Option<&str>) -> PathBuf {
        self.path_resolver.agent_config(agent_name, team)
    }

    /// Get the credentials file path
    fn credentials_path(&self) -> PathBuf {
        self.path_resolver.config_dir().join("credentials.json")
    }

    /// Load credentials from the credentials.json file
    fn load_credentials(&self) -> Result<CredentialsStore> {
        let path = self.credentials_path();

        if !path.exists() {
            return Ok(CredentialsStore {
                version: 1,
                credentials: HashMap::new(),
            });
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read credentials file: {}", path.display()))?;
        let store: CredentialsStore = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse credentials file: {}", path.display()))?;
        Ok(store)
    }

    /// Resolve API key for a provider using credentials.json first, then env var
    fn resolve_api_key(&self, provider_type: ProviderType) -> Option<String> {
        // Map ProviderType to provider name used in credentials.json
        let provider_name = match provider_type {
            ProviderType::OpenAI => "openai",
            ProviderType::Anthropic => "anthropic",
            ProviderType::Moonshot => "moonshot",
            ProviderType::Kimi => "kimi",
            ProviderType::Ollama => return None, // Ollama doesn't need API key
            ProviderType::OpenAICompatible => {
                // For OpenAI-compatible, we can't resolve from credentials
                // Fall through to env var check
                ""
            }
        };

        // Try credentials.json first
        if !provider_name.is_empty() {
            if let Ok(credentials) = self.load_credentials() {
                let key = format!("{}:default", provider_name);
                if let Some(cred) = credentials.credentials.get(&key) {
                    debug!("Resolved API key for {} from credentials.json", provider_name);
                    return Some(cred.api_key.clone());
                }
            }
        }

        // Fall back to environment variable
        let env_var = match provider_type {
            ProviderType::OpenAI => "OPENAI_API_KEY",
            ProviderType::Anthropic => "ANTHROPIC_API_KEY",
            ProviderType::Moonshot => "MOONSHOT_API_KEY",
            ProviderType::Kimi => "KIMI_API_KEY",
            ProviderType::Ollama => return None,
            ProviderType::OpenAICompatible => "OPENAI_API_KEY",
        };

        if let Ok(key) = std::env::var(env_var) {
            if !key.trim().is_empty() {
                debug!("Resolved API key for {} from env var {}", provider_name, env_var);
                return Some(key);
            }
        }

        warn!("No API key found for provider {:?}", provider_type);
        None
    }

    /// Resolve API key for the agent config if not already set
    fn resolve_config_api_key(&self, config: &mut AgentConfig) {
        // Only resolve if api_key is not already set (allows override in config.toml)
        if config.provider.api_key.is_some() {
            debug!("API key already set in config, skipping resolution");
            return;
        }

        if let Some(api_key) = self.resolve_api_key(config.provider.provider_type) {
            config.provider.api_key = Some(api_key);
        }
    }

    /// Load agent configuration from TOML file
    ///
    /// Checks cache first, then loads from disk if not cached.
    /// Returns None if the agent doesn't exist.
    pub async fn get(
        &self,
        agent_name: &str,
        team: Option<&str>,
    ) -> Result<Option<AgentConfigEntry>> {
        let cache_key = format!("{}/{}", team.unwrap_or("default"), agent_name);

        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(&cache_key) {
                debug!(
                    "Cache hit for agent '{}' in team '{}'",
                    agent_name,
                    team.unwrap_or("default")
                );
                return Ok(Some(entry.clone()));
            }
        }

        // Try to find the agent in the specified team, or search all teams
        let (config_path, team_name) = if let Some(t) = team {
            (self.config_path(agent_name, Some(t)), t.to_string())
        } else {
            // Search all teams for this agent
            match self.find_agent_in_teams(agent_name).await? {
                Some((path, found_team)) => (path, found_team),
                None => return Ok(None),
            }
        };

        if !config_path.exists() {
            return Ok(None);
        }

        // Load from disk
        let mut config = Self::load_config_from_file(&config_path).await?;

        // Resolve API key if not set in config (credentials.json -> env var)
        self.resolve_config_api_key(&mut config);

        let entry = AgentConfigEntry {
            name: agent_name.to_string(),
            team: team_name,
            config,
            config_path,
        };

        // Cache the result
        {
            let mut cache = self.cache.write().await;
            cache.insert(cache_key, entry.clone());
        }

        Ok(Some(entry))
    }

    /// Find an agent by searching all teams
    async fn find_agent_in_teams(&self, agent_name: &str) -> Result<Option<(PathBuf, String)>> {
        let teams_dir = self.path_resolver.teams_dir();

        if !teams_dir.exists() {
            return Ok(None);
        }

        let mut entries = tokio::fs::read_dir(&teams_dir)
            .await
            .with_context(|| format!("Failed to read teams directory: {}", teams_dir.display()))?;

        while let Some(entry) = entries.next_entry().await? {
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

    /// Load config from TOML file
    async fn load_config_from_file(path: &PathBuf) -> Result<AgentConfig> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: AgentConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML config: {}", path.display()))?;

        Ok(config)
    }

    /// Save agent configuration to TOML file
    ///
    /// Creates parent directories if they don't exist.
    /// Updates the cache after saving.
    pub async fn save(
        &self,
        agent_name: &str,
        team: &str,
        config: &AgentConfig,
    ) -> Result<PathBuf> {
        let config_path = self.config_path(agent_name, Some(team));

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create agent directory: {}", parent.display())
            })?;
        }

        // Serialize to TOML
        let toml_content =
            toml::to_string_pretty(config).with_context(|| "Failed to serialize config to TOML")?;

        // Write atomically using temp file
        let temp_path = config_path.with_extension("tmp");
        tokio::fs::write(&temp_path, toml_content)
            .await
            .with_context(|| {
                format!("Failed to write temp config file: {}", temp_path.display())
            })?;

        tokio::fs::rename(&temp_path, &config_path)
            .await
            .with_context(|| {
                format!("Failed to rename config file to: {}", config_path.display())
            })?;

        info!(
            "Saved agent '{}' config to team '{}' at {}",
            agent_name,
            team,
            config_path.display()
        );

        // Update cache
        let cache_key = format!("{}/{}", team, agent_name);
        let entry = AgentConfigEntry {
            name: agent_name.to_string(),
            team: team.to_string(),
            config: config.clone(),
            config_path: config_path.clone(),
        };

        {
            let mut cache = self.cache.write().await;
            cache.insert(cache_key, entry);
        }

        Ok(config_path)
    }

    /// Check if an agent exists
    pub async fn exists(&self, agent_name: &str, team: Option<&str>) -> Result<bool> {
        match self.get(agent_name, team).await? {
            Some(_) => Ok(true),
            None => {
                // Double-check by looking at file system
                let config_path = self.config_path(agent_name, team);
                Ok(config_path.exists())
            }
        }
    }

    /// List all agents in a team
    pub async fn list_in_team(&self, team: &str) -> Result<Vec<AgentConfigEntry>> {
        let agents_dir = self.path_resolver.agents_dir(Some(team));
        let mut agents = Vec::new();

        if !agents_dir.exists() {
            return Ok(agents);
        }

        let mut entries = tokio::fs::read_dir(&agents_dir).await.with_context(|| {
            format!("Failed to read agents directory: {}", agents_dir.display())
        })?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let agent_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            match self.get(agent_name, Some(team)).await? {
                Some(config_entry) => agents.push(config_entry),
                None => {
                    warn!(
                        "Agent directory '{}' exists but has no valid config.toml",
                        agent_name
                    );
                }
            }
        }

        Ok(agents)
    }

    /// List all agents across all teams
    pub async fn list_all(&self) -> Result<Vec<AgentConfigEntry>> {
        let teams_dir = self.path_resolver.teams_dir();
        let mut all_agents = Vec::new();

        if !teams_dir.exists() {
            return Ok(all_agents);
        }

        let mut entries = tokio::fs::read_dir(&teams_dir)
            .await
            .with_context(|| format!("Failed to read teams directory: {}", teams_dir.display()))?;

        while let Some(entry) = entries.next_entry().await? {
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

    /// Delete an agent configuration
    ///
    /// Removes from cache and deletes the config file.
    /// Note: This does NOT delete the agent directory or sessions.
    pub async fn delete(&self, agent_name: &str, team: &str) -> Result<bool> {
        let config_path = self.config_path(agent_name, Some(team));

        if !config_path.exists() {
            return Ok(false);
        }

        // Remove from cache
        let cache_key = format!("{}/{}", team, agent_name);
        {
            let mut cache = self.cache.write().await;
            cache.remove(&cache_key);
        }

        // Delete file
        tokio::fs::remove_file(&config_path)
            .await
            .with_context(|| format!("Failed to delete config file: {}", config_path.display()))?;

        info!("Deleted agent '{}' config from team '{}'", agent_name, team);
        Ok(true)
    }

    /// Clear the configuration cache
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        debug!("Agent configuration cache cleared");
    }

    /// Invalidate a specific entry in the cache
    pub async fn invalidate_cache(&self, agent_name: &str, team: &str) {
        let cache_key = format!("{}/{}", team, agent_name);
        let mut cache = self.cache.write().await;
        cache.remove(&cache_key);
        debug!(
            "Cache invalidated for agent '{}' in team '{}'",
            agent_name, team
        );
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
        let config = AgentConfig::default();

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
        let config = AgentConfig::default();
        service.save("existing", "default", &config).await.unwrap();
        assert!(service.exists("existing", Some("default")).await.unwrap());
    }
}
