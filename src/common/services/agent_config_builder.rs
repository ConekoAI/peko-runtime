//! Agent configuration builder (DEPRECATED)
//!
//! DEPRECATED: This module is being merged into AgentService.
//! Use `AgentService::create_agent()` which handles config building internally.
//!
//! Provides functions for building agent configurations with
//! authentication and provider detection.
#![deprecated(
    since = "0.9.0",
    note = "AgentConfigBuilder is deprecated. Use AgentService::create_agent() instead."
)]

use crate::common::paths::PathResolver;
use crate::common::services::auth_resolver::AuthResolver;
use crate::types::agent::{AgentConfig, BootstrapFileConfig, PromptConfig, PromptMode};
use crate::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use std::collections::HashMap;

/// Builder for creating AgentConfig with fluent API (DEPRECATED)
///
/// DEPRECATED: Use `AgentService::create_agent()` instead.
///
/// # Example
/// ```rust,ignore
/// use pekobot::common::services::{AgentConfigBuilder, DirectAuthResolver};
///
/// let auth_resolver = DirectAuthResolver::new(std::collections::HashMap::new());
/// let config = AgentConfigBuilder::new("my-agent")
///     .with_provider("kimi")
///     .with_team("default")
///     .build(&auth_resolver)
///     .await?;
/// ```
#[deprecated(since = "0.9.0", note = "Use AgentService::create_agent() instead")]
pub struct AgentConfigBuilder {
    name: String,
    provider: String,
    model: Option<String>,
    team: Option<String>,
    description: Option<String>,
    env_overrides: HashMap<String, String>,
}

impl AgentConfigBuilder {
    /// Create a new config builder with the given agent name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            provider: "openai".to_string(), // Default provider
            model: None,
            team: None,
            description: None,
            env_overrides: HashMap::new(),
        }
    }

    /// Set the provider (e.g., "kimi", "openai", "anthropic")
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Set the model name
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the team
    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
        self
    }

    /// Set the description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add an environment variable override
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_overrides.insert(key.into(), value.into());
        self
    }

    /// Add multiple environment variable overrides
    pub fn with_envs(mut self, envs: HashMap<String, String>) -> Self {
        self.env_overrides.extend(envs);
        self
    }

    /// Build the AgentConfig
    ///
    /// This will:
    /// 1. Parse the provider type
    /// 2. Build model configuration
    /// 3. Resolve API key via AuthResolver
    /// 4. Apply any environment overrides
    pub async fn build(self, auth_resolver: &dyn AuthResolver) -> anyhow::Result<AgentConfig> {
        let provider_type = parse_provider_type(&self.provider);
        let default_model = self.model.unwrap_or_else(|| "default".to_string());

        // Build models map
        let mut models = HashMap::new();
        models.insert(
            "default".to_string(),
            ModelConfig {
                name: default_model_name(provider_type),
                max_tokens: 4096,
                temperature: 0.7,
                top_p: 1.0,
                presence_penalty: 0.0,
                frequency_penalty: 0.0,
            },
        );

        // Resolve API key
        let api_key = auth_resolver.resolve_api_key(provider_type).await;

        // If API key was resolved, don't use env var fallback
        let api_key_env = if api_key.is_some() {
            None
        } else {
            api_key_env_var(provider_type)
        };

        // Check for API key in env overrides
        let api_key = if let Some(env_key) = api_key_env_var(provider_type) {
            self.env_overrides.get(&env_key).cloned().or(api_key)
        } else {
            api_key
        };

        let description = self
            .description
            .unwrap_or_else(|| format!("Pekobot agent: {}", self.name));

        Ok(AgentConfig {
            version: "1.0".to_string(),
            name: self.name,
            description: Some(description),
            team: self.team,
            tenant: None,
            capabilities: vec![],
            provider: ProviderConfig {
                provider_type,
                api_key,
                api_key_env,
                base_url: base_url(provider_type),
                default_model,
                models,
                timeout_seconds: 60,
                max_retries: 3,
                retry_delay_ms: 1000,
            },
            // Use defaults for the rest
            ..Default::default()
        })
    }
}

/// Build default agent config (legacy function, kept for compatibility)
pub fn build_default_config(
    name: &str,
    provider: &str,
    model: Option<String>,
    _db: Option<String>,
) -> AgentConfig {
    // For synchronous contexts, we can't use async auth resolution
    // Use environment variables directly
    let provider_type = parse_provider_type(provider);
    let default_model = model.unwrap_or_else(|| "default".to_string());

    let mut models = HashMap::new();
    models.insert(
        "default".to_string(),
        ModelConfig {
            name: default_model_name(provider_type),
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        },
    );

    // Try to get API key from environment
    let api_key_env = api_key_env_var(provider_type);
    let api_key = api_key_env.as_ref().and_then(|env| std::env::var(env).ok());

    AgentConfig {
        version: "1.0".to_string(),
        name: name.to_string(),
        description: Some(format!("Pekobot agent: {name}")),
        team: None,
        tenant: None,
        capabilities: vec![],
        provider: ProviderConfig {
            provider_type,
            api_key,
            api_key_env,
            base_url: base_url(provider_type),
            default_model,
            models,
            timeout_seconds: 60,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        // Include default prompt configuration with bootstrap files
        prompt: Some(PromptConfig {
            mode: PromptMode::Full,
            custom_prompt: None,
            extra_sections: vec![],
            bootstrap: Some(BootstrapFileConfig {
                max_chars_per_file: 20_000,
                files: Some(vec![
                    "AGENTS.md".to_string(),
                    "SOUL.md".to_string(),
                    "TOOLS.md".to_string(),
                    "IDENTITY.md".to_string(),
                    "USER.md".to_string(),
                    "MEMORY.md".to_string(),
                ]),
            }),
        }),
        // Use defaults for the rest
        ..Default::default()
    }
}

/// Build config with authentication detection (legacy function)
pub async fn build_config_with_auth(
    _paths: &PathResolver,
    name: &str,
    provider: &str,
    model: Option<String>,
    _db: Option<String>,
) -> anyhow::Result<AgentConfig> {
    // Create a direct resolver that uses env vars
    let resolver = crate::common::services::auth_resolver::DirectAuthResolver::empty();
    AgentConfigBuilder::new(name)
        .with_provider(provider)
        .with_model(model.unwrap_or_default())
        .build(&resolver)
        .await
}

/// Parse provider string to ProviderType
fn parse_provider_type(provider: &str) -> ProviderType {
    match provider.to_lowercase().as_str() {
        "openai" => ProviderType::OpenAI,
        "anthropic" => ProviderType::Anthropic,
        "ollama" => ProviderType::Ollama,
        "moonshot" => ProviderType::Moonshot,
        "kimi" => ProviderType::Kimi,
        "kimi_code" | "kimi-code" => ProviderType::Kimi,
        _ => ProviderType::OpenAI,
    }
}

/// Get default model name for provider
fn default_model_name(provider_type: ProviderType) -> String {
    match provider_type {
        ProviderType::OpenAI => "gpt-4o-mini".to_string(),
        ProviderType::Anthropic => "claude-3-sonnet".to_string(),
        ProviderType::Ollama => "llama3.2".to_string(),
        ProviderType::OpenAICompatible => "default".to_string(),
        ProviderType::Moonshot => "kimi-k2.5".to_string(),
        ProviderType::Kimi => "k2p5".to_string(),
    }
}

/// Get API key environment variable for provider
fn api_key_env_var(provider_type: ProviderType) -> Option<String> {
    match provider_type {
        ProviderType::OpenAI => Some("OPENAI_API_KEY".to_string()),
        ProviderType::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
        ProviderType::Moonshot => Some("MOONSHOT_API_KEY".to_string()),
        ProviderType::Kimi => Some("KIMI_API_KEY".to_string()),
        _ => None,
    }
}

/// Get base URL for provider
fn base_url(provider_type: ProviderType) -> Option<String> {
    match provider_type {
        ProviderType::Ollama => Some("http://localhost:11434".to_string()),
        ProviderType::Moonshot => Some("https://api.moonshot.cn/v1".to_string()),
        ProviderType::Kimi => Some("https://api.kimi.com/coding".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::services::auth_resolver::DirectAuthResolver;

    #[tokio::test]
    async fn test_config_builder_basic() {
        let resolver = DirectAuthResolver::empty();
        let config = AgentConfigBuilder::new("test-agent")
            .with_provider("kimi")
            .build(&resolver)
            .await
            .unwrap();

        assert_eq!(config.name, "test-agent");
        assert_eq!(config.provider.provider_type, ProviderType::Kimi);
    }

    #[tokio::test]
    async fn test_config_builder_with_team() {
        let resolver = DirectAuthResolver::empty();
        let config = AgentConfigBuilder::new("test-agent")
            .with_provider("openai")
            .with_team("my-team")
            .build(&resolver)
            .await
            .unwrap();

        assert_eq!(config.team, Some("my-team".to_string()));
    }

    #[tokio::test]
    async fn test_config_builder_with_env_override() {
        let resolver = DirectAuthResolver::empty();
        let config = AgentConfigBuilder::new("test-agent")
            .with_provider("kimi")
            .with_env("KIMI_API_KEY", "test-key-123")
            .build(&resolver)
            .await
            .unwrap();

        assert_eq!(config.provider.api_key, Some("test-key-123".to_string()));
    }

    #[test]
    fn test_parse_provider_type() {
        assert_eq!(parse_provider_type("openai"), ProviderType::OpenAI);
        assert_eq!(parse_provider_type("kimi"), ProviderType::Kimi);
        assert_eq!(parse_provider_type("KIMI"), ProviderType::Kimi); // Case insensitive
        assert_eq!(parse_provider_type("unknown"), ProviderType::OpenAI); // Default
    }
}
