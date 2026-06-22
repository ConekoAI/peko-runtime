//! Universal Tool Transport - Stdio communication
//!
//! SRP: This module ONLY handles message transport over stdio.
//! No protocol parsing, no execution logic.
//!
//! This implementation now uses the shared `ProcessTransport` to avoid
//! duplication with MCP transport.

use super::protocol::{Request, Response};
use anyhow::Result;
use tokio::time::{timeout, Duration};

/// Default timeout for tool requests
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Transport handle for communicating with a tool process
///
/// This is a thin wrapper around the shared `ProcessTransport` that
/// adds protocol-specific request/response handling.
pub struct Transport {
    inner: crate::extensions::framework::protocols::shared::ProcessTransport,
    request_timeout: Duration,
}

impl Transport {
    /// Spawn a tool and create transport
    ///
    /// Automatically detects script files (.py, .js) and uses appropriate interpreter.
    /// Uses the shared `ProcessTransport` for unified process management.
    pub async fn spawn(executable: impl AsRef<std::path::Path>) -> Result<Self> {
        let inner =
            crate::extensions::framework::protocols::shared::ProcessTransport::spawn_default(executable)
                .await?;

        Ok(Self {
            inner,
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
        })
    }

    /// Set the request timeout
    #[must_use]
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.request_timeout = Duration::from_secs(secs);
        self
    }

    /// Gracefully shut down the transport and kill the process if needed
    pub async fn shutdown(self) -> Result<()> {
        self.inner.shutdown().await
    }

    /// Send a request and wait for response
    pub async fn request(&mut self, req: &Request, timeout_secs: u64) -> Result<Response> {
        let req_json = serde_json::to_string(req)?;

        // Send request
        self.inner.send_line(&req_json).await?;

        // Read response with timeout
        let response_json = timeout(Duration::from_secs(timeout_secs), self.inner.read_line())
            .await
            .map_err(|_| anyhow::anyhow!("Tool request timed out"))??;

        // Parse response
        let response: Response = serde_json::from_str(&response_json)
            .map_err(|e| anyhow::anyhow!("Invalid JSON response: {response_json} (error: {e})"))?;

        // Verify id matches
        if response.id != req.id {
            return Err(anyhow::anyhow!(
                "Response ID mismatch: expected {:?}, got {:?}",
                req.id,
                response.id
            ));
        }

        Ok(response)
    }

    /// Send a notification (fire and forget)
    pub async fn notify(&mut self, req: &Request) -> Result<()> {
        let req_json = serde_json::to_string(req)?;
        self.inner.send_line(&req_json).await
    }
}

/// Simple transport for testing
#[cfg(test)]
pub struct MockTransport {
    responses: Vec<Response>,
    current: usize,
}

#[cfg(test)]
impl MockTransport {
    #[must_use]
    pub fn new(responses: Vec<Response>) -> Self {
        Self {
            responses,
            current: 0,
        }
    }

    pub async fn request(&mut self, _req: &Request) -> Result<Response> {
        if self.current >= self.responses.len() {
            return Err(anyhow::anyhow!("No more mock responses"));
        }
        let resp = self.responses[self.current].clone();
        self.current += 1;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{ErrorObject, ResponseResult};
    use super::*;

    #[tokio::test]
    async fn test_mock_transport() {
        let responses = vec![Response {
            jsonrpc: "2.0".to_string(),
            id: Some("1".to_string()),
            result: ResponseResult::Result(serde_json::json!({"success": true})),
        }];

        let mut transport = MockTransport::new(responses);
        let req = Request::new("test", serde_json::json!({}));
        let resp = transport.request(&req).await.unwrap();

        match resp.result {
            ResponseResult::Result(v) => assert_eq!(v["success"], true),
            _ => panic!("Expected success"),
        }
    }

    #[test]
    fn test_error_object() {
        let err = ErrorObject::internal_error("something broke");
        assert_eq!(err.code, -32603);
        assert!(err.message.contains("broke"));
    }
}
