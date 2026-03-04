//! Provider registry and factory
//!
//! This module provides a unified way to create providers based on metadata.
//! Instead of separate implementations for each provider, we have:
//!
//! 1. **Base implementations**: OpenAI and Anthropic providers handle the actual API calls
//! 2. **Metadata registry**: Maps provider names to their API type, base URL, and auth config
//! 3. **Factory function**: Routes provider requests to the appropriate base implementation
//!
//! This approach means:
//! - Adding a new provider = adding a metadata entry (not a new file)
//! - Bug fixes apply to all compatible providers automatically
//! - ~90% of providers are OpenAI-compatible and just need URL + key

use crate::providers::{
    anthropic::AnthropicProvider, openai::OpenAIProvider, AnthropicConfig, OpenAIConfig,
};
use crate::types::provider::{ProviderConfig, ProviderType};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Provider API types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiType {
    /// OpenAI Chat Completions API (most common)
    OpenAICompletions,
    /// Anthropic Messages API
    AnthropicMessages,
}

impl ApiType {
    /// Parse API type from string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "openai-completions" => Some(ApiType::OpenAICompletions),
            "anthropic-messages" => Some(ApiType::AnthropicMessages),
            _ => None,
        }
    }

    /// Convert to string
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiType::OpenAICompletions => "openai-completions",
            ApiType::AnthropicMessages => "anthropic-messages",
        }
    }
}

/// Provider metadata - defines how to connect to a provider
#[derive(Debug, Clone)]
pub struct ProviderMetadata {
    /// Canonical provider ID
    pub id: &'static str,
    /// Display name
    pub display_name: &'static str,
    /// Alternative names/aliases
    pub aliases: &'static [&'static str],
    /// Environment variable names for API keys
    pub api_key_env: &'static [&'static str],
    /// Which API type to use
    pub api_type: ApiType,
    /// Base URL for the API
    pub base_url: &'static str,
    /// Whether to use Authorization header (vs x-api-key, etc.)
    pub use_auth_header: bool,
    /// Default model
    pub default_model: &'static str,
}

/// Provider registry - maps provider names to their metadata
pub struct ProviderRegistry {
    providers: HashMap<String, &'static ProviderMetadata>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    /// Create a new registry with all built-in providers
    pub fn new() -> Self {
        let providers: HashMap<String, &'static ProviderMetadata> = BUILT_IN_PROVIDERS
            .iter()
            .flat_map(|meta| {
                let mut entries = vec![(meta.id.to_string(), meta as &'static ProviderMetadata)];
                for alias in meta.aliases {
                    entries.push((alias.to_string(), meta as &'static ProviderMetadata));
                }
                entries
            })
            .collect();

        Self { providers }
    }

    /// Look up provider metadata by name
    pub fn get(&self, name: &str) -> Option<&ProviderMetadata> {
        self.providers.get(name).copied()
    }

    /// Check if a provider is supported
    pub fn has(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    /// Get API key from environment variables
    pub fn get_api_key(&self, metadata: &ProviderMetadata) -> Option<String> {
        for env_var in metadata.api_key_env {
            if let Ok(key) = std::env::var(env_var) {
                if !key.trim().is_empty() {
                    return Some(key);
                }
            }
        }
        None
    }
}

/// Built-in provider metadata
///
/// Most providers are OpenAI-compatible and just need different base URLs.
/// Only truly unique APIs get their own implementation.
const BUILT_IN_PROVIDERS: &[ProviderMetadata] = &[
    // ═════════════════════════════════════════════════════════════════
    // OpenAI (native)
    // ═════════════════════════════════════════════════════════════════
    ProviderMetadata {
        id: "openai",
        display_name: "OpenAI",
        aliases: &[],
        api_key_env: &["OPENAI_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.openai.com/v1",
        use_auth_header: true,
        default_model: "gpt-4o-mini",
    },
    // ═════════════════════════════════════════════════════════════════
    // Anthropic (native)
    // ═════════════════════════════════════════════════════════════════
    ProviderMetadata {
        id: "anthropic",
        display_name: "Anthropic",
        aliases: &["claude"],
        api_key_env: &["ANTHROPIC_API_KEY"],
        api_type: ApiType::AnthropicMessages,
        base_url: "https://api.anthropic.com",
        use_auth_header: true,
        default_model: "claude-3-5-sonnet-latest",
    },
    // ═════════════════════════════════════════════════════════════════
    // OpenAI-compatible providers (alphabetical)
    // ═════════════════════════════════════════════════════════════════
    ProviderMetadata {
        id: "azure-openai",
        display_name: "Azure OpenAI",
        aliases: &["azure"],
        api_key_env: &["AZURE_OPENAI_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "", // Must be provided per-deployment
        use_auth_header: true,
        default_model: "gpt-4",
    },
    ProviderMetadata {
        id: "cohere",
        display_name: "Cohere",
        aliases: &[],
        api_key_env: &["COHERE_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.cohere.com/v2",
        use_auth_header: true,
        default_model: "command-r-plus",
    },
    ProviderMetadata {
        id: "deepseek",
        display_name: "DeepSeek",
        aliases: &["deep-seek"],
        api_key_env: &["DEEPSEEK_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.deepseek.com/v1",
        use_auth_header: true,
        default_model: "deepseek-chat",
    },
    ProviderMetadata {
        id: "fireworks",
        display_name: "Fireworks AI",
        aliases: &["fireworks-ai"],
        api_key_env: &["FIREWORKS_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.fireworks.ai/inference/v1",
        use_auth_header: true,
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
    },
    ProviderMetadata {
        id: "groq",
        display_name: "Groq",
        aliases: &[],
        api_key_env: &["GROQ_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.groq.com/openai/v1",
        use_auth_header: true,
        default_model: "llama-3.1-70b-versatile",
    },
    ProviderMetadata {
        id: "kimi",
        display_name: "Kimi (Moonshot)",
        aliases: &["moonshot", "moonshotai", "kimi_code", "kimi-code"],
        api_key_env: &["KIMI_API_KEY", "MOONSHOT_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.moonshot.cn/v1",
        use_auth_header: true,
        default_model: "kimi-k2.5",
    },
    ProviderMetadata {
        id: "ollama",
        display_name: "Ollama",
        aliases: &[],
        api_key_env: &[], // Local, no API key needed
        api_type: ApiType::OpenAICompletions,
        base_url: "http://localhost:11434/v1",
        use_auth_header: false,
        default_model: "llama3.1",
    },
    ProviderMetadata {
        id: "openrouter",
        display_name: "OpenRouter",
        aliases: &[],
        api_key_env: &["OPENROUTER_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://openrouter.ai/api/v1",
        use_auth_header: true,
        default_model: "openai/gpt-4o-mini",
    },
    ProviderMetadata {
        id: "perplexity",
        display_name: "Perplexity",
        aliases: &["pplx"],
        api_key_env: &["PERPLEXITY_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.perplexity.ai",
        use_auth_header: true,
        default_model: "llama-3.1-sonar-large-128k-online",
    },
    ProviderMetadata {
        id: "together",
        display_name: "Together AI",
        aliases: &["together-ai"],
        api_key_env: &["TOGETHER_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.together.xyz/v1",
        use_auth_header: true,
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
    },
    ProviderMetadata {
        id: "xai",
        display_name: "xAI (Grok)",
        aliases: &["grok"],
        api_key_env: &["XAI_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.x.ai/v1",
        use_auth_header: true,
        default_model: "grok-beta",
    },
];

/// Create a provider from configuration
///
/// This is the main factory function. It looks up the provider metadata
/// and creates the appropriate provider implementation.
pub fn create_provider(
    provider_type: ProviderType,
    config: &ProviderConfig,
) -> Result<Arc<dyn crate::providers::Provider>> {
    let registry = ProviderRegistry::new();

    // Get provider name from type
    let provider_name = provider_type.to_string();

    // Look up metadata
    let metadata = registry
        .get(&provider_name)
        .with_context(|| format!("Unknown provider: {}", provider_name))?;

    // Get API key
    let api_key = if metadata.api_key_env.is_empty() {
        // No API key required (e.g., local Ollama)
        String::new()
    } else {
        registry
            .get_api_key(metadata)
            .or_else(|| config.api_key.clone())
            .with_context(|| {
                format!(
                    "No API key found for {}. Set one of: {}",
                    metadata.display_name,
                    metadata.api_key_env.join(", ")
                )
            })?
    };

    // Get base URL (config overrides default)
    let base_url = if config.base_url.is_some() && !config.base_url.as_ref().unwrap().is_empty() {
        config.base_url.clone().unwrap()
    } else {
        metadata.base_url.to_string()
    };

    // Get model
    let model = config
        .default_model_config()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| metadata.default_model.to_string());

    // Create provider based on API type
    match metadata.api_type {
        ApiType::OpenAICompletions => {
            let openai_config = OpenAIConfig {
                api_key,
                base_url,
                model,
                max_tokens: config
                    .default_model_config()
                    .map(|m| m.max_tokens)
                    .unwrap_or(4096),
                temperature: config
                    .default_model_config()
                    .map(|m| m.temperature)
                    .unwrap_or(0.7),
                timeout_seconds: config.timeout_seconds,
            };

            Ok(Arc::new(OpenAIProvider::new(openai_config)?))
        }
        ApiType::AnthropicMessages => {
            let anthropic_config = AnthropicConfig {
                api_key,
                base_url,
                model,
                max_tokens: config
                    .default_model_config()
                    .map(|m| m.max_tokens)
                    .unwrap_or(4096),
                temperature: config
                    .default_model_config()
                    .map(|m| m.temperature)
                    .unwrap_or(0.7),
                timeout_seconds: config.timeout_seconds,
            };

            Ok(Arc::new(AnthropicProvider::new(anthropic_config)?))
        }
    }
}

/// Get provider metadata by name
pub fn get_provider_metadata(name: &str) -> Option<&'static ProviderMetadata> {
    // Direct lookup in BUILT_IN_PROVIDERS to avoid lifetime issues
    let name_lower = name.to_lowercase();
    
    // First try canonical IDs
    for meta in BUILT_IN_PROVIDERS {
        if meta.id == name_lower {
            return Some(meta);
        }
    }
    
    // Then try aliases
    for meta in BUILT_IN_PROVIDERS {
        for alias in meta.aliases {
            if *alias == name_lower {
                return Some(meta);
            }
        }
    }
    
    None
}

/// List all available providers
pub fn list_providers() -> Vec<&'static ProviderMetadata> {
    BUILT_IN_PROVIDERS.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_registry() {
        let registry = ProviderRegistry::new();

        // Test canonical lookup
        assert!(registry.has("openai"));
        assert!(registry.has("kimi"));

        // Test alias lookup
        assert!(registry.has("moonshot")); // alias for kimi
        assert!(registry.has("claude"));   // alias for anthropic
    }

    #[test]
    fn test_provider_metadata() {
        let registry = ProviderRegistry::new();

        let kimi = registry.get("kimi").unwrap();
        assert_eq!(kimi.id, "kimi");
        assert_eq!(kimi.api_type, ApiType::OpenAICompletions);
        assert!(kimi.base_url.contains("moonshot"));

        let moonshot = registry.get("moonshot").unwrap();
        assert_eq!(moonshot.id, "kimi"); // resolves to canonical
    }

    #[test]
    fn test_api_type_roundtrip() {
        assert_eq!(
            ApiType::from_str("openai-completions"),
            Some(ApiType::OpenAICompletions)
        );
        assert_eq!(
            ApiType::from_str("anthropic-messages"),
            Some(ApiType::AnthropicMessages)
        );
        assert_eq!(ApiType::OpenAICompletions.as_str(), "openai-completions");
    }
}
