//! Fetch Tool
//!
//! Fetch a URL and extract its content using readability.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tracing::{debug, info};

/// Execute fetch
pub async fn execute(args: Value) -> Result<String, String> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;

    let extract_text = args
        .get("extract_text")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    info!("Fetching: {}", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build client: {}", e))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("user-agent"),
        HeaderValue::from_static("Mozilla/5.0 (compatible; Pekobot-Fetch/1.0)"),
    );

    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP error: {}", status));
    }

    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_string();

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read body: {}", e))?;

    debug!("Fetched {} bytes, content-type: {}", body.len(), content_type);

    // Handle based on content type
    if content_type.starts_with("text/html") && extract_text {
        extract_article(&body, &final_url)
    } else if content_type.starts_with("application/json") {
        format_json(&body)
    } else {
        // Plain text or other
        Ok(body)
    }
}

/// Extract article content using readability
fn extract_article(html: &str, url: &str) -> Result<String, String> {
    use readability::extractor;

    let mut cursor = std::io::Cursor::new(html.as_bytes());
    
    let parsed_url = reqwest::Url::parse(url)
        .map_err(|e| format!("Invalid URL '{}': {}", url, e))?;
    
    let product = extractor::extract(&mut cursor, &parsed_url)
        .map_err(|e| format!("Failed to extract article: {}", e))?;

    let mut output = String::new();
    output.push_str(&format!("Title: {}\n", product.title));
    output.push_str(&format!("\n{}\n", product.text));

    Ok(output)
}

/// Format JSON content
fn format_json(body: &str) -> Result<String, String> {
    let json: Value = serde_json::from_str(body)
        .map_err(|e| format!("Invalid JSON: {}", e))?;

    let formatted = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to format JSON: {}", e))?;

    Ok(formatted)
}
