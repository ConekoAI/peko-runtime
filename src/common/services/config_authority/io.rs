//! Configuration I/O and API Key Resolution
//!
//! This module provides:
//! - `ConfigIo`: TOML file read/write operations with atomic writes
//! - `ApiKeyResolver`: Single implementation for API key resolution

use crate::types::agent::AgentConfig;
use crate::types::provider::ProviderType;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, warn};

/// Credentials store structure (mirrors src/commands/auth.rs)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Credential {
    provider: String,
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

/// Configuration I/O operations for TOML files
///
/// Handles reading and writing agent configurations to TOML files
/// with atomic write operations.
#[derive(Debug, Clone)]
pub struct ConfigIo;

impl ConfigIo {
    /// Create a new ConfigIo instance
    pub fn new() -> Self {
        Self
    }

    /// Load agent configuration from a TOML file
    pub async fn load_toml(&self, path: &PathBuf) -> Result<AgentConfig> {
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: AgentConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML config: {}", path.display()))?;

        Ok(config)
    }

    /// Save agent configuration to a TOML file atomically
    ///
    /// Writes to a temp file first, then renames for atomicity.
    pub async fn save_toml(&self, path: &PathBuf, config: &AgentConfig) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create agent directory: {}", parent.display())
            })?;
        }

        // Serialize to TOML
        let toml_content =
            toml::to_string_pretty(config).with_context(|| "Failed to serialize config to TOML")?;

        // Write atomically using temp file
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &toml_content)
            .await
            .with_context(|| {
                format!("Failed to write temp config file: {}", temp_path.display())
            })?;

        fs::rename(&temp_path, path)
            .await
            .with_context(|| format!("Failed to rename config file to: {}", path.display()))?;

        Ok(())
    }

    /// Check if a config file exists
    pub async fn exists(&self, path: &PathBuf) -> bool {
        path.exists()
    }

    /// Delete a config file
    pub async fn delete(&self, path: &PathBuf) -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }

        fs::remove_file(path)
            .await
            .with_context(|| format!("Failed to delete config file: {}", path.display()))?;

        Ok(true)
    }
}

impl Default for ConfigIo {
    fn default() -> Self {
        Self::new()
    }
}

/// API Key Resolver - Single implementation for resolving API keys
///
/// Resolves API keys using this priority:
/// 1. credentials.json (set via `pekobot auth set <provider>`)
/// 2. Environment variable
///
/// This is the single canonical implementation, replacing duplicate
/// versions previously found in AgentConfigService and AuthResolver.
#[derive(Debug, Clone)]
pub struct ApiKeyResolver {
    config_dir: PathBuf,
}

impl ApiKeyResolver {
    /// Create a new ApiKeyResolver
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    /// Get the credentials file path
    fn credentials_path(&self) -> PathBuf {
        self.config_dir.join("credentials.json")
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

    /// Resolve API key for a provider type
    ///
    /// Priority: credentials.json first, then environment variable
    pub fn resolve(&self, provider_type: ProviderType) -> Option<String> {
        // Map ProviderType to provider name used in credentials.json
        let provider_name = match provider_type {
            ProviderType::OpenAI => "openai",
            ProviderType::Anthropic => "anthropic",
            ProviderType::Moonshot => "moonshot",
            ProviderType::Kimi => "kimi",
            ProviderType::Minimax => "minimax",
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
                if let Some(cred) = credentials.credentials.get(provider_name) {
                    debug!(
                        "Resolved API key for {} from credentials.json",
                        provider_name
                    );
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
            ProviderType::Minimax => "MINIMAX_API_KEY",
            ProviderType::Ollama => return None,
            ProviderType::OpenAICompatible => "OPENAI_API_KEY",
        };

        if let Ok(key) = std::env::var(env_var) {
            if !key.trim().is_empty() {
                debug!(
                    "Resolved API key for {} from env var {}",
                    provider_name, env_var
                );
                return Some(key);
            }
        }

        warn!("No API key found for provider {:?}", provider_type);
        None
    }

    /// Resolve API key for agent config if not already set
    ///
    /// Only sets the API key if it's not already configured in the config.
    pub fn resolve_config_api_key(&self, config: &mut AgentConfig) {
        // Only resolve if api_key is not already set (allows override in config.toml)
        if config.provider.api_key.is_some() {
            debug!("API key already set in config, skipping resolution");
            return;
        }

        if let Some(api_key) = self.resolve(config.provider.provider_type) {
            config.provider.api_key = Some(api_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_path() {
        let resolver = ApiKeyResolver::new(PathBuf::from("/config"));
        assert_eq!(
            resolver.credentials_path(),
            PathBuf::from("/config/credentials.json")
        );
    }

    #[test]
    fn test_resolve_ollama_returns_none() {
        let resolver = ApiKeyResolver::new(PathBuf::from("/config"));
        assert!(resolver.resolve(ProviderType::Ollama).is_none());
    }
}
