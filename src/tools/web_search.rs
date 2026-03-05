//! Web search tool
//!
//! Provides web search capabilities using Brave Search or `DuckDuckGo`.
//! Results are cached for 15 minutes to reduce API calls.

use async_trait::async_trait;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, warn};

use crate::tools::traits::Tool;

/// Search provider configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SearchProvider {
    /// Brave Search API (requires API key)
    Brave,
    /// `DuckDuckGo` (no API key needed, uses HTML scraping)
    #[default]
    DuckDuckGo,
}

/// Web search tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Enable the tool
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Search provider
    #[serde(default)]
    pub provider: SearchProvider,
    /// API key (for Brave)
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
            provider: SearchProvider::default(),
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
    /// Search query
    pub query: String,
    /// Total results found (approximate)
    pub total_results: Option<u32>,
    /// List of results
    pub results: Vec<SearchResult>,
    /// Provider used
    pub provider: String,
}

/// Simple in-memory cache entry
#[derive(Debug, Clone)]
struct CacheEntry {
    response: SearchResponse,
    timestamp: std::time::Instant,
}

/// Web search tool
pub struct WebSearchTool {
    config: WebSearchConfig,
    client: Client,
    cache: std::sync::Mutex<HashMap<String, CacheEntry>>,
}

impl WebSearchTool {
    /// Create a new web search tool
    #[must_use]
    pub fn new(config: WebSearchConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Pekobot/0.1 (WebSearchTool)")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Generate cache key from query + count + freshness
    fn cache_key(&self, query: &str, count: u32, freshness: Option<&str>) -> String {
        format!(
            "{}:{}:{}",
            query.to_lowercase(),
            count,
            freshness.unwrap_or("none")
        )
    }

    /// Check cache for existing results
    fn check_cache(&self, key: &str) -> Option<SearchResponse> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;

        let age = entry.timestamp.elapsed().as_secs();
        if age < self.config.cache_ttl_seconds {
            debug!("Cache hit for key: {}", key);
            Some(entry.response.clone())
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
            cache.retain(|_, entry| now.duration_since(entry.timestamp) < ttl);

            cache.insert(
                key,
                CacheEntry {
                    response,
                    timestamp: std::time::Instant::now(),
                },
            );
        }
    }

    /// Perform search with Brave API
    async fn search_brave(&self, args: &SearchArgs) -> Result<SearchResponse, String> {
        let api_key = self
            .config
            .api_key
            .as_ref()
            .ok_or("Brave API key not configured")?;

        let count = args.count.unwrap_or(self.config.max_results).min(20).max(1);

        let mut url = Url::parse("https://api.search.brave.com/res/v1/web/search")
            .map_err(|e| format!("Invalid URL: {e}"))?;

        url.query_pairs_mut()
            .append_pair("q", &args.query)
            .append_pair("count", &count.to_string());

        if let Some(freshness) = &args.freshness {
            url.query_pairs_mut().append_pair("freshness", freshness);
        }

        debug!("Brave search URL: {}", url);

        let response = self
            .client
            .get(url)
            .header("X-Subscription-Token", api_key)
            .send()
            .await
            .map_err(|e| format!("Request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!("Brave API error {status}: {text}"));
        }

        let brave_response: BraveResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {e}"))?;

        let results: Vec<SearchResult> = brave_response
            .web
            .results
            .into_iter()
            .map(|r| {
                let url = r.url; // Extract first to avoid move issues
                SearchResult {
                    title: r.title,
                    source: extract_domain(&url),
                    url,
                    snippet: r.description,
                    published: r.age,
                }
            })
            .collect();

        Ok(SearchResponse {
            query: args.query.clone(),
            total_results: brave_response.web.total_count,
            results,
            provider: "brave".to_string(),
        })
    }

    /// Perform search with `DuckDuckGo` (HTML scraping)
    async fn search_ddg(&self, args: &SearchArgs) -> Result<SearchResponse, String> {
        let count = args.count.unwrap_or(self.config.max_results).min(20).max(1);

        // DuckDuckGo HTML interface
        let mut url = Url::parse("https://html.duckduckgo.com/html/")
            .map_err(|e| format!("Invalid URL: {e}"))?;

        url.query_pairs_mut().append_pair("q", &args.query);

        // DDG doesn't support freshness filter in HTML interface
        // We could use the JSON API but it has stricter rate limits

        debug!("DDG search URL: {}", url);

        let response = self
            .client
            .get(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.0",
            )
            .send()
            .await
            .map_err(|e| format!("Request failed: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("DDG returned status: {}", response.status()));
        }

        let html = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {e}"))?;

        let results = parse_ddg_results(&html, count as usize)?;

        Ok(SearchResponse {
            query: args.query.clone(),
            total_results: None, // DDG doesn't provide this
            results,
            provider: "duckduckgo".to_string(),
        })
    }
}

/// Extract domain from URL
fn extract_domain(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
}

/// Brave API response structure
#[derive(Debug, Deserialize)]
struct BraveResponse {
    #[serde(rename = "web")]
    web: BraveWebResults,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    #[serde(rename = "total_count")]
    total_count: Option<u32>,
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: String,
    #[serde(rename = "age")]
    age: Option<String>,
}

/// Parse `DuckDuckGo` HTML results
/// This is fragile - DDG may change their HTML structure
fn parse_ddg_results(html: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);

    // Try multiple selectors as DDG changes their HTML frequently
    let selectors = [
        "article[data-testid='result']",        // 2025 DDG layout
        "div[data-result]",                     // Alternative layout
        ".result[data-testid]",                 // Another variant
        "article.result",                       // Older design
        "div.result",                           // Classic DDG
        ".web-result",                          // Legacy
    ];

    let mut results = Vec::new();

    for selector_str in &selectors {
        let selector = match Selector::parse(selector_str) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for element in document.select(&selector).take(limit) {
            // Try multiple title selectors
            let title = element
                .select(&Selector::parse("h2 a, h3 a, [data-testid='result-title-a'], a[href]").unwrap())
                .next()
                .and_then(|e| {
                    let text = e.text().collect::<String>();
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });

            // Try multiple URL selectors
            let url = element
                .select(&Selector::parse("a[href]").unwrap())
                .next()
                .and_then(|e| e.value().attr("href"))
                .map(std::string::ToString::to_string);

            // Try multiple snippet selectors
            let snippet = element
                .select(&Selector::parse(
                    "[data-testid='result-snippet'], .result__snippet, .result__body, p"
                ).unwrap())
                .next()
                .and_then(|e| {
                    let text = e.text().collect::<String>();
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });

            if let (Some(title), Some(url)) = (title, url) {
                // DDG uses redirects, extract actual URL
                let actual_url = if url.starts_with("//duckduckgo.com/l/") || url.starts_with("/l/") {
                    // Extract from redirect URL
                    extract_ddg_redirect(&url).unwrap_or(url)
                } else {
                    url
                };

                // Skip if it's a DDG internal link
                if actual_url.contains("duckduckgo.com") || actual_url.starts_with("/") {
                    continue;
                }

                results.push(SearchResult {
                    title,
                    url: actual_url.clone(),
                    snippet: snippet.unwrap_or_default(),
                    source: extract_domain(&actual_url),
                    published: None,
                });
            }
        }

        if !results.is_empty() {
            debug!("Found {} DDG results using selector: {}", results.len(), selector_str);
            break; // Found results with this selector
        }
    }

    if results.is_empty() {
        // Fallback: try very simple parsing with regex
        warn!("Could not parse DDG results with standard selectors, trying fallback");
        results = parse_ddg_fallback(html, limit);
    }

    Ok(results)
}

/// Fallback parser using regex when CSS selectors fail
fn parse_ddg_fallback(html: &str, limit: usize) -> Vec<SearchResult> {
    use regex::Regex;
    let mut results = Vec::new();
    
    // Try to find result links with their text
    let link_regex = match Regex::new(r#"<a[^>]+href="([^"]*duckduckgo\.com/l/[^"]*)"[^>]*>(.*?)</a>"#) {
        Ok(re) => re,
        Err(_) => return results,
    };
    
    for cap in link_regex.captures_iter(html).take(limit) {
        let href = &cap[1];
        let title_html = &cap[2];
        
        // Extract title (strip HTML tags)
        let title = title_html.replace(|c: char| c == '<' || c == '>', " ").trim().to_string();
        
        if !title.is_empty() {
            let actual_url = extract_ddg_redirect(href).unwrap_or_else(|| href.to_string());
            if !actual_url.contains("duckduckgo.com") && !actual_url.starts_with("/") {
                results.push(SearchResult {
                    title,
                    url: actual_url.clone(),
                    snippet: String::new(),
                    source: extract_domain(&actual_url),
                    published: None,
                });
            }
        }
    }
    
    results
}

/// Extract actual URL from DDG redirect
fn extract_ddg_redirect(redirect_url: &str) -> Option<String> {
    // DDG redirect URLs look like: //duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com
    if let Some(pos) = redirect_url.find("uddg=") {
        let encoded = &redirect_url[pos + 5..];
        return urlencoding::decode(encoded).ok().map(|s| s.to_string());
    }
    None
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web using Brave Search or DuckDuckGo"
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

        // Perform search
        let result = match self.config.provider {
            SearchProvider::Brave => self.search_brave(&args).await,
            SearchProvider::DuckDuckGo => self.search_ddg(&args).await,
        };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_generation() {
        let config = WebSearchConfig::default();
        let tool = WebSearchTool::new(config);

        let key1 = tool.cache_key("hello world", 10, None);
        assert_eq!(key1, "hello world:10:none");

        let key2 = tool.cache_key("Hello World", 10, Some("pd"));
        assert_eq!(key2, "hello world:10:pd");

        // Different queries = different keys
        assert_ne!(
            tool.cache_key("hello", 10, None),
            tool.cache_key("world", 10, None)
        );
    }

    #[test]
    fn test_extract_domain() {
        assert_eq!(
            extract_domain("https://example.com/path"),
            Some("example.com".to_string())
        );
        assert_eq!(
            extract_domain("https://sub.example.co.uk/path"),
            Some("sub.example.co.uk".to_string())
        );
        assert_eq!(extract_domain("not-a-url"), None);
    }

    #[tokio::test]
    async fn test_ddg_fallback_when_no_brave_key() {
        let config = WebSearchConfig {
            enabled: true,
            provider: SearchProvider::Brave,
            api_key: None,
            max_results: 5,
            cache_ttl_seconds: 900,
        };

        let tool = WebSearchTool::new(config);
        let args = SearchArgs {
            query: "test".to_string(),
            count: Some(5),
            freshness: None,
        };

        // Should fail because no API key
        let result = tool.search_brave(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("API key not configured"));
    }
}
