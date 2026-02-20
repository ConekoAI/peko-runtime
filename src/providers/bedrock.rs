//! AWS Bedrock provider implementation
//! Access to Claude, Llama, and other models via AWS

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;

use crate::providers::Provider;

/// AWS Bedrock provider
pub struct BedrockProvider {
    access_key_id: String,
    secret_access_key: String,
    region: String,
    model: String,
    client: reqwest::Client,
}

impl BedrockProvider {
    /// Create new Bedrock provider
    #[must_use]
    pub fn new(access_key_id: String, secret_access_key: String, region: String) -> Self {
        Self {
            access_key_id,
            secret_access_key,
            region,
            model: "anthropic.claude-3-sonnet-20240229-v1:0".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self> {
        let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
            .context("AWS_ACCESS_KEY_ID environment variable not set")?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .context("AWS_SECRET_ACCESS_KEY environment variable not set")?;
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());

        Ok(Self::new(access_key_id, secret_access_key, region))
    }

    /// Set model
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Get the invoke URL for the model
    fn invoke_url(&self) -> String {
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
            self.region, self.model
        )
    }
}

#[async_trait]
impl Provider for BedrockProvider {
    fn name(&self) -> &'static str {
        "bedrock"
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
        // AWS Signature Version 4 signing required
        // For simplicity, this is a basic implementation
        // Full implementation would use aws-sigv4

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
            "anthropic_version": "bedrock-2023-05-31",
            "messages": messages,
            "max_tokens": 4096,
            "temperature": temperature
        });

        let url = format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
            self.region, model
        );

        // Note: This is a simplified version. Full AWS SigV4 signing is complex
        // and would typically use the aws-sdk-bedrock crate
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            // AWS Signature V4 headers would go here
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Bedrock API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Bedrock API error ({status}): {error_text}");
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Bedrock API response")?;

        // Claude format on Bedrock
        let content = result
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|c| c.as_str())
            .context("No content in Bedrock response")?;

        Ok(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bedrock_provider_creation() {
        let provider = BedrockProvider::new(
            "AKIA...".to_string(),
            "secret...".to_string(),
            "us-east-1".to_string(),
        )
        .with_model("anthropic.claude-3-haiku-20240307-v1:0");

        assert_eq!(provider.name(), "bedrock");
    }
}
