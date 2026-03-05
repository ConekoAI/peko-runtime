//! Web search tool
//!
//! Provides web search capabilities using Brave Search API.
//! Results are cached for 15 minutes to reduce API calls.
//!
//! # Configuration
//! Requires `BRAVE_API_KEY` environment variable or api_key in config.
//! Get a free API key at: https://api.search.brave.com/

use async_trait::async_trait;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::tools::traits::Tool;

/// Search provider (only Brave supported now)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    /// Brave Search API (requires API key)
    Brave,
}

impl Default for SearchProvider {
    fn default() -> Self {
        SearchProvider::Brave
    }
}

/// Web search tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Enable the tool
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// API key for Brave Search (or use BRAVE_API_KEY env var)
    pub api_key: Option<String>,
    /// Maximum results per query (1-20)
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    /// Cache TTL in seconds
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_seconds: u64,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: None,
            max_results: 10,
            cache_ttl_seconds: 900, // 15 minutes
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_max_results() -> u32 {
    10
}

fn default_cache_ttl() -> u64 {
    900
}

/// Search arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchArgs {
    /// Search query
    pub query: String,
    /// Number of results (1-20, default from config)
    pub count: Option<u32>,
    /// Freshness filter: "pd" (past day), "pw" (past week), "pm" (past month), "py" (past year)
    pub freshness: Option<String>,
}

/// Individual search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Result title
    pub title: String,
    /// Result URL
    pub url: String,
    /// Snippet/description
    pub snippet: String,
    /// Source domain (extracted from URL)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Published date if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

/// Search response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Original query
    pub query: String,
    /// Total results found (may be None for some providers)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_results: Option<u32>,
    /// Search results
    pub results: Vec<SearchResult>,
    /// Provider used
    pub provider: String,
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
    fn cache_key(&self, query: &str, count: u32, freshness: Option<&str>) -> String {
        format!("{}:{}:{:?}", query.to_lowercase(), count, freshness)
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

    /// Search using Brave API
    async fn search_brave(&self, args: &SearchArgs) -> anyhow::Result<SearchResponse> {
        let api_key = self
            .get_api_key()
            .ok_or_else(|| anyhow::anyhow!("BRAVE_API_KEY not configured. Get a free key at https://api.search.brave.com/"))?;

        let count = args.count.unwrap_or(self.config.max_results).min(20).max(1);

        let url = Url::parse_with_params(
            "https://api.search.brave.com/res/v1/web/search",
            &[
                ("q", args.query.as_str()),
                ("count", &count.to_string()),
                ("offset", "0"),
                ("mkt", "en-US"),
            ],
        )
        .map_err(|e| anyhow::anyhow!("Invalid URL: {e}"))?;

        debug!("Brave search URL: {}", url);

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
            
            // Provide helpful error messages
            return Err(anyhow::anyhow!(
                "{}",
                match status.as_u16() {
                    401 => "Invalid Brave API key. Please check your BRAVE_API_KEY.".to_string(),
                    429 => "Brave API rate limit exceeded. Please try again later.".to_string(),
                    _ => format!("Brave API error: {} - {}", status, error_text),
                }
            ));
        }

        let brave_response: BraveSearchResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse response: {e}"))?;

        let results: Vec<SearchResult> = brave_response
            .web
            .results
            .into_iter()
            .take(count as usize)
            .map(|r| {
                let source = extract_domain(&r.url);
                SearchResult {
                    title: r.title,
                    url: r.url,
                    snippet: r.description,
                    source,
                    published: r.age,
                }
            })
            .collect();

        Ok(SearchResponse {
            query: args.query.clone(),
            total_results: Some(results.len() as u32),
            results,
            provider: "brave".to_string(),
        })
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web using Brave Search API. Requires BRAVE_API_KEY environment variable."
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
                    "description": "Number of results to return (1-20)",
                    "minimum": 1,
                    "maximum": 20
                },
                "freshness": {
                    "type": "string",
                    "description": "Filter by freshness: pd (past day), pw (past week), pm (past month), py (past year)",
                    "enum": ["pd", "pw", "pm", "py"]
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: SearchArgs =
            serde_json::from_value(args).map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        if args.query.is_empty() {
            return Err(anyhow::anyhow!("Query cannot be empty"));
        }

        let count = args.count.unwrap_or(self.config.max_results).min(20).max(1);
        let cache_key = self.cache_key(&args.query, count, args.freshness.as_deref());

        // Check cache
        if let Some(cached) = self.check_cache(&cache_key) {
            return Ok(serde_json::to_value(cached)?);
        }

        // Perform search with Brave
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
fn extract_domain(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
}

/// Brave Search API response structure
#[derive(Debug, Deserialize)]
struct BraveSearchResponse {
    web: BraveWebResults,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    age: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_generation() {
        let config = WebSearchConfig::default();
        let tool = WebSearchTool::new(config);

        let key1 = tool.cache_key("rust", 10, None);
        let key2 = tool.cache_key("rust", 10, None);
        let key3 = tool.cache_key("Rust", 10, None); // case insensitive
        let key4 = tool.cache_key("rust", 5, None);

        assert_eq!(key1, key2);
        assert_eq!(key1, key3); // case insensitive
        assert_ne!(key1, key4); // different count
    }

    #[test]
    fn test_extract_domain() {
        assert_eq!(
            extract_domain("https://example.com/path"),
            Some("example.com".to_string())
        );
        assert_eq!(
            extract_domain("https://www.example.com/path"),
            Some("www.example.com".to_string())
        );
        assert_eq!(extract_domain("not-a-url"), None);
    }
}
