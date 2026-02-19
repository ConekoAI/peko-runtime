//! Together AI provider implementation
//! High-performance inference for open-source models

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;

use crate::providers::Provider;

/// Together AI provider
pub struct TogetherProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl TogetherProvider {
    /// Create new Together provider from API key
    #[must_use] 
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "meta-llama/Llama-3.1-70B-Instruct-Turbo".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create from environment variable
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("TOGETHER_API_KEY")
            .context("TOGETHER_API_KEY environment variable not set")?;
        Ok(Self::new(api_key))
    }

    /// Set model
    #[must_use] 
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }
}

#[async_trait]
impl Provider for TogetherProvider {
    fn name(&self) -> &'static str {
        "together"
    }

    async fn complete(&self, prompt: &str) -> Result<String> {
        self.chat_with_system(None, prompt, &self.model, 0.7).await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> Result<String> {
        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(json!({
                "role": "system",
                "content": sys
            }));
        }

        messages.push(json!({
            "role": "user",
            "content": message
        }));

        let body = json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": 4096
        });

        let response = self
            .client
            .post("https://api.together.xyz/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Together API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Together API error ({status}): {error_text}");
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Together API response")?;

        let content = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .context("No content in Together response")?;

        Ok(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_together_provider_creation() {
        let provider =
            TogetherProvider::new("test-key".to_string()).with_model("meta-llama/Llama-3.1-8B");

        assert_eq!(provider.name(), "together");
    }
}
