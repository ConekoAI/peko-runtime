//! Fetch tool
//!
//! Lightweight HTTP fetcher that retrieves web pages and converts HTML to markdown.
//! Simpler and faster than the full browser tool for read-only operations.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

use crate::tools::traits::Tool;

/// Specific user-agent to use for fetch requests
const FETCH_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";

/// Fetch tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchConfig {
    /// Enable the tool
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum content size in characters
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// Cache TTL in seconds
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_seconds: u64,
    /// Request timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_chars: 50000,
            cache_ttl_seconds: 900, // 15 minutes
            timeout_seconds: 30,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_max_chars() -> usize {
    50000
}

fn default_cache_ttl() -> u64 {
    900
}

fn default_timeout() -> u64 {
    30
}

/// Extraction mode
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExtractMode {
    /// Convert HTML to markdown
    #[default]
    Markdown,
    /// Plain text extraction
    Text,
}

/// Fetch arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchArgs {
    /// URL to fetch
    pub url: String,
    /// Extraction mode
    #[serde(default)]
    pub extract_mode: ExtractMode,
    /// Maximum characters to return
    #[serde(default)]
    pub max_chars: Option<usize>,
}

/// Fetch result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    /// Original URL
    pub url: String,
    /// Final URL after redirects
    pub final_url: String,
    /// Page title (if available)
    pub title: Option<String>,
    /// Extracted content
    pub content: String,
    /// Content type
    pub content_type: String,
    /// HTTP status code
    pub status_code: u16,
    /// Whether content was truncated
    pub truncated: bool,
}

/// Simple in-memory cache entry
#[derive(Debug, Clone)]
struct CacheEntry {
    response: FetchResult,
    timestamp: std::time::Instant,
}

/// Fetch tool
pub struct FetchTool {
    config: FetchConfig,
    client: Client,
    cache: std::sync::Mutex<std::collections::HashMap<String, CacheEntry>>,
}

impl FetchTool {
    /// Create a new fetch tool
    #[must_use]
    pub fn new(config: FetchConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .user_agent(FETCH_USER_AGENT)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Generate cache key
    fn cache_key(&self, url: &str, mode: &ExtractMode) -> String {
        format!("{}:{:?}", url.to_lowercase(), mode)
    }

    /// Check cache for existing results
    fn check_cache(&self, key: &str) -> Option<FetchResult> {
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
    fn store_cache(&self, key: String, response: FetchResult) {
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

    /// Extract title from HTML
    fn extract_title(html: &str) -> Option<String> {
        if let Some(start) = html.to_lowercase().find("<title>") {
            if let Some(end) = html[start..].to_lowercase().find("</title>") {
                let title = &html[start + 7..start + end];
                return Some(title.trim().to_string());
            }
        }
        if let Some(start) = html.find("<h1") {
            if let Some(close) = html[start..].find('>') {
                if let Some(end) = html[start + close..].find("</h1>") {
                    let h1 = &html[start + close + 1..start + close + end];
                    return Some(Self::extract_text(h1));
                }
            }
        }
        None
    }

    /// Extract text from HTML using readability
    fn extract_text(html: &str) -> String {
        use readability::extractor;

        // Create a dummy URL for extraction
        let url = reqwest::Url::parse("http://example.com").unwrap();

        match extractor::extract(&mut html.as_bytes(), &url) {
            Ok(product) => {
                let mut result = String::new();
                if !product.title.is_empty() {
                    result.push_str(&product.title);
                    result.push('\n');
                    result.push('\n');
                }
                result.push_str(&product.text);
                result
            }
            Err(_) => {
                // Fallback to simple extraction
                Self::simple_extract_text(html)
            }
        }
    }

    /// Simple fallback text extraction
    fn simple_extract_text(html: &str) -> String {
        // Remove script and style tags first
        let mut text = html.to_string();

        // Remove script tags
        while let Some(start) = text.find("<script") {
            if let Some(end) = text[start..].find("</script>") {
                text.replace_range(start..start + end + 9, "");
            } else {
                break;
            }
        }

        // Remove style tags
        while let Some(start) = text.find("<style") {
            if let Some(end) = text[start..].find("</style>") {
                text.replace_range(start..start + end + 8, "");
            } else {
                break;
            }
        }

        // Simple tag removal - replace tags with spaces
        let mut result = String::new();
        let mut in_tag = false;

        for c in text.chars() {
            match c {
                '<' => in_tag = true,
                '>' => {
                    in_tag = false;
                    result.push(' ');
                }
                _ if !in_tag => result.push(c),
                _ => {}
            }
        }

        // Normalize whitespace
        result.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Convert HTML to markdown using readability for text extraction
    fn extract_markdown(html: &str) -> (Option<String>, String) {
        use readability::extractor;

        // Create a dummy URL for extraction
        let url = reqwest::Url::parse("http://example.com").unwrap();

        match extractor::extract(&mut html.as_bytes(), &url) {
            Ok(product) => {
                let title = if product.title.is_empty() {
                    None
                } else {
                    Some(product.title.clone())
                };
                // Convert readability text to simple markdown
                let markdown = Self::text_to_markdown(&product.text);
                (title, markdown)
            }
            Err(_) => {
                // Fallback to simple extraction
                let title = Self::extract_title(html);
                let markdown = Self::html_to_markdown(html);
                (title, markdown)
            }
        }
    }

    /// Convert plain text to simple markdown
    fn text_to_markdown(text: &str) -> String {
        // Just return the text as-is for now
        // In the future, could add formatting detection
        text.to_string()
    }

    /// Convert HTML to markdown (simplified)
    fn html_to_markdown(html: &str) -> String {
        let mut result = String::new();
        let mut in_tag = false;
        let mut tag_buffer = String::new();
        let mut skip_depth = 0; // For skipping script/style content

        for c in html.chars() {
            if skip_depth > 0 {
                // Skip content inside script/style
                if c == '<' {
                    tag_buffer.clear();
                    tag_buffer.push(c);
                } else if c == '>' && !tag_buffer.is_empty() {
                    tag_buffer.push(c);
                    let tag_lower = tag_buffer.to_lowercase();
                    if tag_lower.contains("</script") || tag_lower.contains("</style") {
                        skip_depth -= 1;
                    }
                    tag_buffer.clear();
                } else if !tag_buffer.is_empty() {
                    tag_buffer.push(c);
                }
                continue;
            }

            match c {
                '<' => {
                    if !tag_buffer.is_empty() {
                        result.push_str(&tag_buffer);
                    }
                    tag_buffer.clear();
                    tag_buffer.push(c);
                    in_tag = true;
                }
                '>' => {
                    tag_buffer.push(c);
                    let tag = tag_buffer.clone();
                    let tag_lower = tag.to_lowercase();

                    // Check for script/style start
                    if tag_lower.contains("<script") || tag_lower.contains("<style") {
                        skip_depth += 1;
                    }

                    // Convert common tags to markdown
                    if tag_lower.starts_with("<h1") {
                        result.push('\n');
                        result.push_str("# ");
                    } else if tag_lower.starts_with("<h2") {
                        result.push('\n');
                        result.push_str("## ");
                    } else if tag_lower.starts_with("<h3") {
                        result.push('\n');
                        result.push_str("### ");
                    } else if tag_lower.starts_with("<h4") {
                        result.push('\n');
                        result.push_str("#### ");
                    } else if tag_lower.starts_with("<p") || tag_lower.starts_with("<div") {
                        result.push('\n');
                    } else if tag_lower.starts_with("<br") || tag_lower.starts_with("</br") {
                        result.push('\n');
                    } else if tag_lower.starts_with("<a ") {
                        // Extract href for markdown link
                        if let Some(href_start) = tag_lower.find("href=") {
                            let href_start = href_start + 5;
                            let rest = &tag[href_start..];
                            let quote = rest.chars().next().unwrap_or('"');
                            if quote == '"' || quote == '\'' {
                                if let Some(href_end) = rest[1..].find(quote) {
                                    let _href = &rest[1..=href_end];
                                    result.push('[');
                                    // Link text will be added after closing </a>
                                }
                            }
                        }
                    } else if tag_lower.starts_with("</a>") {
                        result.push_str("](link)"); // Simplified
                    } else if tag_lower.starts_with("<strong") || tag_lower.starts_with("<b") {
                        result.push_str("**");
                    } else if tag_lower.starts_with("</strong") || tag_lower.starts_with("</b") {
                        result.push_str("**");
                    } else if tag_lower.starts_with("<em") || tag_lower.starts_with("<i") {
                        result.push('*');
                    } else if tag_lower.starts_with("</em") || tag_lower.starts_with("</i") {
                        result.push('*');
                    } else if tag_lower.starts_with("<ul") {
                        result.push('\n');
                    } else if tag_lower.starts_with("<li") {
                        result.push_str("- ");
                    }

                    in_tag = false;
                    tag_buffer.clear();
                }
                _ if in_tag => {
                    tag_buffer.push(c);
                }
                _ => {
                    result.push(c);
                }
            }
        }

        // Clean up
        result.replace("\n\n\n", "\n\n").trim().to_string()
    }

    /// Perform the fetch with retry logic
    async fn fetch(&self, args: &FetchArgs) -> anyhow::Result<FetchResult> {
        // Try fetching with retries
        let max_retries = 2;
        let mut last_error = None;

        for attempt in 0..=max_retries {
            match self.fetch_once(args).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let error_str = e.to_string();
                    
                    // Don't retry on certain errors
                    if error_str.contains("Invalid URL")
                        || error_str.contains("timeout") && attempt == max_retries
                    {
                        return Err(e);
                    }
                    
                    last_error = Some(e);
                    if attempt < max_retries {
                        let delay = Duration::from_millis(500 * (attempt + 1) as u64);
                        debug!("Fetch attempt {} failed, retrying in {:?}...", attempt + 1, delay);
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Fetch failed after {} retries", max_retries)))
    }

    /// Single fetch attempt
    async fn fetch_once(&self, args: &FetchArgs) -> anyhow::Result<FetchResult> {
        // Fetch the URL
        let response = self
            .client
            .get(&args.url)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    anyhow::anyhow!("Request timed out - the server took too long to respond")
                } else if e.is_connect() {
                    anyhow::anyhow!("Connection failed - could not connect to the server")
                } else {
                    anyhow::anyhow!("Request failed: {}", e)
                }
            })?;

        let status_code = response.status().as_u16();
        let final_url = response.url().to_string();

        if !response.status().is_success() {
            let status_text = response.status().canonical_reason().unwrap_or("Unknown");
            
            // Provide helpful messages for common errors
            let message = match status_code {
                403 => format!("HTTP 403 Forbidden - access denied (may be blocked by robots.txt or require authentication)"),
                429 => format!("HTTP 429 Too Many Requests - rate limited, please try again later"),
                404 => format!("HTTP 404 Not Found - page does not exist"),
                500..=599 => format!("HTTP {} {} - server error", status_code, status_text),
                _ => format!("HTTP error {}: {}", status_code, status_text),
            };
            
            return Err(anyhow::anyhow!(message));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_lowercase();

        let body = response
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read response body: {}", e))?;

        // Extract content based on content type
        let (title, content) = if content_type.contains("text/plain") || content_type.contains("text/markdown") {
            // Return as-is for plain text and markdown
            (None, body)
        } else if content_type.contains("text/html") {
            // Extract from HTML
            match args.extract_mode {
                ExtractMode::Markdown => Self::extract_markdown(&body),
                ExtractMode::Text => (None, Self::extract_text(&body)),
            }
        } else {
            // For other types, return as-is
            (None, body)
        };

        // Truncate if needed
        let max_chars = args.max_chars.unwrap_or(self.config.max_chars);
        let truncated = content.len() > max_chars;
        let content = if truncated {
            content.chars().take(max_chars).collect()
        } else {
            content
        };

        Ok(FetchResult {
            url: args.url.clone(),
            final_url,
            title,
            content,
            content_type,
            status_code,
            truncated,
        })
    }
}

#[async_trait]
impl Tool for FetchTool {
    fn name(&self) -> &'static str {
        "fetch"
    }

    fn description(&self) -> &'static str {
        "Fetch web pages and extract content as markdown or text"
    }

    fn llm_description(&self) -> String {
        "Fetch web pages and extract content as markdown or text. \
        Use when: you need to access a specific known URL, read documentation, get content from a webpage. \
        Don't use when: you need to search for information (use `web_search` instead), or the page requires browser interaction (use `browser` instead)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "format": {
                    "type": "string",
                    "description": "Output format: 'markdown' or 'text'",
                    "enum": ["markdown", "text"],
                    "default": "markdown"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: FetchArgs =
            serde_json::from_value(args).map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        // Validate URL
        if args.url.is_empty() {
            return Err(anyhow::anyhow!("URL cannot be empty"));
        }

        if !args.url.starts_with("http://") && !args.url.starts_with("https://") {
            return Err(anyhow::anyhow!("URL must start with http:// or https://"));
        }

        let cache_key = self.cache_key(&args.url, &args.extract_mode);

        // Check cache
        if let Some(cached) = self.check_cache(&cache_key) {
            return Ok(serde_json::to_value(cached)?);
        }

        // Perform fetch
        match self.fetch(&args).await {
            Ok(result) => {
                // Store in cache
                self.store_cache(cache_key, result.clone());
                Ok(serde_json::to_value(result)?)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_title() {
        let html = "<html><head><title>Test Page</title></head><body></body></html>";
        assert_eq!(
            FetchTool::extract_title(html),
            Some("Test Page".to_string())
        );
    }

    #[test]
    fn test_extract_title_from_h1() {
        let html = "<html><body><h1>Main Title</h1></body></html>";
        assert_eq!(
            FetchTool::extract_title(html),
            Some("Main Title".to_string())
        );
    }

    #[test]
    fn test_extract_text() {
        let html = "<p>Hello <b>World</b>!</p>";
        let text = FetchTool::extract_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_html_to_markdown_headers() {
        let html = "<h1>Title</h1><h2>Subtitle</h2>";
        let md = FetchTool::html_to_markdown(html);
        assert!(md.contains("# Title"));
        assert!(md.contains("## Subtitle"));
    }

    #[test]
    fn test_cache_key() {
        let config = FetchConfig::default();
        let tool = FetchTool::new(config);

        let key1 = tool.cache_key("https://example.com", &ExtractMode::Markdown);
        let key2 = tool.cache_key("https://example.com", &ExtractMode::Text);
        let key3 = tool.cache_key("https://EXAMPLE.COM", &ExtractMode::Markdown);

        assert_ne!(key1, key2); // Different modes
        assert_eq!(key1, key3); // Case insensitive
    }

    #[test]
    fn test_simple_extract_text_removes_scripts() {
        let html = "<html><script>alert('xss');</script><p>Content</p></html>";
        let text = FetchTool::simple_extract_text(html);
        assert!(!text.contains("alert"));
        assert!(!text.contains("script"));
        assert!(text.contains("Content"));
    }

    #[test]
    fn test_simple_extract_text_removes_styles() {
        let html = "<html><style>.body{color:red}</style><p>Content</p></html>";
        let text = FetchTool::simple_extract_text(html);
        assert!(!text.contains("style"));
        assert!(!text.contains("color"));
        assert!(text.contains("Content"));
    }

    #[test]
    fn test_html_to_markdown_paragraphs() {
        let html = "<p>First paragraph.</p><p>Second paragraph.</p>";
        let md = FetchTool::html_to_markdown(html);
        assert!(md.contains("First paragraph"));
        assert!(md.contains("Second paragraph"));
    }

    #[test]
    fn test_html_to_markdown_bold() {
        let html = "<p>This is <strong>bold</strong> text.</p>";
        let md = FetchTool::html_to_markdown(html);
        assert!(md.contains("**bold**"));
    }

    #[test]
    fn test_html_to_markdown_italic() {
        let html = "<p>This is <em>italic</em> text.</p>";
        let md = FetchTool::html_to_markdown(html);
        assert!(md.contains("*italic*"));
    }

    #[test]
    fn test_html_to_markdown_list() {
        let html = "<ul><li>Item 1</li><li>Item 2</li></ul>";
        let md = FetchTool::html_to_markdown(html);
        assert!(md.contains("- Item 1"));
        assert!(md.contains("- Item 2"));
    }

    #[test]
    fn test_config_default() {
        let config = FetchConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_chars, 50000);
        assert_eq!(config.cache_ttl_seconds, 900);
        assert_eq!(config.timeout_seconds, 30);
    }

    #[test]
    fn test_fetch_result_serialization() {
        let result = FetchResult {
            url: "https://example.com".to_string(),
            final_url: "https://example.com/redirect".to_string(),
            title: Some("Example".to_string()),
            content: "Hello World".to_string(),
            content_type: "text/html".to_string(),
            status_code: 200,
            truncated: false,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("https://example.com"));
        assert!(json.contains("Hello World"));

        let deserialized: FetchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.status_code, 200);
        assert_eq!(deserialized.title, Some("Example".to_string()));
    }

    #[test]
    fn test_fetch_args_parsing() {
        let json = serde_json::json!({
            "url": "https://example.com",
            "extract_mode": "markdown"
        });

        let args: FetchArgs = serde_json::from_value(json).unwrap();
        assert_eq!(args.url, "https://example.com");
        // extract_mode should default to Markdown
    }

    #[tokio::test]
    async fn test_fetch_tool_creation() {
        let config = FetchConfig::default();
        let tool = FetchTool::new(config);
        assert_eq!(tool.name(), "fetch");
    }
}
