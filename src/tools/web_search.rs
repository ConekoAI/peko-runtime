//! Web search tool using Brave LLM Context API
//!
//! Provides web search capabilities optimized for AI agents.
//! Uses Brave's LLM Context API which returns pre-extracted, relevance-scored
//! web content ready for LLM consumption—no scraping needed.
//!
//! # Configuration
//! Requires `BRAVE_API_KEY` environment variable or api_key in config.
//! Get a free API key at: https://api.search.brave.com/

use async_trait::async_trait;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, error, warn};

use crate::tools::traits::Tool;

/// Web search tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Enable the tool
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// API key for Brave Search (or use BRAVE_API_KEY env var)
    pub api_key: Option<String>,
    /// Maximum URLs to include in context (1-20)
    #[serde(default = "default_max_urls")]
    pub max_urls: u32,
    /// Maximum tokens per URL (100-5000)
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Cache TTL in seconds
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_seconds: u64,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: None,
            max_urls: 5,
            max_tokens: 2000,
            cache_ttl_seconds: 900, // 15 minutes
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_max_urls() -> u32 {
    5
}

fn default_max_tokens() -> u32 {
    2000
}

fn default_cache_ttl() -> u64 {
    900
}

/// Search arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchArgs {
    /// Search query
    pub query: String,
    /// Number of URLs to include (1-20, default from config)
    pub count: Option<u32>,
}

/// Search result with pre-extracted content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Source URL
    pub url: String,
    /// Page title
    pub title: String,
    /// Extracted content (text, tables, code blocks)
    pub content: String,
    /// Source domain
    pub source: String,
}

/// Search response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Original query
    pub query: String,
    /// Aggregated context from all sources
    pub context: String,
    /// Individual source results
    pub sources: Vec<SearchResult>,
}

/// Web search tool implementation
pub struct WebSearchTool {
    config: WebSearchConfig,
    client: Client,
    cache: std::sync::Mutex<HashMap<String, (SearchResponse, std::time::Instant)>>,
}

impl WebSearchTool {
    /// Create a new web search tool
    pub fn new(config: WebSearchConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Get API key from config or environment
    fn get_api_key(&self) -> Option<String> {
        self.config
            .api_key
            .clone()
            .or_else(|| std::env::var("BRAVE_API_KEY").ok())
    }

    /// Generate cache key
    fn cache_key(&self, query: &str, max_urls: u32) -> String {
        format!("{}:{}", query.to_lowercase(), max_urls)
    }

    /// Check cache for existing results
    fn check_cache(&self, key: &str) -> Option<SearchResponse> {
        let cache = self.cache.lock().ok()?;
        let (response, timestamp) = cache.get(key)?;

        let age = timestamp.elapsed().as_secs();
        if age < self.config.cache_ttl_seconds {
            debug!("Cache hit for key: {}", key);
            Some(response.clone())
        } else {
            debug!("Cache expired for key: {}", key);
            None
        }
    }

    /// Store result in cache
    fn store_cache(&self, key: String, response: SearchResponse) {
        if let Ok(mut cache) = self.cache.lock() {
            // Simple cleanup: remove entries older than 2x TTL
            let now = std::time::Instant::now();
            let ttl = Duration::from_secs(self.config.cache_ttl_seconds * 2);
            cache.retain(|_, (_, timestamp)| now.duration_since(*timestamp) < ttl);

            cache.insert(key, (response, std::time::Instant::now()));
        }
    }

    /// Search using Brave LLM Context API
    async fn search_brave(&self,
        args: &SearchArgs,
    ) -> anyhow::Result<SearchResponse> {
        let api_key = self
            .get_api_key()
            .ok_or_else(|| anyhow::anyhow!("BRAVE_API_KEY not configured. Get a free key at https://api.search.brave.com/"))?;

        let max_urls = args.count.unwrap_or(self.config.max_urls).min(20).max(1);
        let max_tokens = self.config.max_tokens.min(5000).max(100);

        let url = Url::parse_with_params(
            "https://api.search.brave.com/res/v1/llm/context",
            &[
                ("q", args.query.as_str()),
                ("max_urls", &max_urls.to_string()),
                ("max_tokens_per_url", &max_tokens.to_string()),
            ],
        )
        .map_err(|e| anyhow::anyhow!("Invalid URL: {e}"))?;

        debug!("Brave LLM Context URL: {}", url);

        let response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", api_key)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            
            let message = match status.as_u16() {
                401 => "Invalid Brave API key. Please check your BRAVE_API_KEY.".to_string(),
                429 => "Brave API rate limit exceeded. Please try again later.".to_string(),
                _ => format!("Brave API error: {} - {}", status, error_text),
            };
            
            return Err(anyhow::anyhow!(message));
        }

        let llm_response: BraveLlmContextResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse response: {e}"))?;

        // Convert to our SearchResult format
        let sources: Vec<SearchResult> = llm_response
            .urls
            .into_iter()
            .map(|u| SearchResult {
                url: u.url.clone(),
                title: u.title.clone(),
                content: u.content.clone(),
                source: extract_domain(&u.url),
            })
            .collect();

        Ok(SearchResponse {
            query: args.query.clone(),
            context: llm_response.context,
            sources,
        })
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web using Brave LLM Context API. Returns pre-extracted, relevance-scored content optimized for AI agents. Requires BRAVE_API_KEY environment variable."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to execute"
                },
                "count": {
                    "type": "integer",
                    "description": "Number of URLs to include in context (1-20)",
                    "minimum": 1,
                    "maximum": 20
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let args: SearchArgs =
            serde_json::from_value(args).map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        if args.query.is_empty() {
            return Err(anyhow::anyhow!("Query cannot be empty"));
        }

        let count = args.count.unwrap_or(self.config.max_urls).min(20).max(1);
        let cache_key = self.cache_key(&args.query, count);

        // Check cache
        if let Some(cached) = self.check_cache(&cache_key) {
            return Ok(serde_json::to_value(cached)?);
        }

        // Perform search with Brave LLM Context API
        let result = self.search_brave(&args).await;

        match result {
            Ok(response) => {
                // Store in cache
                self.store_cache(cache_key, response.clone());
                Ok(serde_json::to_value(response)?)
            }
            Err(e) => Err(anyhow::anyhow!("Search failed: {e}")),
        }
    }
}

/// Extract domain from URL
fn extract_domain(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Brave LLM Context API response structure
#[derive(Debug, Deserialize)]
struct BraveLlmContextResponse {
    /// Aggregated context from all sources
    context: String,
    /// Individual URL results
    urls: Vec<BraveUrlResult>,
}

#[derive(Debug, Deserialize)]
struct BraveUrlResult {
    /// Source URL
    url: String,
    /// Page title
    title: String,
    /// Extracted content
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_generation() {
        let config = WebSearchConfig::default();
        let tool = WebSearchTool::new(config);

        let key1 = tool.cache_key("rust", 5);
        let key2 = tool.cache_key("rust", 5);
        let key3 = tool.cache_key("Rust", 5); // case insensitive
        let key4 = tool.cache_key("rust", 3);

        assert_eq!(key1, key2);
        assert_eq!(key1, key3); // case insensitive
        assert_ne!(key1, key4); // different count
    }

    #[test]
    fn test_extract_domain() {
        assert_eq!(
            extract_domain("https://example.com/path"),
            "example.com"
        );
        assert_eq!(
            extract_domain("https://www.example.com/path"),
            "www.example.com"
        );
        assert_eq!(extract_domain("not-a-url"), "unknown");
    }
}
