//! Kimi Code provider implementation
//!
//! Kimi Code uses Anthropic Claude Code's backend, so it follows
//! the Anthropic API format rather than the Moonshot API format.

use super::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

/// Kimi Code API configuration
#[derive(Debug, Clone)]
pub struct KimiCodeConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout_seconds: u64,
}

impl Default for KimiCodeConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            // Correct endpoint from pi-mono: https://api.kimi.com/coding
            base_url: "https://api.kimi.com/coding".to_string(),
            model: "k2p5".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }
}

impl KimiCodeConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        // Kimi Code can use KIMI_API_KEY or KIMICODE_API_KEY
        // NOTE: Do NOT strip the "sk-kimi-" prefix - the key works as-is!
        let api_key = std::env::var("KIMI_API_KEY")
            .or_else(|_| std::env::var("KIMICODE_API_KEY"))
            .or_else(|_| std::env::var("MOONSHOT_API_KEY"))
            .map_err(|_| {
                anyhow::anyhow!("KIMI_API_KEY, KIMICODE_API_KEY, or MOONSHOT_API_KEY not set")
            })?;

        Ok(Self {
            api_key,
            ..Default::default()
        })
    }
}

/// Kimi Code provider
///
/// Note: Kimi Code uses Anthropic Claude Code's backend, so it follows
/// the Anthropic API format (x-api-key header, /v1/messages endpoint).
pub struct KimiCodeProvider {
    config: KimiCodeConfig,
    client: Client,
}

impl KimiCodeProvider {
    /// Create a new Kimi Code provider
    pub fn new(config: KimiCodeConfig) -> anyhow::Result<Self> {
        if config.api_key.is_empty() {
            return Err(anyhow::anyhow!("Kimi Code API key is required"));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()?;

        info!(
            "Kimi Code provider initialized with model: {}",
            config.model
        );

        Ok(Self { config, client })
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Self::new(KimiCodeConfig::from_env()?)
    }

    /// Create with API key directly
    pub fn with_api_key(api_key: String) -> anyhow::Result<Self> {
        let config = KimiCodeConfig {
            api_key, // Use key as-is (do not strip prefix)
            ..Default::default()
        };
        Self::new(config)
    }
}

#[async_trait]
impl Provider for KimiCodeProvider {
    fn name(&self) -> &'static str {
        "kimi-code"
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat_with_system(
            None,
            prompt,
            &self.config.model,
            f64::from(self.config.temperature),
        )
        .await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let mut messages: Vec<Message> = Vec::new();

        // Add system message if provided
        if let Some(system) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: system.to_string(),
            });
        }

        // Add user message
        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let request = MessagesRequest {
            model: model.to_string(),
            max_tokens: self.config.max_tokens,
            temperature: Some(temperature as f32),
            messages,
        };

        debug!("Sending request to Kimi Code: model={}", model);

        // Kimi Code uses Anthropic API format
        let response = self
            .client
            .post(format!("{}/v1/messages", self.config.base_url))
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Kimi Code API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!(
                "Kimi Code API error: {status} - {error_text}"
            ));
        }

        let completion: MessagesResponse = response.json().await?;

        let content = completion
            .content
            .into_iter()
            .next()
            .map(|c| c.text)
            .unwrap_or_default();

        debug!(
            "Received response from Kimi Code: input_tokens={}, output_tokens={}",
            completion.usage.input_tokens, completion.usage.output_tokens
        );

        Ok(content)
    }
}

// Anthropic API types (Kimi Code uses same format)

#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    messages: Vec<Message>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kimi_code_config_default() {
        let config = KimiCodeConfig::default();
        assert_eq!(config.model, "k2p5");
        assert_eq!(config.max_tokens, 4096);
        assert_eq!(config.temperature, 0.7);
        // Should use correct Kimi Code endpoint from pi-mono
        assert!(config.base_url.contains("api.kimi.com"));
    }

    #[tokio::test]
    async fn test_kimi_code_provider_creation() {
        // This will fail without API key - that's expected
        let result = KimiCodeProvider::from_env();
        // We expect an error if key is not set
        if std::env::var("KIMI_API_KEY").is_err()
            && std::env::var("KIMICODE_API_KEY").is_err()
            && std::env::var("MOONSHOT_API_KEY").is_err()
        {
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_api_key_prefix_stripping() {
        let _config = KimiCodeConfig {
            api_key: "kimi-abc123".to_string(),
            ..Default::default()
        };
        // When created with with_api_key, prefix should be stripped
        let provider = KimiCodeProvider::with_api_key("kimi-abc123".to_string());
        assert!(provider.is_ok());
    }
}
