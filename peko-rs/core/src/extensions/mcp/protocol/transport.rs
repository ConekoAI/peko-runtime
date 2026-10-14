//! MCP transport layer
//!
//! Provides transport abstractions for MCP communication:
//! - `SseTransport`: HTTP+SSE remote communication
//! - `InMemoryTransport`: paired in-process endpoints for tests

use crate::common::vault::Vault;
use crate::extensions::mcp::protocol::{config::McpAuthConfig, types::JsonRpcMessage};
use async_trait::async_trait;
use secrecy::ExposeSecret;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, trace, warn};

// For SSE transport
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::Client;
use url::Url;

/// Errors that can occur during transport operations
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Process error: {0}")]
    Process(String),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Timeout")]
    Timeout,

    #[error("Invalid message: {0}")]
    InvalidMessage(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),

    #[error("SSE error: {0}")]
    Sse(String),
}

/// Result type for transport operations
pub type Result<T> = std::result::Result<T, TransportError>;

/// MCP transport trait
///
/// Defines the interface for all MCP transport implementations.
/// Transports are responsible for sending and receiving JSON-RPC messages.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC message
    ///
    /// # Arguments
    /// * `message` - The JSON-RPC message to send
    ///
    /// # Returns
    /// * `Ok(())` if the message was sent successfully
    /// * `Err(TransportError)` if sending failed
    async fn send(&self, message: JsonRpcMessage) -> Result<()>;

    /// Receive a JSON-RPC message
    ///
    /// # Arguments
    /// * `timeout` - Maximum time to wait for a message
    ///
    /// # Returns
    /// * `Ok(Some(JsonRpcMessage))` if a message was received
    /// * `Ok(None)` if the timeout expired
    /// * `Err(TransportError)` if receiving failed
    async fn recv(&self, timeout: Duration) -> Result<Option<JsonRpcMessage>>;

    /// Close the transport connection
    ///
    /// # Returns
    /// * `Ok(())` if the connection was closed successfully
    async fn close(&self) -> Result<()>;

    /// Check if the transport is healthy
    ///
    /// # Returns
    /// * `true` if the transport is healthy and can send/receive messages
    /// * `false` if the transport is in an error state
    fn is_healthy(&self) -> bool;
}

// =============================================================================
// In-Memory Transport (for testing)
// =============================================================================

/// In-memory transport for testing
///
/// This transport connects two endpoints directly without using actual I/O.
/// Useful for unit testing without spawning subprocesses.
pub struct InMemoryTransport {
    /// Sender for outgoing messages
    sender: tokio::sync::mpsc::Sender<JsonRpcMessage>,
    /// Receiver for incoming messages
    receiver: Arc<Mutex<tokio::sync::mpsc::Receiver<JsonRpcMessage>>>,
    /// Whether the transport is healthy
    healthy: Arc<std::sync::atomic::AtomicBool>,
}

impl InMemoryTransport {
    /// Create a pair of connected in-memory transports
    ///
    /// Returns two transports that are connected to each other.
    /// Messages sent on one will be received on the other.
    #[must_use]
    pub fn pair() -> (Self, Self) {
        let (tx1, rx1) = tokio::sync::mpsc::channel(100);
        let (tx2, rx2) = tokio::sync::mpsc::channel(100);

        let transport1 = Self {
            sender: tx1,
            receiver: Arc::new(Mutex::new(rx2)),
            healthy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        let transport2 = Self {
            sender: tx2,
            receiver: Arc::new(Mutex::new(rx1)),
            healthy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        (transport1, transport2)
    }
}

#[async_trait]
impl McpTransport for InMemoryTransport {
    async fn send(&self, message: JsonRpcMessage) -> Result<()> {
        if !self.is_healthy() {
            return Err(TransportError::ConnectionClosed);
        }

        self.sender
            .send(message)
            .await
            .map_err(|_| TransportError::ConnectionClosed)
    }

    async fn recv(&self, timeout: Duration) -> Result<Option<JsonRpcMessage>> {
        if !self.is_healthy() {
            return Err(TransportError::ConnectionClosed);
        }

        match tokio::time::timeout(timeout, self.receiver.lock().await.recv()).await {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => {
                // Channel closed
                self.healthy
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                Err(TransportError::ConnectionClosed)
            }
            Err(_) => Ok(None), // Timeout
        }
    }

    async fn close(&self) -> Result<()> {
        self.healthy
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// =============================================================================
// SSE Transport
// =============================================================================

/// SSE-based transport for remote MCP servers
///
/// Uses HTTP POST for client-to-server messages and SSE for server-to-client
/// messages, as specified in the MCP protocol.
pub struct SseTransport {
    /// HTTP client
    client: Client,
    /// Server endpoint URL
    endpoint: Url,
    message_endpoint: Arc<Mutex<Option<Url>>>,
    /// Session ID for stateful connections
    session_id: Mutex<Option<String>>,
    /// Message receiver channel
    receiver: Arc<Mutex<tokio::sync::mpsc::Receiver<JsonRpcMessage>>>,
    /// Sender for the receiver channel (kept to detect close)
    #[allow(dead_code)]
    sender: Arc<Mutex<tokio::sync::mpsc::Sender<JsonRpcMessage>>>,
    /// Whether the transport is healthy
    healthy: Arc<std::sync::atomic::AtomicBool>,
    /// SSE stream task handle
    receive_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Optional authentication configuration.
    auth: Option<McpAuthConfig>,
    /// Optional vault for reading/writing OAuth tokens.
    vault: Option<Arc<Vault>>,
    /// Server name used as the vault key for OAuth tokens.
    server_name: Option<String>,
}

impl SseTransport {
    /// Classic HTTP/SSE announces a separate POST endpoint on its GET stream.
    pub(crate) async fn require_message_endpoint(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), async {
            while self.message_endpoint.lock().await.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| TransportError::Sse("SSE server did not announce a message endpoint".into()))
    }
    /// Create a new SSE transport and establish connection
    ///
    /// # Arguments
    /// * `endpoint` - The MCP server endpoint URL
    ///
    /// # Returns
    /// A new `SseTransport` connected to the server
    pub async fn connect(endpoint: impl AsRef<str>) -> Result<Self> {
        let endpoint = Url::parse(endpoint.as_ref())?;
        debug!("Connecting to MCP server at: {}", endpoint);

        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()?;

        // Create channel for receiving messages from SSE stream
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let transport = Self {
            client: client.clone(),
            endpoint: endpoint.clone(),
            message_endpoint: Arc::new(Mutex::new(None)),
            session_id: Mutex::new(None),
            receiver: Arc::new(Mutex::new(rx)),
            sender: Arc::new(Mutex::new(tx.clone())),
            healthy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            receive_task: Mutex::new(None),
            auth: None,
            vault: None,
            server_name: None,
        };

        // Start SSE receive loop
        let task = tokio::spawn(Self::sse_receive_loop(
            client,
            endpoint,
            tx,
            transport.healthy.clone(),
            transport.auth_headers()?,
            transport.message_endpoint.clone(),
        ));

        *transport.receive_task.lock().await = Some(task);

        debug!("SSE transport connected to {}", transport.endpoint);
        Ok(transport)
    }

    /// Create a new SSE transport with authentication configuration.
    ///
    /// # Arguments
    /// * `endpoint` - The MCP server endpoint URL
    /// * `auth` - Authentication configuration
    /// * `vault` - Optional encrypted vault for OAuth token storage/refresh
    /// * `server_name` - Server identifier used as the OAuth vault key
    ///
    /// # Returns
    /// A new `SseTransport` connected to the server
    pub async fn connect_with_auth(
        endpoint: impl AsRef<str>,
        auth: McpAuthConfig,
        vault: Option<Arc<Vault>>,
        server_name: impl Into<String>,
    ) -> Result<Self> {
        let endpoint = Url::parse(endpoint.as_ref())?;
        debug!("Connecting to authenticated MCP server at: {}", endpoint);

        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()?;

        // Create channel for receiving messages from SSE stream
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let auth = if auth.is_empty() { None } else { Some(auth) };
        let server_name = server_name.into();
        let transport = Self {
            client: client.clone(),
            endpoint: endpoint.clone(),
            message_endpoint: Arc::new(Mutex::new(None)),
            session_id: Mutex::new(None),
            receiver: Arc::new(Mutex::new(rx)),
            sender: Arc::new(Mutex::new(tx.clone())),
            healthy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            receive_task: Mutex::new(None),
            auth: auth.clone(),
            vault: vault.clone(),
            server_name: Some(server_name.clone()),
        };

        let task = tokio::spawn(Self::sse_receive_loop(
            client,
            endpoint,
            tx,
            transport.healthy.clone(),
            transport.auth_headers()?,
            transport.message_endpoint.clone(),
        ));

        *transport.receive_task.lock().await = Some(task);

        debug!(
            "Authenticated SSE transport connected to {}",
            transport.endpoint
        );
        Ok(transport)
    }

    /// Create a new SSE transport with an existing HTTP client
    ///
    /// # Arguments
    /// * `client` - The HTTP client to use
    /// * `endpoint` - The MCP server endpoint URL
    ///
    /// # Returns
    /// A new `SseTransport` connected to the server
    pub async fn with_client(client: Client, endpoint: impl AsRef<str>) -> Result<Self> {
        let endpoint = Url::parse(endpoint.as_ref())?;

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let transport = Self {
            client: client.clone(),
            endpoint: endpoint.clone(),
            message_endpoint: Arc::new(Mutex::new(None)),
            session_id: Mutex::new(None),
            receiver: Arc::new(Mutex::new(rx)),
            sender: Arc::new(Mutex::new(tx.clone())),
            healthy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            receive_task: Mutex::new(None),
            auth: None,
            vault: None,
            server_name: None,
        };

        let task = tokio::spawn(Self::sse_receive_loop(
            client,
            endpoint,
            tx,
            transport.healthy.clone(),
            transport.auth_headers()?,
            transport.message_endpoint.clone(),
        ));

        *transport.receive_task.lock().await = Some(task);

        Ok(transport)
    }

    /// SSE receive loop - continuously reads from SSE stream
    async fn sse_receive_loop(
        client: Client,
        endpoint: Url,
        sender: tokio::sync::mpsc::Sender<JsonRpcMessage>,
        healthy: Arc<std::sync::atomic::AtomicBool>,
        auth_headers: HeaderMap,
        message_endpoint: Arc<Mutex<Option<Url>>>,
    ) {
        let mut backoff = Duration::from_secs(1);
        const MAX_BACKOFF: Duration = Duration::from_secs(30);

        while healthy.load(std::sync::atomic::Ordering::SeqCst) {
            match Self::connect_sse(
                &client,
                &endpoint,
                &sender,
                &auth_headers,
                &message_endpoint,
            )
            .await
            {
                Ok(()) => {
                    // Connection closed normally
                    debug!("SSE connection closed");
                    break;
                }
                Err(e) => {
                    error!("SSE connection error: {}, reconnecting in {:?}", e, backoff);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }

        healthy.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Connect to SSE stream and process events
    async fn connect_sse(
        client: &Client,
        endpoint: &Url,
        sender: &tokio::sync::mpsc::Sender<JsonRpcMessage>,
        auth_headers: &HeaderMap,
        message_endpoint: &Arc<Mutex<Option<Url>>>,
    ) -> Result<()> {
        let mut headers = auth_headers.clone();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        let response = client
            .get(endpoint.as_str())
            .headers(headers)
            .send()
            .await
            .map_err(TransportError::Http)?;

        if !response.status().is_success() {
            return Err(TransportError::Sse(format!(
                "HTTP {}: {}",
                response.status(),
                response.text().await.unwrap_or_default()
            )));
        }

        debug!("SSE stream connected");

        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk.map_err(TransportError::Http)?);
            if buffer.len() > 8 * 1024 * 1024 {
                return Err(TransportError::Sse("SSE event exceeds 8 MiB".into()));
            }
            loop {
                let lf = buffer
                    .windows(2)
                    .position(|bytes| bytes == b"\n\n")
                    .map(|pos| (pos, 2));
                let crlf = buffer
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .map(|pos| (pos, 4));
                let Some((pos, delimiter)) = (match (lf, crlf) {
                    (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
                    (a, b) => a.or(b),
                }) else {
                    break;
                };
                let event = String::from_utf8(buffer.drain(..pos + delimiter).collect())
                    .map_err(|_| TransportError::Sse("SSE event is not UTF-8".into()))?;
                if event.lines().any(|line| line.trim() == "event: endpoint") {
                    let data = event
                        .lines()
                        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let post = endpoint.join(&data)?;
                    if post.origin() != endpoint.origin() {
                        return Err(TransportError::Sse(
                            "SSE message endpoint changed origin".into(),
                        ));
                    }
                    *message_endpoint.lock().await = Some(post);
                } else if let Some(message) = Self::parse_sse_event(&event) {
                    if sender.send(message).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse an SSE event into a `JsonRpcMessage`
    fn parse_sse_event(event: &str) -> Option<JsonRpcMessage> {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if !data.is_empty() {
            match serde_json::from_str::<JsonRpcMessage>(&data) {
                Ok(message) => Some(message),
                Err(e) => {
                    warn!("Failed to parse SSE data: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }

    /// Build the authentication headers for this transport.
    fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();

        if let Some(ref auth) = self.auth {
            // Static bearer token takes precedence when no vault token is present.
            let bearer = self
                .vault_token()
                .or_else(|| auth.bearer_token.clone())
                .filter(|t| !t.is_empty());
            if let Some(token) = bearer {
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))
                        .map_err(|e| TransportError::Sse(format!("Invalid bearer token: {e}")))?,
                );
            }

            for (name, value) in &auth.headers {
                let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| {
                        TransportError::Sse(format!("Invalid header name '{name}': {e}"))
                    })?;
                headers.insert(
                    header_name,
                    HeaderValue::from_str(value)
                        .map_err(|e| TransportError::Sse(format!("Invalid header value: {e}")))?,
                );
            }
        }

        Ok(headers)
    }

    /// Read secret material from the vault for a given namespace/name slot.
    fn vault_material(&self, namespace: &str, name: &str) -> Option<String> {
        let vault = self.vault.as_ref()?;
        vault
            .get_material_for(namespace, name)
            .ok()
            .flatten()
            .map(|s| s.expose_secret().to_string())
    }

    /// Read the current OAuth access token from the vault, if any.
    fn vault_token(&self) -> Option<String> {
        let server_name = self.server_name.as_ref()?;
        self.vault_material(&format!("oauth:{server_name}"), "default")
    }

    /// Try to refresh the OAuth token using the stored refresh token.
    /// Returns true if a new access token was written to the vault.
    async fn try_refresh_token(&self) -> bool {
        let (vault, server_name, auth) = match (&self.vault, &self.server_name, &self.auth) {
            (Some(vault), Some(server_name), Some(auth)) => (vault, server_name, auth),
            _ => return false,
        };

        let refresh_token = match vault.get_oauth_token(server_name) {
            Some(entry) => match entry.refresh_token {
                Some(t) => t,
                None => return false,
            },
            None => return false,
        };

        match crate::extensions::mcp::protocol::oauth::OAuthFlow::refresh_token(
            auth,
            &refresh_token,
        )
        .await
        {
            Ok(entry) => {
                let _ = vault.set_oauth_token(server_name, &entry);
                true
            }
            Err(e) => {
                warn!("OAuth token refresh failed for '{}': {}", server_name, e);
                false
            }
        }
    }

    /// Send the JSON-RPC body with the provided headers.
    async fn send_with_headers(
        &self,
        json: &str,
        mut headers: HeaderMap,
    ) -> Result<reqwest::Response> {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        // Add session ID if available
        if let Some(session_id) = self.session_id().await {
            headers.insert(
                "Mcp-Session-Id",
                HeaderValue::from_str(&session_id)
                    .map_err(|e| TransportError::Sse(format!("Invalid session ID: {e}")))?,
            );
        }

        let endpoint = self
            .message_endpoint
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| self.endpoint.clone());
        self.client
            .post(endpoint.as_str())
            .headers(headers)
            .body(json.to_string())
            .send()
            .await
            .map_err(TransportError::Http)
    }

    /// Get the session ID
    pub async fn session_id(&self) -> Option<String> {
        self.session_id.lock().await.clone()
    }

    /// Set the session ID
    pub async fn set_session_id(&self, id: String) {
        *self.session_id.lock().await = Some(id);
    }

    /// Get the endpoint URL
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn send(&self, message: JsonRpcMessage) -> Result<()> {
        if !self.is_healthy() {
            return Err(TransportError::ConnectionClosed);
        }

        // Serialize the message
        let json = serde_json::to_string(&message)?;
        trace!("SSE sending an MCP message");

        // Build request headers (auth + content type + accept + session id).
        let response = self.send_with_headers(&json, self.auth_headers()?).await?;

        // On 401, attempt a single OAuth token refresh and retry.
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && self.try_refresh_token().await
        {
            let response = self.send_with_headers(&json, self.auth_headers()?).await?;
            if response.status().is_success() {
                return Ok(());
            }
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(TransportError::Sse(format!("HTTP {status}: {body}")));
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(TransportError::Sse(format!("HTTP {status}: {body}")));
        }

        Ok(())
    }

    async fn recv(&self, timeout: Duration) -> Result<Option<JsonRpcMessage>> {
        if !self.is_healthy() {
            return Err(TransportError::ConnectionClosed);
        }

        match tokio::time::timeout(timeout, self.receiver.lock().await.recv()).await {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => {
                // Channel closed
                self.healthy
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                Err(TransportError::ConnectionClosed)
            }
            Err(_) => Ok(None), // Timeout
        }
    }

    async fn close(&self) -> Result<()> {
        debug!("Closing SSE transport");

        self.healthy
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // Stop the receive task
        if let Some(task) = self.receive_task.lock().await.take() {
            task.abort();
        }

        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for SseTransport {
    fn drop(&mut self) {
        self.healthy
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(task) = self.receive_task.get_mut().take() {
            task.abort();
        }
    }
}

/// SSE event type
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SseEvent {
    event: Option<String>,
    data: String,
    id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::vault::{Credential, CredentialKind, Vault};
    use crate::extensions::mcp::protocol::types::JsonRpcRequest;
    use secrecy::SecretString;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_in_memory_transport() {
        let (transport1, transport2) = InMemoryTransport::pair();

        // Send a message from transport1 to transport2
        let request = JsonRpcMessage::Request(JsonRpcRequest::new(
            1i64,
            "test",
            Some(serde_json::json!({"hello": "world"})),
        ));

        transport1.send(request.clone()).await.unwrap();

        // Receive on transport2
        let received = transport2
            .recv(Duration::from_secs(1))
            .await
            .unwrap()
            .expect("should receive message");

        match (request, received) {
            (JsonRpcMessage::Request(sent), JsonRpcMessage::Request(received)) => {
                assert_eq!(sent.id, received.id);
                assert_eq!(sent.method, received.method);
            }
            _ => panic!("Message type mismatch"),
        }
    }

    #[tokio::test]
    async fn test_in_memory_transport_timeout() {
        let (transport1, transport2) = InMemoryTransport::pair();

        // Try to receive without sending (should timeout)
        let result = transport1.recv(Duration::from_millis(10)).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        // Keep transport2 alive until end of test
        drop(transport2);
    }

    #[tokio::test]
    async fn test_in_memory_transport_close() {
        let (transport1, _transport2) = InMemoryTransport::pair();

        // Close transport1
        transport1.close().await.unwrap();

        // Sending should fail
        let request = JsonRpcMessage::Request(JsonRpcRequest::new(1i64, "test", None));
        assert!(transport1.send(request).await.is_err());

        // Receiving on transport2 should also fail (channel closed)
        // Note: It might succeed if the message was already in the buffer
        // but eventually it will fail
    }

    #[test]
    fn vault_material_reads_from_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::for_test(tmp.path(), "test-passphrase"));
        vault
            .set_credential(&Credential::now(
                "mcp:analytics",
                "default",
                CredentialKind::ApiKey,
                SecretString::new("analytics-key".into()),
            ))
            .unwrap();

        let (tx, rx) = tokio::sync::mpsc::channel::<JsonRpcMessage>(1);
        let transport = SseTransport {
            client: Client::new(),
            endpoint: Url::parse("http://localhost:8080").unwrap(),
            message_endpoint: Arc::new(Mutex::new(None)),
            session_id: Mutex::new(None),
            receiver: Arc::new(Mutex::new(rx)),
            sender: Arc::new(Mutex::new(tx)),
            healthy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            receive_task: Mutex::new(None),
            auth: None,
            vault: Some(vault),
            server_name: None,
        };

        assert_eq!(
            transport.vault_material("mcp:analytics", "default"),
            Some("analytics-key".to_string())
        );
        assert_eq!(transport.vault_material("mcp:analytics", "missing"), None);
        assert_eq!(transport.vault_material("mcp:other", "default"), None);
    }
}
