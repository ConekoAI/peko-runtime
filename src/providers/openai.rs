//! `OpenAI` provider implementation

use super::traits::Provider;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

/// `OpenAI` API configuration
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

/// `OpenAI` provider
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: Client,
}

impl OpenAIProvider {
    /// Create a new `OpenAI` provider
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

    /// Build messages from system prompt and user message
    fn build_messages(&self, system_prompt: Option<&str>, message: &str) -> Vec<Message> {
        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        messages
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat(
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
        let request = ChatCompletionRequest {
            model: model.to_string(),
            messages: self.build_messages(system_prompt, message),
            max_tokens: Some(self.config.max_tokens),
            temperature: Some(temperature as f32),
            stream: None,
        };

        debug!("Sending request to OpenAI: model={}", model);

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
            return Err(anyhow::anyhow!("OpenAI API error: {status} - {error_text}"));
        }

        let completion: ChatCompletionResponse = response.json().await?;

        let content = completion
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        debug!(
            "Received {} tokens from OpenAI",
            completion.usage.total_tokens
        );

        Ok(content)
    }

    async fn complete_stream(
        &self,
        prompt: &str,
        event_tx: tokio::sync::mpsc::Sender<crate::engine::AgenticEvent>,
        run_id: String,
    ) -> anyhow::Result<()> {
        // Use default implementation (blocking with events)
        // Full streaming implementation requires reqwest stream feature
        <Self as Provider>::complete_stream(self, prompt, event_tx, run_id).await
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
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
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

// Streaming types
#[derive(Debug, Deserialize)]
struct StreamDelta {
    id: String,
    object: String,
    choices: Vec<DeltaChoice>,
}

#[derive(Debug, Deserialize)]
struct DeltaChoice {
    index: u32,
    delta: DeltaContent,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeltaContent {
    role: Option<String>,
    content: Option<String>,
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
