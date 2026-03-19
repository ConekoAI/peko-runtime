//! Agent configuration builder
//!
//! Provides functions for building agent configurations with
//! authentication and provider detection.

use crate::common::paths::PathResolver;
use crate::types::agent::AgentConfig;
use crate::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use std::collections::HashMap;

/// Build default agent config
pub fn build_default_config(
    name: &str,
    provider: &str,
    model: Option<String>,
    _db: Option<String>,
) -> AgentConfig {
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

    AgentConfig {
        version: "1.0".to_string(),
        name: name.to_string(),
        description: Some(format!("Pekobot agent: {name}")),
        team: None,
        tenant: None,
        capabilities: vec![],
        provider: ProviderConfig {
            provider_type,
            api_key: None,
            api_key_env: api_key_env_var(provider_type),
            base_url: base_url(provider_type),
            default_model,
            models,
            timeout_seconds: 60,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        // Use defaults for the rest
        ..Default::default()
    }
}

/// Build config with authentication detection
pub async fn build_config_with_auth(
    _paths: &PathResolver,
    name: &str,
    provider: &str,
    model: Option<String>,
    _db: Option<String>,
) -> anyhow::Result<AgentConfig> {
    let config = build_default_config(name, provider, model, _db);
    // TODO: Detect available providers and configure auth if available
    Ok(config)
}

/// Parse provider string to ProviderType
fn parse_provider_type(provider: &str) -> ProviderType {
    match provider.to_lowercase().as_str() {
        "openai" => ProviderType::OpenAI,
        "anthropic" => ProviderType::Anthropic,
        "ollama" => ProviderType::Ollama,
        "moonshot" => ProviderType::Moonshot,
        "kimi" => ProviderType::Kimi,
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
