//! `OpenRouter` provider implementation
//! Provides access to 100+ models through a single API

use super::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

/// `OpenRouter` provider
pub struct OpenRouterProvider {
    api_key: String,
    client: Client,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

impl OpenRouterProvider {
    /// Create new `OpenRouter` provider
    pub fn new(api_key: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());

        info!("OpenRouter provider initialized");

        Self { api_key, client }
    }

    /// Create from environment variable
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| anyhow::anyhow!("OPENROUTER_API_KEY not set"))?;
        Ok(Self::new(api_key))
    }

    /// Warm up HTTP connection pool
    pub async fn warmup(&self) -> anyhow::Result<()> {
        self.client
            .get("https://openrouter.ai/api/v1/auth/key")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        // Default to anthropic/claude-3.5-sonnet
        self.chat_with_system(None, prompt, "anthropic/claude-3.5-sonnet", 0.7)
            .await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
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

        let request = ChatRequest {
            model: model.to_string(),
            messages,
            temperature,
        };

        debug!("Sending request to OpenRouter: model={}", model);

        let response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://pekobot.dev")
            .header("X-Title", "Pekobot")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("OpenRouter API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!(
                "OpenRouter API error: {status} - {error_text}"
            ));
        }

        let chat_response: ChatResponse = response.json().await?;

        let content = chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        debug!("Received response from OpenRouter");

        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openrouter_provider_creation() {
        let provider = OpenRouterProvider::new("test-key".to_string());
        assert_eq!(provider.name(), "openrouter");
    }
}
