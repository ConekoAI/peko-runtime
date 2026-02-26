//! Embedding providers for semantic search
//!
//! Supports multiple embedding backends:
//! - `OpenAI` (text-embedding-3-small)
//! - Gemini (gemini-embedding-001)
//! - Local (GGUF models via llama.cpp)
//! - Ollama (local API)

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Embedding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Provider type
    pub provider: EmbeddingProvider,
    /// Model name
    pub model: String,
    /// API key (if needed)
    pub api_key: Option<String>,
    /// Base URL (for custom endpoints)
    pub base_url: Option<String>,
    /// Embedding dimension
    pub dimension: usize,
    /// Request timeout (seconds)
    pub timeout_seconds: u64,
    /// Max retries
    pub max_retries: u32,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: EmbeddingProvider::OpenAI,
            model: "text-embedding-3-small".to_string(),
            api_key: None,
            base_url: None,
            dimension: 1536,
            timeout_seconds: 30,
            max_retries: 3,
        }
    }
}

/// Embedding provider types
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingProvider {
    /// `OpenAI` embeddings API
    OpenAI,
    /// Google Gemini embeddings
    Gemini,
    /// Local GGUF model
    Local,
    /// Ollama local API
    Ollama,
}

impl std::fmt::Display for EmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbeddingProvider::OpenAI => write!(f, "openai"),
            EmbeddingProvider::Gemini => write!(f, "gemini"),
            EmbeddingProvider::Local => write!(f, "local"),
            EmbeddingProvider::Ollama => write!(f, "ollama"),
        }
    }
}

/// Trait for embedding providers
#[async_trait]
pub trait EmbeddingProviderTrait: Send + Sync {
    /// Generate embedding for text
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Generate embeddings for multiple texts (batch)
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// Get embedding dimension
    fn dimension(&self) -> usize;

    /// Get provider name
    fn name(&self) -> &str;
}

/// `OpenAI` embedding provider
pub struct OpenAIEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    dimension: usize,
}

impl OpenAIEmbedder {
    /// Create a new `OpenAI` embedder
    #[must_use]
    pub fn new(api_key: String, model: Option<String>, base_url: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            model: model.unwrap_or_else(|| "text-embedding-3-small".to_string()),
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            dimension: 1536,
        }
    }
}

#[async_trait]
impl EmbeddingProviderTrait for OpenAIEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/embeddings", self.base_url);

        let body = serde_json::json!({
            "input": text,
            "model": self.model,
        });

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .context("Failed to send embedding request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Embedding API error {status}: {text}"));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse embedding response")?;

        let embedding = result["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid embedding response format"))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        debug!("Generated embedding with OpenAI {}", self.model);
        Ok(embedding)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn name(&self) -> &'static str {
        "openai"
    }
}

/// Gemini embedding provider
pub struct GeminiEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    dimension: usize,
}

impl GeminiEmbedder {
    /// Create a new Gemini embedder
    #[must_use]
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            model: model.unwrap_or_else(|| "embedding-001".to_string()),
            base_url: "https://generativelanguage.googleapis.com/v1".to_string(),
            dimension: 768,
        }
    }
}

#[async_trait]
impl EmbeddingProviderTrait for GeminiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!(
            "{}/models/{}:embedContent?key={}",
            self.base_url, self.model, self.api_key
        );

        let body = serde_json::json!({
            "content": {
                "parts": [{"text": text}]
            }
        });

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .context("Failed to send Gemini embedding request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Gemini API error {status}: {text}"));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Gemini embedding response")?;

        let embedding = result["embedding"]["values"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid Gemini embedding response format"))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        debug!("Generated embedding with Gemini {}", self.model);
        Ok(embedding)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn name(&self) -> &'static str {
        "gemini"
    }
}

/// Ollama embedding provider
pub struct OllamaEmbedder {
    client: reqwest::Client,
    model: String,
    base_url: String,
    dimension: usize,
}

impl OllamaEmbedder {
    /// Create a new Ollama embedder
    #[must_use]
    pub fn new(model: String, base_url: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            model,
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434".to_string()),
            dimension: 4096, // Default for most Ollama models
        }
    }
}

#[async_trait]
impl EmbeddingProviderTrait for OllamaEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/api/embeddings", self.base_url);

        let body = serde_json::json!({
            "model": self.model,
            "prompt": text,
        });

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .context("Failed to send Ollama embedding request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Ollama API error {status}: {text}"));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Ollama embedding response")?;

        let embedding = result["embedding"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid Ollama embedding response format"))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        debug!("Generated embedding with Ollama {}", self.model);
        Ok(embedding)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn name(&self) -> &'static str {
        "ollama"
    }
}

/// Factory for creating embedding providers
pub struct EmbeddingProviderFactory;

impl EmbeddingProviderFactory {
    /// Create an embedding provider from configuration
    pub fn create(config: &EmbeddingConfig) -> Result<Arc<dyn EmbeddingProviderTrait>> {
        match config.provider {
            EmbeddingProvider::OpenAI => {
                let api_key = config
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                    .ok_or_else(|| anyhow::anyhow!("OpenAI API key not found"))?;

                Ok(Arc::new(OpenAIEmbedder::new(
                    api_key,
                    Some(config.model.clone()),
                    config.base_url.clone(),
                )))
            }

            EmbeddingProvider::Gemini => {
                let api_key = config
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                    .ok_or_else(|| anyhow::anyhow!("Gemini API key not found"))?;

                Ok(Arc::new(GeminiEmbedder::new(
                    api_key,
                    Some(config.model.clone()),
                )))
            }

            EmbeddingProvider::Ollama => Ok(Arc::new(OllamaEmbedder::new(
                config.model.clone(),
                config.base_url.clone(),
            ))),

            EmbeddingProvider::Local => {
                warn!("Local embeddings not yet implemented");
                Err(anyhow::anyhow!("Local embeddings not yet implemented"))
            }
        }
    }

    /// Try to create from environment variables
    pub fn from_env() -> Result<Arc<dyn EmbeddingProviderTrait>> {
        // Try OpenAI first
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            info!("Creating OpenAI embedder from environment");
            return Ok(Arc::new(OpenAIEmbedder::new(api_key, None, None)));
        }

        // Try Gemini second
        if let Ok(api_key) = std::env::var("GEMINI_API_KEY") {
            info!("Creating Gemini embedder from environment");
            return Ok(Arc::new(GeminiEmbedder::new(api_key, None)));
        }

        // Try Ollama (no key needed)
        if std::env::var("OLLAMA_HOST").is_ok() || std::env::var("OLLAMA_MODEL").is_ok() {
            info!("Creating Ollama embedder from environment");
            let model =
                std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "nomic-embed-text".to_string());
            let host = std::env::var("OLLAMA_HOST").ok();
            return Ok(Arc::new(OllamaEmbedder::new(model, host)));
        }

        Err(anyhow::anyhow!(
            "No embedding provider configured. Set OPENAI_API_KEY, GEMINI_API_KEY, or OLLAMA_HOST."
        ))
    }
}

/// Semantic memory service - combines vector storage with embedding generation
pub struct SemanticMemory {
    vector_memory: crate::memory::vector::VectorMemory,
    embedder: Arc<dyn EmbeddingProviderTrait>,
}

impl SemanticMemory {
    /// Create a new semantic memory service
    pub fn new(
        vector_memory: crate::memory::vector::VectorMemory,
        embedder: Arc<dyn EmbeddingProviderTrait>,
    ) -> Self {
        Self {
            vector_memory,
            embedder,
        }
    }

    /// Store content with automatic embedding
    pub async fn store(
        &self,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let embedding = self
            .embedder
            .embed(content)
            .await
            .context("Failed to generate embedding")?;

        self.vector_memory
            .store(content, embedding, Some(self.embedder.name()), metadata)
    }

    /// Search by semantic similarity
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        min_similarity: f32,
    ) -> Result<Vec<crate::memory::vector::SimilarityResult>> {
        let query_embedding = self
            .embedder
            .embed(query)
            .await
            .context("Failed to generate query embedding")?;

        self.vector_memory
            .search_similar(&query_embedding, limit, min_similarity)
    }

    /// Search with hybrid scoring (semantic + keyword)
    pub async fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        _semantic_weight: f32,
    ) -> Result<Vec<crate::memory::vector::SimilarityResult>> {
        // First get semantic results
        let semantic_results = self.search(query, limit * 2, -1.0).await?;

        // For now, just return semantic results
        // In a full implementation, we'd also do keyword search and merge
        let mut results = semantic_results;
        results.truncate(limit);

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_config_default() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.provider, EmbeddingProvider::OpenAI);
        assert_eq!(config.model, "text-embedding-3-small");
        assert_eq!(config.dimension, 1536);
    }

    #[test]
    fn test_provider_display() {
        assert_eq!(EmbeddingProvider::OpenAI.to_string(), "openai");
        assert_eq!(EmbeddingProvider::Gemini.to_string(), "gemini");
        assert_eq!(EmbeddingProvider::Ollama.to_string(), "ollama");
    }

    #[test]
    fn test_gemini_embedder_creation() {
        let embedder = GeminiEmbedder::new("test-key".to_string(), None);
        assert_eq!(embedder.name(), "gemini");
        assert_eq!(embedder.dimension(), 768);
    }

    #[test]
    fn test_ollama_embedder_creation() {
        let embedder = OllamaEmbedder::new("nomic-embed-text".to_string(), None);
        assert_eq!(embedder.name(), "ollama");
        assert_eq!(embedder.model, "nomic-embed-text");
    }
}
