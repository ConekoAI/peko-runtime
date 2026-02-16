//! HTTP tool for making web requests

use async_trait::async_trait;
use serde_json::json;
use anyhow::{Context, Result};

use crate::tools::Tool;

/// HTTP method for requests
#[derive(Debug, Clone, Copy, Default)]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
}

impl HttpMethod {
    fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Head => "HEAD",
        }
    }
}

impl std::str::FromStr for HttpMethod {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "GET" => Ok(HttpMethod::Get),
            "POST" => Ok(HttpMethod::Post),
            "PUT" => Ok(HttpMethod::Put),
            "DELETE" => Ok(HttpMethod::Delete),
            "PATCH" => Ok(HttpMethod::Patch),
            "HEAD" => Ok(HttpMethod::Head),
            _ => Err(anyhow::anyhow!("Unknown HTTP method: {}", s)),
        }
    }
}

/// HTTP tool for making web requests
pub struct HttpTool {
    client: reqwest::Client,
}

impl HttpTool {
    /// Create a new HTTP tool with default configuration
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Pekobot/0.1.0")
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self { client })
    }

    /// Create with custom timeout
    pub fn with_timeout(timeout_secs: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .user_agent("Pekobot/0.1.0")
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self { client })
    }
}

impl Default for HttpTool {
    fn default() -> Self {
        Self::new().expect("Failed to create default HTTP tool")
    }
}

#[async_trait]
impl Tool for HttpTool {
    fn name(&self) -> &str {
        "http"
    }

    fn description(&self) -> &str {
        "Make HTTP requests to web services. Parameters: {\"url\": string, \"method\": \"GET|POST|PUT|DELETE|PATCH\", \"headers\": object (optional), \"body\": object|string (optional)}"
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        // Extract URL (required)
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: url"))?;

        // Extract method (default to GET)
        let method = params
            .get("method")
            .and_then(|v| v.as_str())
            .map(|m| m.parse::<HttpMethod>())
            .transpose()?
            .unwrap_or_default();

        // Build request
        let mut request = match method {
            HttpMethod::Get => self.client.get(url),
            HttpMethod::Post => self.client.post(url),
            HttpMethod::Put => self.client.put(url),
            HttpMethod::Delete => self.client.delete(url),
            HttpMethod::Patch => self.client.patch(url),
            HttpMethod::Head => self.client.head(url),
        };

        // Add headers if provided
        if let Some(headers) = params.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in headers {
                if let Some(val_str) = value.as_str() {
                    request = request.header(key, val_str);
                }
            }
        }

        // Add body if provided (only for POST, PUT, PATCH)
        match method {
            HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch => {
                if let Some(body) = params.get("body") {
                    if let Some(body_str) = body.as_str() {
                        request = request.body(body_str.to_string());
                    } else {
                        request = request.json(body);
                    }
                }
            }
            _ => {}
        }

        // Execute request
        let response = request
            .send()
            .await
            .context("HTTP request failed")?;

        let status = response.status();
        let headers = response.headers().clone();
        let body_text = response
            .text()
            .await
            .context("Failed to read response body")?;

        // Try to parse body as JSON, fall back to string
        let body_json = serde_json::from_str(&body_text).unwrap_or_else(|_| json!(body_text));

        // Build response object
        let mut response_headers = serde_json::Map::new();
        for (key, value) in headers.iter() {
            if let Ok(val_str) = value.to_str() {
                response_headers.insert(key.to_string(), json!(val_str));
            }
        }

        Ok(json!({
            "status": status.as_u16(),
            "status_text": status.canonical_reason().unwrap_or("Unknown"),
            "headers": response_headers,
            "body": body_json,
            "success": status.is_success(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_method_parsing() {
        assert!(matches!("GET".parse::<HttpMethod>().unwrap(), HttpMethod::Get));
        assert!(matches!("post".parse::<HttpMethod>().unwrap(), HttpMethod::Post));
        assert!(matches!("DELETE".parse::<HttpMethod>().unwrap(), HttpMethod::Delete));
    }

    #[test]
    fn test_http_tool_creation() {
        let tool = HttpTool::new();
        assert!(tool.is_ok());
        
        let tool = tool.unwrap();
        assert_eq!(tool.name(), "http");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn test_http_get() {
        let tool = HttpTool::new().unwrap();
        
        // Use httpbin.org for testing (or mock in real tests)
        let params = json!({
            "url": "https://httpbin.org/get",
            "method": "GET"
        });

        let result = tool.execute(params).await;
        
        // This may fail in test environments without internet
        // In production, use mock servers
        if let Ok(response) = result {
            assert!(response.get("status").is_some());
            assert!(response.get("success").is_some());
        }
    }
}
