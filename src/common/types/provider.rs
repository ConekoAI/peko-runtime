//! LLM Provider configuration types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider type
    pub provider_type: ProviderType,
    /// API key (optional - can use env var)
    #[serde(default)]
    pub api_key: Option<String>,
    /// API key environment variable name
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Base URL (for custom/OpenAI-compatible endpoints)
    #[serde(default)]
    pub base_url: Option<String>,
    /// Default model
    #[serde(default)]
    pub default_model: String,
    /// Model configurations
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
    /// Request timeout (seconds)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    /// Maximum retries
    #[serde(default = "default_retries")]
    pub max_retries: u32,
    /// Retry delay (milliseconds)
    #[serde(default = "default_retry_delay")]
    pub retry_delay_ms: u64,
}

fn default_timeout() -> u64 {
    300
}

fn default_retries() -> u32 {
    3
}

fn default_retry_delay() -> u64 {
    1000
}

impl Default for ProviderConfig {
    fn default() -> Self {
        let mut models = HashMap::new();
        models.insert(
            "default".to_string(),
            ModelConfig {
                name: "gpt-4o-mini".to_string(),
                max_tokens: 4096,
                temperature: 0.7,
                top_p: 1.0,
                presence_penalty: 0.0,
                frequency_penalty: 0.0,
            },
        );

        Self {
            provider_type: ProviderType::OpenAI,
            api_key: None,
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            base_url: None,
            default_model: "default".to_string(),
            models,
            timeout_seconds: default_timeout(),
            max_retries: 3,
            retry_delay_ms: 1000,
        }
    }
}

/// LLM Provider type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// `OpenAI` (GPT-4, GPT-3.5)
    ///
    /// Explicit `#[serde(rename)]` is needed because heck's snake_case
    /// splitter produces `open_a_i` for the adjacent-capitals `OpenAI`,
    /// but the Display impl (and what users see in `peko principal create`,
    /// logs, and config files) writes `openai`. Same situation for
    /// `OpenAICompatible` below.
    #[serde(rename = "openai")]
    OpenAI,
    /// Anthropic (Claude)
    Anthropic,
    /// Ollama (local models)
    Ollama,
    /// OpenAI-compatible API (custom endpoint)
    #[serde(rename = "openai_compatible")]
    OpenAICompatible,
    /// Moonshot AI (Kimi models via Moonshot API)
    Moonshot,
    /// Kimi (Kimi Code API)
    Kimi,
    /// `MiniMax` (Anthropic-compatible API)
    Minimax,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderType::OpenAI => write!(f, "openai"),
            ProviderType::Anthropic => write!(f, "anthropic"),
            ProviderType::Ollama => write!(f, "ollama"),
            ProviderType::OpenAICompatible => write!(f, "openai_compatible"),
            ProviderType::Moonshot => write!(f, "moonshot"),
            ProviderType::Kimi => write!(f, "kimi"),
            ProviderType::Minimax => write!(f, "minimax"),
        }
    }
}

/// Model configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model name/ID
    pub name: String,
    /// Maximum tokens to generate
    pub max_tokens: u32,
    /// Temperature (0.0 - 2.0)
    pub temperature: f32,
    /// Top-p sampling
    pub top_p: f32,
    /// Presence penalty
    #[serde(default)]
    pub presence_penalty: f32,
    /// Frequency penalty
    #[serde(default)]
    pub frequency_penalty: f32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            name: "gpt-4o-mini".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        }
    }
}

impl ProviderConfig {
    /// Get API key (plain text, no secret resolution)
    ///
    /// Returns the API key from config or environment variable.
    /// For environment variables, use `${env:VAR_NAME}` syntax.
    pub fn get_api_key(&self) -> anyhow::Result<String> {
        // First check if we have a direct api_key
        if let Some(key) = &self.api_key {
            // Check if it's an env reference
            if key.starts_with("${env:") && key.ends_with('}') {
                let env_var = &key[6..key.len() - 1];
                return std::env::var(env_var)
                    .map_err(|_| anyhow::anyhow!("Environment variable '{env_var}' not found"));
            }
            // Plain value, return as-is
            return Ok(key.clone());
        }

        // Fall back to environment variable
        if let Some(env_var) = &self.api_key_env {
            return std::env::var(env_var)
                .map_err(|_| anyhow::anyhow!(
                    "API key not found. Set '{env_var}' environment variable or provide api_key in config"
                ));
        }

        anyhow::bail!("API key not configured")
    }

    /// Get model configuration
    #[must_use]
    pub fn get_model_config(&self, model_name: &str) -> Option<&ModelConfig> {
        self.models.get(model_name)
    }

    /// Get default model configuration
    #[must_use]
    pub fn default_model_config(&self) -> Option<&ModelConfig> {
        self.get_model_config(&self.default_model)
    }

    /// Create `OpenAI` config
    #[must_use]
    pub fn openai(api_key: &str, model: &str) -> Self {
        let mut config = Self::default();
        config.provider_type = ProviderType::OpenAI;
        config.api_key = Some(api_key.to_string());
        config.default_model = "default".to_string();
        config.models.insert(
            "default".to_string(),
            ModelConfig {
                name: model.to_string(),
                ..ModelConfig::default()
            },
        );
        config
    }

    /// Create Ollama config
    #[must_use]
    pub fn ollama(base_url: &str, model: &str) -> Self {
        let mut config = Self::default();
        config.provider_type = ProviderType::Ollama;
        config.base_url = Some(base_url.to_string());
        config.default_model = "default".to_string();
        config.models.insert(
            "default".to_string(),
            ModelConfig {
                name: model.to_string(),
                ..ModelConfig::default()
            },
        );
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ProviderConfig::default();
        assert_eq!(config.provider_type, ProviderType::OpenAI);
        assert_eq!(config.timeout_seconds, 300);
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_model_config() {
        let config = ModelConfig::default();
        assert_eq!(config.name, "gpt-4o-mini");
        assert!((config.temperature - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_provider_type_display() {
        assert_eq!(ProviderType::OpenAI.to_string(), "openai");
        assert_eq!(ProviderType::Anthropic.to_string(), "anthropic");
        assert_eq!(ProviderType::Ollama.to_string(), "ollama");
    }

    /// Regression: serde's `snake_case` rename and the `Display` impl must
    /// agree for every variant, otherwise TOML configs that match the CLI's
    /// `peko principal create` output fail to parse. Specifically, the
    /// adjacent-capitals `OpenAI` and `OpenAICompatible` need explicit
    /// `#[serde(rename)]` because heck would otherwise produce
    /// `open_a_i` / `open_a_i_compatible`.
    #[test]
    fn test_provider_type_display_matches_serde_rename() {
        for v in [
            ProviderType::OpenAI,
            ProviderType::Anthropic,
            ProviderType::Ollama,
            ProviderType::OpenAICompatible,
            ProviderType::Moonshot,
            ProviderType::Kimi,
            ProviderType::Minimax,
        ] {
            let s = v.to_string();
            let round: ProviderType = serde_json::from_str(&format!("\"{s}\""))
                .unwrap_or_else(|e| panic!("Display '{s}' not accepted by serde: {e}"));
            assert_eq!(round, v, "round-trip mismatch for {s}");
        }
    }
}
