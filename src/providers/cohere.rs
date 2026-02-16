//! Cohere provider implementation
//! Enterprise-grade language models

use async_trait::async_trait;
use serde_json::json;
use anyhow::{Context, Result};

use crate::providers::Provider;

/// Cohere provider
pub struct CohereProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl CohereProvider {
    /// Create new Cohere provider from API key
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "command-r-plus".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create from environment variable
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("COHERE_API_KEY")
            .context("COHERE_API_KEY environment variable not set")?;
        Ok(Self::new(api_key))
    }

    /// Set model
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }
}

#[async_trait]
impl Provider for CohereProvider {
    fn name(&self) -> &str {
        "cohere"
    }

    async fn complete(
        &self,
        prompt: &str,
    ) -> Result<String> {
        self.chat_with_system(None, prompt, &self.model, 0.7).await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> Result<String> {
        let mut body = json!({
            "model": model,
            "message": message,
            "temperature": temperature,
            "max_tokens": 4096
        });

        if let Some(sys) = system_prompt {
            body["preamble"] = json!(sys);
        }

        let response = self
            .client
            .post("https://api.cohere.ai/v1/chat")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Cohere API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Cohere API error ({}): {}", status, error_text);
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Cohere API response")?;

        let content = result
            .get("text")
            .and_then(|c| c.as_str())
            .context("No content in Cohere response")?;

        Ok(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cohere_provider_creation() {
        let provider = CohereProvider::new("test-key".to_string())
            .with_model("command-r");
        
        assert_eq!(provider.name(), "cohere");
    }
}
