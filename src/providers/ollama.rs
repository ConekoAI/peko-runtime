//! Ollama provider implementation for local LLMs

use super::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

/// Ollama API configuration
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub timeout_seconds: u64,
    /// Context window size (Ollama-specific)
    pub num_ctx: Option<u32>,
    /// Number of GPU layers to use
    pub num_gpu: Option<u32>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".to_string(),
            model: "llama3.2".to_string(),
            temperature: 0.7,
            timeout_seconds: 120, // Longer timeout for local models
            num_ctx: Some(4096),
            num_gpu: None,
        }
    }
}

impl OllamaConfig {
    /// Create config with custom base URL
    pub fn with_host(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            ..Default::default()
        }
    }

    /// Create config with model
    pub fn with_model(model: &str) -> Self {
        Self {
            model: model.to_string(),
            ..Default::default()
        }
    }
}

/// Ollama provider
pub struct OllamaProvider {
    config: OllamaConfig,
    client: Client,
}

impl OllamaProvider {
    /// Create a new Ollama provider
    pub fn new(config: OllamaConfig) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()?;

        info!("Ollama provider initialized with model: {}", config.model);

        Ok(Self { config, client })
    }

    /// Create with default local configuration
    pub fn local() -> anyhow::Result<Self> {
        Self::new(OllamaConfig::default())
    }

    /// Check if Ollama is available
    pub async fn is_available(&self) -> bool {
        match self.client.get(format!("{}/api/tags", self.config.base_url)).send().await {
            Ok(response) -> response.status().is_success(),
            Err(_) -> false,
        }
    }

    /// List available models
    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.config.base_url))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Failed to list models: {}", response.status()));
        }

        let tags: TagsResponse = response.json().await?;
        let models = tags.models.into_iter().map(|m| m.name).collect();
        Ok(models)
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        let mut options = serde_json::Map::new();
        options.insert("temperature".to_string(), serde_json::json!(self.config.temperature));
        
        if let Some(ctx) = self.config.num_ctx {
            options.insert("num_ctx".to_string(), serde_json::json!(ctx));
        }
        
        if let Some(gpu) = self.config.num_gpu {
            options.insert("num_gpu".to_string(), serde_json::json!(gpu));
        }

        let request = GenerateRequest {
            model: self.config.model.clone(),
            prompt: prompt.to_string(),
            stream: false,
            options: Some(options),
        };

        debug!("Sending request to Ollama: model={}", self.config.model);

        let response = self
            .client
            .post(format!("{}/api/generate", self.config.base_url))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Ollama API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!("Ollama API error: {} - {}", status, error_text));
        }

        let completion: GenerateResponse = response.json().await?;

        debug!(
            "Received response from Ollama: prompt_tokens={}, completion_tokens={}",
            completion.prompt_eval_count.unwrap_or(0),
            completion.eval_count.unwrap_or(0)
        );

        Ok(completion.response)
    }
}

// Ollama API types

#[derive(Debug, Serialize)]
struct GenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    model: String,
    created_at: String,
    response: String,
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    name: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    modified_at: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    digest: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_config_default() {
        let config = OllamaConfig::default();
        assert_eq!(config.model, "llama3.2");
        assert_eq!(config.base_url, "http://localhost:11434");
        assert_eq!(config.temperature, 0.7);
    }

    #[test]
    fn test_ollama_config_with_host() {
        let config = OllamaConfig::with_host("http://192.168.1.100:11434");
        assert_eq!(config.base_url, "http://192.168.1.100:11434");
    }

    #[test]
    fn test_ollama_provider_creation() {
        let provider = OllamaProvider::local();
        assert!(provider.is_ok());
        
        let provider = provider.unwrap();
        assert_eq!(provider.name(), "ollama");
    }

    // Note: This test requires a running Ollama instance
    #[tokio::test]
    #[ignore]
    async fn test_ollama_availability() {
        let provider = OllamaProvider::local().unwrap();
        // This may fail if Ollama is not running
        let _ = provider.is_available().await;
    }
}
