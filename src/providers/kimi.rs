//! Kimi provider implementation

use async_trait::async_trait;
use serde_json::json;
use anyhow::{Context, Result};

use crate::providers::Provider;

/// Kimi (Moonshot) provider
pub struct KimiProvider {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
}

impl KimiProvider {
    /// Create new Kimi provider from environment
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("KIMI_API_KEY")
            .or_else(|_| std::env::var("MOONSHOT_API_KEY"))
            .context("KIMI_API_KEY or MOONSHOT_API_KEY environment variable required")?;
        
        Ok(Self::new(api_key))
    }

    /// Create new Kimi provider with API key
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "kimi-k2.5".to_string(),
            base_url: "https://api.moonshot.cn/v1".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Set model
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Build request body
    fn build_request_body(&self,
        messages: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        json!({
            "model": self.model,
            "messages": messages,
            "temperature": 0.7,
            "stream": false
        })
    }
}

#[async_trait]
impl Provider for KimiProvider {
    fn name(&self) -> &str {
        "kimi"
    }

    async fn complete(
        &self,
        prompt: &str,
    ) -> Result<String> {
        let messages = vec![
            json!({
                "role": "user",
                "content": prompt
            })
        ];

        let body = self.build_request_body(messages);

        let response = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Kimi API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Kimi API error ({}): {}", status, error_text);
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Kimi API response")?;

        let content = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .context("No content in Kimi response")?;

        Ok(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kimi_provider_creation() {
        let provider = KimiProvider::new("test-api-key".to_string())
            .with_model("kimi-k2.5");
        
        assert_eq!(provider.name(), "kimi");
    }

    #[test]
    fn test_build_request_body() {
        let provider = KimiProvider::new("test".to_string());
        let messages = vec![
            json!({"role": "user", "content": "Hello"})
        ];
        
        let body = provider.build_request_body(messages);
        assert_eq!(body["model"], "kimi-k2.5");
        assert!(body["messages"].as_array().unwrap().len() > 0);
    }
}
