//! HTTP Tool
//!
//! Make HTTP requests (GET, POST, PUT, DELETE, etc.)

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tracing::{debug, info};

/// Execute HTTP request
pub async fn execute(args: Value) -> Result<String, String> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;

    let method = args
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();

    let headers = args
        .get("headers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    info!("HTTP {} {}", method, url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build client: {}", e))?;

    // Build headers
    let mut header_map = HeaderMap::new();
    for (key, value) in headers {
        let header_name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|e| format!("Invalid header name '{}': {}", key, e))?;
        let header_value = HeaderValue::from_str(value.as_str().unwrap_or(""))
            .map_err(|e| format!("Invalid header value for '{}': {}", key, e))?;
        header_map.insert(header_name, header_value);
    }

    // Add default user-agent if not provided
    if !header_map.contains_key("user-agent") {
        header_map.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static("Mozilla/5.0 (compatible; Pekobot-HTTP/1.0)"),
        );
    }

    // Build request
    let request_builder = match method.as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "PATCH" => client.patch(url),
        "HEAD" => client.head(url),
        "OPTIONS" => client.request(reqwest::Method::OPTIONS, url),
        _ => return Err(format!("Unsupported HTTP method: {}", method)),
    };

    let request_builder = request_builder.headers(header_map);
    let request_builder = if let Some(body_str) = body {
        request_builder.body(body_str)
    } else {
        request_builder
    };

    // Send request
    let response = request_builder
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    let status = response.status();
    let headers = response.headers().clone();
    let final_url = response.url().to_string();

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    debug!("Response: {} {} bytes", status, body.len());

    // Format output
    let mut output = String::new();
    output.push_str(&format!("Status: {}\n", status));
    output.push_str(&format!("URL: {}\n", final_url));
    output.push_str("Headers:\n");

    for (key, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            output.push_str(&format!("  {}: {}\n", key, v));
        }
    }

    output.push_str("\n");
    
    // Try to pretty-print JSON body
    if let Ok(json) = serde_json::from_str::<Value>(&body) {
        if let Ok(pretty) = serde_json::to_string_pretty(&json) {
            output.push_str(&pretty);
        } else {
            output.push_str(&body);
        }
    } else {
        output.push_str(&body);
    }

    Ok(output)
}
