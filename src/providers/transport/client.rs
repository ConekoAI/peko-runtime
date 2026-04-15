//! Shared HTTP client for all providers
//!
//! Handles authentication, retries, timeouts, and request/response formatting.

use super::retry::{RetryExecutor, RetryPolicy};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;
use tracing::debug;

/// Authentication configuration
#[derive(Debug, Clone)]
pub enum AuthConfig {
    Bearer { token: String },
    Header { name: String, value: String },
}

/// Shared HTTP client for provider API calls
pub struct HttpClient {
    inner: Client,
    base_url: String,
    auth: AuthConfig,
    extra_headers: Vec<(String, String)>,
    retry_policy: Option<RetryPolicy>,
}

impl HttpClient {
    /// Create a new HTTP client
    pub fn new(
        base_url: impl Into<String>,
        auth: AuthConfig,
        timeout_secs: u64,
    ) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(60))
            .http1_only() // Force HTTP/1.1 to avoid HTTP/2 issues with some providers
            .build()?;

        let base_url = base_url.into();
        // Remove trailing slash for consistency
        let base_url = base_url.trim_end_matches('/').to_string();

        Ok(Self {
            inner: client,
            base_url,
            auth,
            extra_headers: vec![],
            retry_policy: None,
        })
    }

    /// Create a new HTTP client with extra headers
    pub fn with_headers(
        base_url: impl Into<String>,
        auth: AuthConfig,
        timeout_secs: u64,
        extra_headers: Vec<(String, String)>,
    ) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(60))
            .http1_only()
            .build()?;

        let base_url = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_string();

        Ok(Self {
            inner: client,
            base_url,
            auth,
            extra_headers,
            retry_policy: None,
        })
    }

    /// Set retry policy for this client
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// Build request with authentication headers
    fn build_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}{}", self.base_url, path)
        };
        
        let mut request = self.inner.request(method, &url);

        // Add authentication
        match &self.auth {
            AuthConfig::Bearer { token } => {
                request = request.header("Authorization", format!("Bearer {}", token));
            }
            AuthConfig::Header { name, value } => {
                request = request.header(name, value);
            }
        }

        // Add extra headers
        for (name, value) in &self.extra_headers {
            request = request.header(name, value);
        }

        request
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
    }

    /// Send a POST request with JSON body and parse JSON response
    pub async fn post_json<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> anyhow::Result<R> {
        let body_json = serde_json::to_value(body)?;
        let operation = || async {
            let request = self
                .build_request(reqwest::Method::POST, path)
                .json(&body_json);

            debug!("Sending POST request to {}{}", self.base_url, path);

            let response = request.send().await?;
            let status = response.status();

            if !status.is_success() {
                let error_text = response.text().await.unwrap_or_default();
                debug!("HTTP error {}: {}", status, error_text);
                return Err(anyhow::anyhow!("HTTP error {}: {}", status, error_text));
            }

            let result: R = response.json().await?;
            Ok(result)
        };

        match &self.retry_policy {
            Some(policy) => {
                RetryExecutor::execute(policy, &format!("POST {}", path), operation).await
            }
            None => operation().await,
        }
    }

    /// Send a POST request with JSON body and return streaming response
    pub async fn post_stream(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<Bytes>>> {
        let body_json = serde_json::to_value(body)?;
        let operation = || async {
            let request = self
                .build_request(reqwest::Method::POST, path)
                .json(&body_json)
                .header("Accept", "text/event-stream");

            debug!(
                "Sending streaming POST request to {}{}",
                self.base_url, path
            );

            let response = request.send().await?;
            let status = response.status();

            if !status.is_success() {
                let error_text = response.text().await.unwrap_or_default();
                debug!("HTTP error {}: {}", status, error_text);
                return Err(anyhow::anyhow!("HTTP error {}: {}", status, error_text));
            }

            Ok(response)
        };

        // Retry the initial request if configured
        let response = match &self.retry_policy {
            Some(policy) => {
                RetryExecutor::execute(policy, &format!("POST {}", path), operation).await?
            }
            None => operation().await?,
        };

        // Convert the byte stream to a stream of anyhow::Result<Bytes>
        let stream = response.bytes_stream().map(|result| match result {
            Ok(bytes) => Ok(bytes),
            Err(e) => Err(anyhow::anyhow!("Stream error: {}", e)),
        });

        Ok(stream)
    }

    /// Send a simple GET request
    pub async fn get<R: DeserializeOwned>(&self, path: &str) -> anyhow::Result<R> {
        let operation = || async {
            let request = self.build_request(reqwest::Method::GET, path);

            debug!("Sending GET request to {}{}", self.base_url, path);

            let response = request.send().await?;
            let status = response.status();

            if !status.is_success() {
                let error_text = response.text().await.unwrap_or_default();
                debug!("HTTP error {}: {}", status, error_text);
                return Err(anyhow::anyhow!("HTTP error {}: {}", status, error_text));
            }

            let result: R = response.json().await?;
            Ok(result)
        };

        match &self.retry_policy {
            Some(policy) => {
                RetryExecutor::execute(policy, &format!("GET {}", path), operation).await
            }
            None => operation().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_config_bearer() {
        let auth = AuthConfig::Bearer {
            token: "test_token".to_string(),
        };
        match auth {
            AuthConfig::Bearer { token } => assert_eq!(token, "test_token"),
            _ => panic!("Expected Bearer auth"),
        }
    }

    #[test]
    fn test_client_creation() {
        let auth = AuthConfig::Bearer {
            token: "test".to_string(),
        };
        let client = HttpClient::new("https://api.example.com", auth, 30);
        assert!(client.is_ok());
    }
}
