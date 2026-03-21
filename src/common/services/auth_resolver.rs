//! Authentication Resolver
//!
//! Provides an abstraction for resolving API keys from different sources.
//! Used by both CLI (filesystem-based) and API (request/env-based).

use crate::types::provider::ProviderType;
use async_trait::async_trait;
use std::collections::HashMap;

/// Trait for resolving authentication credentials
#[async_trait]
pub trait AuthResolver: Send + Sync {
    /// Resolve API key for a given provider
    async fn resolve_api_key(&self, provider: ProviderType) -> Option<String>;

    /// Get the source name for debugging/logging
    fn source_name(&self) -> &'static str;
}

/// Filesystem-based auth resolver (for CLI)
///
/// Reads credentials from the filesystem storage (e.g., ~/.pekobot/credentials)
pub struct FilesystemAuthResolver {
    config_dir: std::path::PathBuf,
}

impl FilesystemAuthResolver {
    /// Create a new filesystem auth resolver
    pub fn new(config_dir: std::path::PathBuf) -> Self {
        Self { config_dir }
    }

    /// Get credential file path for a provider
    fn credential_path(&self, provider: &str) -> std::path::PathBuf {
        self.config_dir
            .join("credentials")
            .join(format!("{}.txt", provider))
    }
}

#[async_trait]
impl AuthResolver for FilesystemAuthResolver {
    async fn resolve_api_key(&self, provider: ProviderType) -> Option<String> {
        let provider_name = match provider {
            ProviderType::OpenAI => "openai",
            ProviderType::Anthropic => "anthropic",
            ProviderType::Moonshot => "moonshot",
            ProviderType::Kimi => "kimi",
            ProviderType::Ollama => return None, // Ollama doesn't need API key
            ProviderType::OpenAICompatible => "openai-compatible",
        };

        let path = self.credential_path(provider_name);

        // Try to read from file
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                let key = content.trim();
                if key.is_empty() {
                    None
                } else {
                    Some(key.to_string())
                }
            }
            Err(_) => {
                // Fallback to environment variable
                let env_var = api_key_env_var(provider)?;
                std::env::var(env_var).ok()
            }
        }
    }

    fn source_name(&self) -> &'static str {
        "filesystem"
    }
}

/// Direct auth resolver (for API)
///
/// Uses provided environment variables directly
pub struct DirectAuthResolver {
    env: HashMap<String, String>,
}

impl DirectAuthResolver {
    /// Create a new direct auth resolver with environment variables
    pub fn new(env: HashMap<String, String>) -> Self {
        Self { env }
    }

    /// Create an empty resolver
    pub fn empty() -> Self {
        Self {
            env: HashMap::new(),
        }
    }
}

#[async_trait]
impl AuthResolver for DirectAuthResolver {
    async fn resolve_api_key(&self, provider: ProviderType) -> Option<String> {
        let env_var = api_key_env_var(provider)?;

        // First check provided env
        if let Some(key) = self.env.get(&env_var) {
            return Some(key.clone());
        }

        // Fallback to system env
        std::env::var(env_var).ok()
    }

    fn source_name(&self) -> &'static str {
        "direct"
    }
}

/// Get API key environment variable name for a provider
fn api_key_env_var(provider: ProviderType) -> Option<String> {
    match provider {
        ProviderType::OpenAI => Some("OPENAI_API_KEY".to_string()),
        ProviderType::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
        ProviderType::Moonshot => Some("MOONSHOT_API_KEY".to_string()),
        ProviderType::Kimi => Some("KIMI_API_KEY".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_direct_auth_resolver() {
        let mut env = HashMap::new();
        env.insert("KIMI_API_KEY".to_string(), "test-key-123".to_string());

        let resolver = DirectAuthResolver::new(env);
        let key = resolver.resolve_api_key(ProviderType::Kimi).await;

        assert_eq!(key, Some("test-key-123".to_string()));
    }

    #[tokio::test]
    async fn test_direct_auth_resolver_missing() {
        let resolver = DirectAuthResolver::empty();
        let key = resolver.resolve_api_key(ProviderType::OpenAI).await;

        // Should return None if env var not set
        assert!(key.is_none() || std::env::var("OPENAI_API_KEY").is_ok());
    }

    #[test]
    fn test_source_names() {
        let fs_resolver = FilesystemAuthResolver::new(std::path::PathBuf::from("/tmp"));
        let direct_resolver = DirectAuthResolver::empty();

        assert_eq!(fs_resolver.source_name(), "filesystem");
        assert_eq!(direct_resolver.source_name(), "direct");
    }
}
