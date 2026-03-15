//! Web Search Tool
//!
//! Search the web using DuckDuckGo or Brave Search.

use serde_json::Value;
use tracing::{debug, info};

/// Execute web search
pub async fn execute(args: Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'query' parameter")?;

    let count = args
        .get("count")
        .and_then(|v| v.as_i64())
        .map(|c| c.clamp(1, 20) as usize)
        .unwrap_or(10);

    let engine = args
        .get("engine")
        .and_then(|v| v.as_str())
        .unwrap_or("ddg");

    info!("Web search: '{}' using {}", query, engine);

    match engine {
        "brave" => search_brave(query, count).await,
        _ => search_duckduckgo(query, count).await,
    }
}

/// Search using DuckDuckGo HTML
async fn search_duckduckgo(query: &str, count: usize) -> Result<String, String> {
    let client = reqwest::Client::new();
    let encoded_query = urlencoding::encode(query);
    
    // Try DuckDuckGo Lite first (simpler HTML)
    let url = format!(
        "https://lite.duckduckgo.com/lite/?q={}&kl=us-en",
        encoded_query
    );

    debug!("Searching DuckDuckGo: {}", url);

    let response = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (compatible; Pekobot/1.0)")
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    let html = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // Parse results
    let results = parse_duckduckgo_results(&html, count);

    if results.is_empty() {
        // Try regular DuckDuckGo if lite fails
        let url = format!(
            "https://duckduckgo.com/html/?q={}",
            encoded_query
        );
        
        let response = client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0 (compatible; Pekobot/1.0)")
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let html = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        let results = parse_duckduckgo_results(&html, count);
        
        if results.is_empty() {
            return Ok(format!("No results found for '{}'", query));
        }
        
        return format_results(query, &results);
    }

    format_results(query, &results)
}

/// Parse DuckDuckGo HTML results
fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let document = scraper::Html::parse_document(html);
    let mut results = Vec::new();

    // Try multiple selectors for robustness
    let selectors = [
        "div.result",           // Lite version
        ".web-result",          // Standard version
        ".result",              // Generic
        ".links_main",          // Another variation
    ];

    for selector_str in &selectors {
        let selector = match scraper::Selector::parse(selector_str) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for element in document.select(&selector) {
            if results.len() >= max_results {
                break;
            }

            // Try to extract title and URL
            let (title, url) = extract_title_url(&element);
            
            if title.is_empty() || url.is_empty() {
                continue;
            }

            // Try to extract snippet
            let snippet = extract_snippet(&element);

            results.push(SearchResult {
                title: title.to_string(),
                url: url.to_string(),
                snippet: snippet.to_string(),
            });
        }

        if !results.is_empty() {
            break;
        }
    }

    results
}

/// Extract title and URL from a result element
fn extract_title_url(element: &scraper::ElementRef) -> (String, String) {
    // Try various link selectors
    let link_selectors = ["a.result__a", "a.result__snippet", "a", ".links_main a"];

    for selector_str in &link_selectors {
        let selector = match scraper::Selector::parse(selector_str) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if let Some(link) = element.select(&selector).next() {
            let href = link.value().attr("href").unwrap_or("");
            let title = link.text().collect::<String>().trim().to_string();

            if !title.is_empty() {
                let url = if href.starts_with("http") {
                    href.to_string()
                } else if href.starts_with("//") {
                    format!("https:{}", href)
                } else {
                    href.to_string()
                };

                return (title, url);
            }
        }
    }

    (String::new(), String::new())
}

/// Extract snippet from a result element
fn extract_snippet(element: &scraper::ElementRef) -> String {
    let snippet_selectors = [
        "div.result__snippet",
        ".result__snippet",
        ".result-snippet",
        "div.snippet",
        "td.result-snippet",
    ];

    for selector_str in &snippet_selectors {
        let selector = match scraper::Selector::parse(selector_str) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if let Some(snippet_el) = element.select(&selector).next() {
            let text = snippet_el.text().collect::<String>().trim().to_string();
            if !text.is_empty() {
                return text;
            }
        }
    }

    String::new()
}

/// Search using Brave Search (requires API key)
async fn search_brave(query: &str, count: usize) -> Result<String, String> {
    // Check for API key
    let api_key = std::env::var("BRAVE_API_KEY")
        .map_err(|_| "Brave Search requires BRAVE_API_KEY environment variable")?;

    let client = reqwest::Client::new();
    let offset = 0u32;

    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}&offset={}",
        urlencoding::encode(query),
        count.min(20),
        offset
    );

    debug!("Searching Brave: {}", url);

    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", &api_key)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let text = response
            .text()
            .await
            .unwrap_or_default();
        return Err(format!("Brave API error ({}): {}", status, text));
    }

    let json: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let results = parse_brave_results(&json, count);
    
    if results.is_empty() {
        return Ok(format!("No results found for '{}'", query));
    }

    format_results(query, &results)
}

/// Parse Brave API response
fn parse_brave_results(json: &Value, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    if let Some(web) = json.get("web") {
        if let Some(pages) = web.get("results").and_then(|r| r.as_array()) {
            for page in pages.iter().take(max_results) {
                let title = page
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                
                let url = page
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                
                let snippet = page
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();

                if !title.is_empty() && !url.is_empty() {
                    results.push(SearchResult {
                        title,
                        url,
                        snippet,
                    });
                }
            }
        }
    }

    results
}

/// Format search results
fn format_results(query: &str, results: &[SearchResult]) -> Result<String, String> {
    let mut output = format!("Search results for '{}':\n\n", query);

    for (i, result) in results.iter().enumerate() {
        output.push_str(&format!(
            "{}. {}\n   URL: {}\n   {}\n\n",
            i + 1,
            result.title,
            result.url,
            result.snippet
        ));
    }

    Ok(output)
}

/// Search result struct
#[derive(Debug)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}
