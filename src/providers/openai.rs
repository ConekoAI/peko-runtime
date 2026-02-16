//! OpenAI provider implementation

use super::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

/// OpenAI API configuration
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout_seconds: u64,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }
}

impl OpenAIConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY not set"))?;
        
        Ok(Self {
            api_key,
            ..Default::default()
        })
    }
}

/// OpenAI provider
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: Client,
}

impl OpenAIProvider {
    /// Create a new OpenAI provider
    pub fn new(config: OpenAIConfig) -> anyhow::Result<Self> {
        if config.api_key.is_empty() {
            return Err(anyhow::anyhow!("OpenAI API key is required"));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()?;

        info!("OpenAI provider initialized with model: {}", config.model);

        Ok(Self { config, client })
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Self::new(OpenAIConfig::from_env()?)
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            max_tokens: Some(self.config.max_tokens),
            temperature: Some(self.config.temperature),
        };

        debug!("Sending request to OpenAI: model={}", self.config.model);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("OpenAI API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!("OpenAI API error: {} - {}", status, error_text));
        }

        let completion: ChatCompletionResponse = response.json().await?;

        let content = completion
            .choices
            .into_iter()
            .next()
            .and_then(|c| Some(c.message.content))
            .unwrap_or_default();

        debug!("Received {} tokens from OpenAI", completion.usage.total_tokens);

        Ok(content)
    }
}

// OpenAI API types

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Usage {
    total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_config_default() {
        let config = OpenAIConfig::default();
        assert_eq!(config.model, "gpt-4o-mini");
        assert_eq!(config.max_tokens, 4096);
        assert_eq!(config.temperature, 0.7);
    }

    // Note: Tests requiring actual API calls are skipped without OPENAI_API_KEY
    #[tokio::test]
    async fn test_openai_provider_creation() {
        // This will fail without API key - that's expected
        let result = OpenAIProvider::from_env();
        // We expect an error if key is not set
        if std::env::var("OPENAI_API_KEY").is_err() {
            assert!(result.is_err());
        }
    }
}
