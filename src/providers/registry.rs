//! Provider registry and factory
//!
//! This module provides a unified way to create providers based on metadata.
//! Instead of separate implementations for each provider, we have:
//!
//! 1. **Base adapters**: `OpenAiAdapter` and `AnthropicAdapter` handle API format conversion
//! 2. **Metadata registry**: Maps provider names to their API type, base URL, and auth config
//! 3. **Factory function**: Creates the appropriate adapter and wraps it in Provider
//!
//! This approach means:
//! - Adding a new OpenAI-compatible provider = adding a metadata entry
//! - Only truly unique APIs need a new adapter implementation

use crate::common::types::provider::{ProviderConfig, ProviderType};
use crate::providers::{
    adapters::{AnthropicAdapter, AnyAdapter, OpenAiAdapter, OpenAiCompatibleAdapter},
    core::Provider,
    DEFAULT_MAX_OUTPUT_TOKENS,
};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Provider API types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiType {
    /// `OpenAI` Chat Completions API (most common)
    OpenAICompletions,
    /// Anthropic Messages API
    AnthropicMessages,
}

impl ApiType {
    /// Parse API type from string
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "openai-completions" => Some(ApiType::OpenAICompletions),
            "anthropic-messages" => Some(ApiType::AnthropicMessages),
            _ => None,
        }
    }

    /// Convert to string
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ProviderMetadata> {
        self.providers.get(name).copied()
    }

    /// Check if a provider is supported
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    /// Get API key from environment variables
    #[must_use]
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

    /// Iterate over all providers (including aliases)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ProviderMetadata)> + '_ {
        self.providers.iter().map(|(k, v)| (k, *v))
    }
}

/// Built-in provider metadata
///
/// Most providers are OpenAI-compatible and just need different base URLs.
/// Only truly unique APIs get their own adapter.
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
        default_model: "gpt-4",
    },
    ProviderMetadata {
        id: "cohere",
        display_name: "Cohere",
        aliases: &[],
        api_key_env: &["COHERE_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.cohere.com/v2",
        default_model: "command-r-plus",
    },
    ProviderMetadata {
        id: "deepseek",
        display_name: "DeepSeek",
        aliases: &["deep-seek"],
        api_key_env: &["DEEPSEEK_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
    },
    ProviderMetadata {
        id: "fireworks",
        display_name: "Fireworks AI",
        aliases: &["fireworks-ai"],
        api_key_env: &["FIREWORKS_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.fireworks.ai/inference/v1",
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
    },
    ProviderMetadata {
        id: "groq",
        display_name: "Groq",
        aliases: &[],
        api_key_env: &["GROQ_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.1-70b-versatile",
    },
    ProviderMetadata {
        id: "moonshot",
        display_name: "Moonshot AI",
        aliases: &["kimi", "moonshotai"],
        api_key_env: &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.moonshot.cn/v1",
        default_model: "kimi-k2.5",
    },
    ProviderMetadata {
        id: "ollama",
        display_name: "Ollama",
        aliases: &[],
        api_key_env: &[], // Local, no API key needed
        api_type: ApiType::OpenAICompletions,
        base_url: "http://localhost:11434/v1",
        default_model: "llama3.1",
    },
    ProviderMetadata {
        id: "openrouter",
        display_name: "OpenRouter",
        aliases: &[],
        api_key_env: &["OPENROUTER_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-4o-mini",
    },
    ProviderMetadata {
        id: "perplexity",
        display_name: "Perplexity",
        aliases: &["pplx"],
        api_key_env: &["PERPLEXITY_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.perplexity.ai",
        default_model: "llama-3.1-sonar-large-128k-online",
    },
    ProviderMetadata {
        id: "together",
        display_name: "Together AI",
        aliases: &["together-ai"],
        api_key_env: &["TOGETHER_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.together.xyz/v1",
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
    },
    ProviderMetadata {
        id: "xai",
        display_name: "xAI (Grok)",
        aliases: &["grok"],
        api_key_env: &["XAI_API_KEY"],
        api_type: ApiType::OpenAICompletions,
        base_url: "https://api.x.ai/v1",
        default_model: "grok-beta",
    },
    // ═════════════════════════════════════════════════════════════════
    // Anthropic-compatible providers
    // ═════════════════════════════════════════════════════════════════
    ProviderMetadata {
        id: "kimi",
        display_name: "Kimi (Kimi Code API)",
        aliases: &["kimi-code", "kimi-ai"],
        api_key_env: &["KIMI_API_KEY"],
        api_type: ApiType::AnthropicMessages,
        base_url: "https://api.kimi.com/coding",
        default_model: "kimi-for-coding",
    },
    ProviderMetadata {
        id: "minimax",
        display_name: "MiniMax",
        aliases: &["minimax-ai"],
        api_key_env: &["MINIMAX_API_KEY"],
        api_type: ApiType::AnthropicMessages,
        base_url: "https://api.minimaxi.com/anthropic",
        default_model: "MiniMax-M3",
    },
];

/// Create a provider from configuration
///
/// This is the main factory function. It looks up the provider metadata
/// and creates the appropriate provider implementation using the new
/// adapter-based architecture.
pub fn create_provider(config: ProviderConfig) -> Result<Arc<Provider>> {
    let registry = ProviderRegistry::new();

    // Convert ProviderType to string for lookup
    let provider_name = match config.provider_type {
        ProviderType::OpenAI => "openai",
        ProviderType::Anthropic => "anthropic",
        ProviderType::Ollama => "ollama",
        ProviderType::OpenAICompatible => {
            // For OpenAI-compatible, we need to determine the actual provider from base_url
            // or use a generic OpenAI-compatible adapter
            return create_openai_compatible_provider(&config);
        }
        ProviderType::Moonshot => "moonshot",
        ProviderType::Kimi => "kimi",
        ProviderType::Minimax => "minimax",
    };

    // Look up metadata
    let metadata = registry
        .get(provider_name)
        .with_context(|| format!("Unknown provider: {provider_name}"))?;

    // Get API key
    let api_key = config
        .api_key
        .clone()
        .or_else(|| registry.get_api_key(metadata))
        .with_context(|| {
            format!(
                "No API key found for {}. Set one of: {}",
                metadata.display_name,
                metadata.api_key_env.join(", ")
            )
        })?;

    // Get base URL (config overrides default)
    let base_url = config.base_url.clone().unwrap_or_else(|| {
        if metadata.base_url.is_empty() {
            String::new()
        } else {
            metadata.base_url.to_string()
        }
    });

    // Get model from config or use default
    let model = config
        .default_model_config()
        .map_or_else(|| metadata.default_model.to_string(), |m| m.name.clone());

    create_provider_with_adapter(metadata, api_key, base_url, model, config)
}

/// Create an OpenAI-compatible provider
fn create_openai_compatible_provider(config: &ProviderConfig) -> Result<Arc<Provider>> {
    let api_key = config
        .api_key
        .clone()
        .or_else(|| {
            // Try common env vars
            std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
        })
        .context("No API key found for OpenAI-compatible provider")?;

    let base_url = config
        .base_url
        .clone()
        .context("Base URL required for OpenAI-compatible provider")?;

    // Create a generic OpenAI-compatible adapter. The model id is no
    // longer baked into the adapter — it's threaded per request.
    let adapter =
        AnyAdapter::OpenAiCompatible(OpenAiCompatibleAdapter::new("openai-compatible", base_url));
    Ok(Arc::new(Provider::new(adapter, api_key, config.clone())?))
}

/// Create provider with appropriate adapter.
///
/// `model` is preserved only on `ProviderConfig` (used as the default
/// `model_id` when callers don't pass one explicitly). Adapters no
/// longer store it.
fn create_provider_with_adapter(
    metadata: &ProviderMetadata,
    api_key: String,
    base_url: String,
    _model: String,
    config: ProviderConfig,
) -> Result<Arc<Provider>> {
    match metadata.api_type {
        ApiType::OpenAICompletions => {
            let adapter = if base_url.is_empty() || base_url == metadata.base_url {
                AnyAdapter::OpenAi(OpenAiAdapter::new())
            } else {
                AnyAdapter::OpenAi(OpenAiAdapter::new().with_base_url(base_url))
            };
            Ok(Arc::new(Provider::new(adapter, api_key, config)?))
        }
        ApiType::AnthropicMessages => {
            let adapter = if base_url.is_empty() {
                AnyAdapter::Anthropic(AnthropicAdapter::new())
            } else {
                AnyAdapter::Anthropic(AnthropicAdapter::new().with_base_url(base_url))
            };
            Ok(Arc::new(Provider::new(adapter, api_key, config)?))
        }
    }
}

/// Create provider by name with defaults
///
/// Convenience function for creating providers with just a name
pub fn create_provider_by_name(name: &str) -> Result<Arc<Provider>> {
    let registry = ProviderRegistry::new();
    let metadata = registry
        .get(name)
        .with_context(|| format!("Unknown provider: {name}"))?;

    // Determine ProviderType from metadata
    let provider_type = match metadata.api_type {
        ApiType::OpenAICompletions => ProviderType::OpenAI,
        ApiType::AnthropicMessages => ProviderType::Anthropic,
    };

    let mut config = ProviderConfig::default();
    config.provider_type = provider_type;
    config.base_url = if metadata.base_url.is_empty() {
        None
    } else {
        Some(metadata.base_url.to_string())
    };

    // Set default model
    let mut models = std::collections::HashMap::new();
    models.insert(
        "default".to_string(),
        crate::common::types::provider::ModelConfig {
            name: metadata.default_model.to_string(),
            max_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            temperature: 0.7,
            top_p: 1.0,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        },
    );
    config.models = models;
    config.default_model = "default".to_string();

    create_provider(config)
}

/// Get provider metadata by name
#[must_use]
pub fn get_provider_metadata(name: &str) -> Option<&'static ProviderMetadata> {
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
#[must_use]
pub fn list_providers() -> Vec<&'static ProviderMetadata> {
    BUILT_IN_PROVIDERS.iter().collect()
}

/// Build an `Arc<Provider>` from a catalog entry plus an API key and
/// the chosen model.
///
/// This is the factory used by `LlmResolver` and the migration path.
/// The adapter is chosen from `entry.api_format`; the model id is
/// stored on the `ProviderConfig` so `Provider::model_id()` returns
/// it as the default for legacy callers.
pub fn create_provider_for_entry(
    entry: &crate::providers::catalog::ProviderCatalogEntry,
    api_key: &str,
    model: &crate::providers::catalog::ModelInfo,
) -> Result<Arc<Provider>> {
    use crate::providers::adapters::{AnthropicAdapter, AnyAdapter, OpenAiAdapter};
    use crate::providers::catalog::ApiFormat;
    use crate::providers::types::ProviderConfig;

    let adapter = match entry.api_format {
        ApiFormat::OpenaiCompletions => {
            let a = if entry.base_url.is_empty() {
                OpenAiAdapter::new()
            } else {
                OpenAiAdapter::new().with_base_url(&entry.base_url)
            };
            AnyAdapter::OpenAi(a)
        }
        ApiFormat::AnthropicMessages => {
            let a = if entry.base_url.is_empty() {
                AnthropicAdapter::new()
            } else {
                AnthropicAdapter::new().with_base_url(&entry.base_url)
            };
            AnyAdapter::Anthropic(a)
        }
    };

    let mut models = std::collections::HashMap::new();
    models.insert(
        entry.default_model_id.clone(),
        crate::common::types::provider::ModelConfig {
            name: model.id.clone(),
            ..Default::default()
        },
    );
    let mut config = ProviderConfig::default();
    config.default_model = entry.default_model_id.clone();
    config.models = models;

    Provider::new(adapter, api_key.to_string(), config).map(Arc::new)
}

/// Resolve the model name to thread as the default for the legacy
/// `ProviderConfig`-based path. Mirrors `entry.default_model_id`.
pub fn default_model_for_entry(entry: &crate::providers::catalog::ProviderCatalogEntry) -> &str {
    &entry.default_model_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_registry() {
        let registry = ProviderRegistry::new();

        // Test canonical lookup
        assert!(registry.has("openai"));
        assert!(registry.has("anthropic"));

        // Test alias lookup
        assert!(registry.has("moonshot"));
        assert!(registry.has("claude"));
    }

    #[test]
    fn test_provider_metadata_anthropic_groq() {
        let registry = ProviderRegistry::new();

        let anthropic = registry.get("anthropic").unwrap();
        assert_eq!(anthropic.id, "anthropic");
        assert_eq!(anthropic.api_type, ApiType::AnthropicMessages);

        let groq = registry.get("groq").unwrap();
        assert_eq!(groq.id, "groq");
        assert_eq!(groq.api_type, ApiType::OpenAICompletions);
        assert!(groq.base_url.contains("groq"));
    }

    #[test]
    fn test_list_providers_includes_canonical_ids() {
        let providers = list_providers();
        assert!(!providers.is_empty());
        assert!(providers.iter().any(|p| p.id == "openai"));
        assert!(providers.iter().any(|p| p.id == "anthropic"));
    }
}
