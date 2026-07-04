//! MCP client implementation
//!
//! Provides a high-level client for communicating with MCP servers.
//! Handles JSON-RPC request/response correlation, initialization, and lifecycle.

use crate::extensions::mcp::protocol::transport::{McpTransport, TransportError};
use crate::extensions::mcp::protocol::types::{
    CallToolRequest, CallToolResult, ClientCapabilities, GetPromptRequest, GetPromptResult,
    Implementation, InitializeRequest, InitializeResult, JsonRpcError, JsonRpcErrorResponse,
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, JsonRpcSuccess,
    ListPromptsRequest, ListPromptsResult, ListResourcesRequest, ListResourcesResult,
    ListToolsRequest, ListToolsResult, Prompt, ReadResourceRequest, ReadResourceResult, RequestId,
    Resource, ResourceContents, ServerInfo, Tool, MCP_PROTOCOL_VERSION,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, trace, warn};

/// Errors that can occur during MCP client operations
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),

    #[error("JSON-RPC error: {0} ({1})")]
    JsonRpc(i32, String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Client not initialized")]
    NotInitialized,

    #[error("Already initialized")]
    AlreadyInitialized,

    #[error("Request timeout")]
    Timeout,

    #[error("Request cancelled: {0}")]
    Cancelled(String),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Server does not support capability: {0}")]
    UnsupportedCapability(String),
}

/// Result type for client operations
pub type Result<T> = std::result::Result<T, ClientError>;

/// Handler for server-initiated JSON-RPC requests.
///
/// MCP servers can send requests to the client (e.g. `sampling/createMessage`).
/// Implementations are registered on `McpClient` via `with_handler` and are
/// invoked from the background receive loop.
#[async_trait]
pub trait ServerRequestHandler: Send + Sync {
    /// Handle a server-initiated request.
    async fn handle_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> std::result::Result<serde_json::Value, JsonRpcError>;
}

/// MCP client for communicating with MCP servers
///
/// Manages the connection lifecycle and provides high-level methods
/// for MCP protocol operations.
pub struct McpClient {
    /// The underlying transport (shared with the background receive loop)
    transport: Arc<dyn McpTransport>,
    /// Server information after initialization
    server_info: Option<ServerInfo>,
    /// Request counter for generating unique IDs
    request_counter: AtomicU64,
    /// Client capabilities
    client_capabilities: ClientCapabilities,
    /// Client implementation info
    client_info: Implementation,
    /// Pending requests waiting for a JSON-RPC response
    pending: Arc<Mutex<HashMap<RequestId, tokio::sync::oneshot::Sender<JsonRpcResponse>>>>,
    /// Receive task handle
    receive_task: Option<tokio::task::JoinHandle<()>>,
    /// Shutdown signal for the receive loop
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server_info", &self.server_info)
            .field("request_counter", &self.request_counter)
            .field("client_capabilities", &self.client_capabilities)
            .field("client_info", &self.client_info)
            .field("initialized", &self.server_info.is_some())
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// Create a new MCP client with the given transport
    ///
    /// # Arguments
    /// * `transport` - The transport to use for communication
    ///
    /// # Returns
    /// A new un-initialized MCP client with a background receive loop running.
    #[must_use]
    pub fn new(transport: Box<dyn McpTransport>) -> Self {
        Self::with_handler_and_capabilities(
            transport,
            None,
            ClientCapabilities::for_peko(),
            Implementation {
                name: "peko".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        )
    }

    /// Create a new MCP client with a handler for server-initiated requests.
    ///
    /// This is used when the host wants to support server-to-client methods such
    /// as `sampling/createMessage`.
    #[must_use]
    pub fn with_handler(
        transport: Box<dyn McpTransport>,
        handler: Arc<dyn ServerRequestHandler>,
    ) -> Self {
        Self::with_handler_and_capabilities(
            transport,
            Some(handler),
            ClientCapabilities::for_peko(),
            Implementation {
                name: "peko".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        )
    }

    /// Create a new MCP client with custom capabilities
    ///
    /// # Arguments
    /// * `transport` - The transport to use
    /// * `capabilities` - Client capabilities to advertise
    /// * `client_info` - Client implementation information
    #[must_use]
    pub fn with_capabilities(
        transport: Box<dyn McpTransport>,
        capabilities: ClientCapabilities,
        client_info: Implementation,
    ) -> Self {
        Self::with_handler_and_capabilities(transport, None, capabilities, client_info)
    }

    /// Internal constructor that wires up the shared transport and background loop.
    fn with_handler_and_capabilities(
        transport: Box<dyn McpTransport>,
        handler: Option<Arc<dyn ServerRequestHandler>>,
        capabilities: ClientCapabilities,
        client_info: Implementation,
    ) -> Self {
        let transport = Arc::from(transport);
        let pending: Arc<Mutex<HashMap<RequestId, tokio::sync::oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

        let receive_task =
            Self::spawn_receive_loop(Arc::clone(&transport), Arc::clone(&pending), handler, shutdown_rx);

        Self {
            transport,
            server_info: None,
            request_counter: AtomicU64::new(1),
            client_capabilities: capabilities,
            client_info,
            pending,
            receive_task: Some(receive_task),
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Spawn the background receive/dispatch loop.
    fn spawn_receive_loop(
        transport: Arc<dyn McpTransport>,
        pending: Arc<Mutex<HashMap<RequestId, tokio::sync::oneshot::Sender<JsonRpcResponse>>>>,
        handler: Option<Arc<dyn ServerRequestHandler>>,
        mut shutdown_rx: mpsc::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let recv_timeout = Duration::from_millis(500);
            loop {
                let msg = tokio::select! {
                    _ = shutdown_rx.recv() => {
                        trace!("MCP receive loop shutting down");
                        break;
                    }
                    msg = transport.recv(recv_timeout) => msg,
                };

                match msg {
                    Ok(Some(JsonRpcMessage::Response(response))) => {
                        let id = response.id().clone();
                        let sender = pending.lock().await.remove(&id);
                        if let Some(tx) = sender {
                            trace!("Routing response for request {}", id);
                            let _ = tx.send(response);
                        } else {
                            warn!("Received response for unknown request id: {}", id);
                        }
                    }
                    Ok(Some(JsonRpcMessage::Request(request))) => {
                        trace!(
                            "Received server-initiated request {}: {}",
                            request.id,
                            request.method
                        );
                        if let Some(handler) = handler.clone() {
                            let transport = Arc::clone(&transport);
                            tokio::spawn(async move {
                                let id = request.id.clone();
                                let result = handler
                                    .handle_request(&request.method, request.params.clone())
                                    .await;
                                let response = Self::build_response(id, result);
                                if let Err(e) = transport.send(response).await {
                                    warn!("Failed to send response to server request: {}", e);
                                }
                            });
                        } else {
                            let response = Self::build_response(
                                request.id.clone(),
                                Err(JsonRpcError {
                                    code: JsonRpcError::METHOD_NOT_FOUND,
                                    message: format!("Method '{}' not found", request.method),
                                    data: None,
                                }),
                            );
                            if let Err(e) = transport.send(response).await {
                                warn!("Failed to send method-not-found response: {}", e);
                            }
                        }
                    }
                    Ok(Some(JsonRpcMessage::Notification(notification))) => {
                        trace!("Received notification: {}", notification.method);
                    }
                    Ok(None) => {
                        // Receive timeout — loop and check shutdown signal
                    }
                    Err(e) => {
                        warn!("MCP transport receive error: {}", e);
                        // If the transport has become permanently unhealthy, exit the loop
                        // so we don't burn CPU waiting for a dead connection.
                        if !transport.is_healthy() {
                            warn!("MCP transport unhealthy; exiting receive loop");
                            break;
                        }
                        // Otherwise continue; the transport may recover or the client
                        // will be shut down explicitly via `shutdown_tx`.
                    }
                }
            }
        })
    }

    /// Build a JSON-RPC response message from a result.
    fn build_response(
        id: RequestId,
        result: std::result::Result<serde_json::Value, JsonRpcError>,
    ) -> JsonRpcMessage {
        match result {
            Ok(value) => JsonRpcMessage::Response(JsonRpcResponse::Success(JsonRpcSuccess {
                jsonrpc: crate::extensions::mcp::protocol::types::JSONRPC_VERSION.to_string(),
                id,
                result: value,
            })),
            Err(error) => JsonRpcMessage::Response(JsonRpcResponse::Error(JsonRpcErrorResponse {
                jsonrpc: crate::extensions::mcp::protocol::types::JSONRPC_VERSION.to_string(),
                id,
                error,
            })),
        }
    }

    /// Initialize the connection to the MCP server
    ///
    /// This must be called before any other operations.
    /// Performs the MCP initialization handshake.
    ///
    /// # Returns
    /// The server information returned by the server
    ///
    /// # Errors
    /// * `ClientError::AlreadyInitialized` if already initialized
    /// * `ClientError::Transport` if communication fails
    pub async fn initialize(&mut self) -> Result<&ServerInfo> {
        if self.server_info.is_some() {
            return Err(ClientError::AlreadyInitialized);
        }

        debug!("Initializing MCP client");

        // Start the receive loop
        self.start_receive_loop();

        // Send initialize request
        let request = InitializeRequest {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: self.client_capabilities.clone(),
            client_info: self.client_info.clone(),
        };

        let result: InitializeResult = self
            .request("initialize", Some(serde_json::to_value(request)?))
            .await?;

        // Validate protocol version
        if result.protocol_version != MCP_PROTOCOL_VERSION {
            warn!(
                "Server protocol version ({}) doesn't match client ({})",
                result.protocol_version, MCP_PROTOCOL_VERSION
            );
        }

        // Store server info
        self.server_info = Some(ServerInfo::from(result));

        // Send initialized notification
        self.notify("notifications/initialized", None).await?;

        debug!("MCP client initialized successfully");
        Ok(self.server_info.as_ref().unwrap())
    }

    /// Check if the client is initialized
    pub fn is_initialized(&self) -> bool {
        self.server_info.is_some()
    }

    /// Get server information
    ///
    /// # Returns
    /// * `Some(&ServerInfo)` if initialized
    /// * `None` if not initialized
    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.as_ref()
    }

    /// Check if the server supports a specific capability
    pub fn supports_capability(&self, capability: &str) -> bool {
        let Some(info) = &self.server_info else {
            return false;
        };

        match capability {
            "tools" => info.capabilities.tools.is_some(),
            "resources" => info.capabilities.resources.is_some(),
            "prompts" => info.capabilities.prompts.is_some(),
            "logging" => info.capabilities.logging.is_some(),
            _ => false,
        }
    }

    // ==========================================================================
    // Tool Operations
    // ==========================================================================

    /// List available tools from the server
    ///
    /// # Returns
    /// List of tool definitions
    ///
    /// # Errors
    /// * `ClientError::NotInitialized` if not initialized
    /// * `ClientError::UnsupportedCapability` if server doesn't support tools
    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        self.check_initialized()?;
        self.check_capability("tools")?;

        let request = ListToolsRequest::default();
        let result: ListToolsResult = self
            .request("tools/list", Some(serde_json::to_value(request)?))
            .await?;

        Ok(result.tools)
    }

    /// Call a tool on the server
    ///
    /// # Arguments
    /// * `name` - The tool name
    /// * `arguments` - Tool arguments (must match tool's input schema)
    ///
    /// # Returns
    /// Tool execution result
    ///
    /// # Errors
    /// * `ClientError::NotInitialized` if not initialized
    /// * `ClientError::UnsupportedCapability` if server doesn't support tools
    /// * `ClientError::JsonRpc` if the tool execution failed
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        self.check_initialized()?;
        self.check_capability("tools")?;

        let request = CallToolRequest {
            name: name.to_string(),
            arguments,
        };

        let result: CallToolResult = self
            .request("tools/call", Some(serde_json::to_value(request)?))
            .await?;

        Ok(result)
    }

    // ==========================================================================
    // Resource Operations
    // ==========================================================================

    /// List available resources from the server
    ///
    /// # Returns
    /// List of resource definitions
    pub async fn list_resources(&self) -> Result<Vec<Resource>> {
        self.check_initialized()?;
        self.check_capability("resources")?;

        let request = ListResourcesRequest::default();
        let result: ListResourcesResult = self
            .request("resources/list", Some(serde_json::to_value(request)?))
            .await?;

        Ok(result.resources)
    }

    /// Read a resource from the server
    ///
    /// # Arguments
    /// * `uri` - The resource URI
    ///
    /// # Returns
    /// Resource contents
    pub async fn read_resource(&self, uri: &str) -> Result<Vec<ResourceContents>> {
        self.check_initialized()?;
        self.check_capability("resources")?;

        let request = ReadResourceRequest {
            uri: uri.to_string(),
        };

        let result: ReadResourceResult = self
            .request("resources/read", Some(serde_json::to_value(request)?))
            .await?;

        Ok(result.contents)
    }

    // ==========================================================================
    // Prompt Operations
    // ==========================================================================

    /// List available prompts from the server
    ///
    /// # Returns
    /// List of prompt definitions
    pub async fn list_prompts(&self) -> Result<Vec<Prompt>> {
        self.check_initialized()?;
        self.check_capability("prompts")?;

        let request = ListPromptsRequest::default();
        let result: ListPromptsResult = self
            .request("prompts/list", Some(serde_json::to_value(request)?))
            .await?;

        Ok(result.prompts)
    }

    /// Get a prompt from the server
    ///
    /// # Arguments
    /// * `name` - The prompt name
    /// * `arguments` - Prompt arguments (optional)
    ///
    /// # Returns
    /// Prompt result with messages
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<GetPromptResult> {
        self.check_initialized()?;
        self.check_capability("prompts")?;

        let request = GetPromptRequest {
            name: name.to_string(),
            arguments,
        };

        let result: GetPromptResult = self
            .request("prompts/get", Some(serde_json::to_value(request)?))
            .await?;

        Ok(result)
    }

    // ==========================================================================
    // Ping
    // ==========================================================================

    /// Ping the server to check connectivity
    ///
    /// # Returns
    /// `Ok(())` if the server responded
    pub async fn ping(&self) -> Result<()> {
        self.check_initialized()?;
        let _: serde_json::Value = self.request("ping", None).await?;
        Ok(())
    }

    // ==========================================================================
    // Low-level Operations
    // ==========================================================================

    /// Send a request and wait for response
    ///
    /// # Arguments
    /// * `method` - JSON-RPC method name
    /// * `params` - Request parameters (optional)
    ///
    /// # Returns
    /// Deserialized response
    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<T> {
        let id = self.next_request_id();
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Register pending request before sending so the receive loop can route the response.
        self.pending.lock().await.insert(id.clone(), tx);

        let request = JsonRpcMessage::Request(JsonRpcRequest::new(id.clone(), method, params));
        trace!("Sending request {}: {}", id, method);

        if let Err(e) = self.transport.send(request).await {
            self.pending.lock().await.remove(&id);
            return Err(ClientError::Transport(e));
        }

        let timeout = Duration::from_secs(30);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => match response.into_result() {
                Ok(result) => serde_json::from_value(result).map_err(|e| {
                    ClientError::InvalidResponse(format!(
                        "Failed to deserialize response for {method}: {e}"
                    ))
                }),
                Err(error) => Err(ClientError::JsonRpc(error.code, error.message)),
            },
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                Err(ClientError::Cancelled(format!(
                    "Response channel closed for request {id}"
                )))
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(ClientError::Timeout)
            }
        }
    }

    /// Send a notification (no response expected)
    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let notification = JsonRpcMessage::Notification(JsonRpcNotification::new(method, params));
        self.transport.send(notification).await?;
        Ok(())
    }

    /// Generate next request ID
    fn next_request_id(&self) -> RequestId {
        let id = self.request_counter.fetch_add(1, Ordering::SeqCst);
        RequestId::Number(id as i64)
    }

    /// Check if client is initialized
    fn check_initialized(&self) -> Result<()> {
        if self.server_info.is_none() {
            return Err(ClientError::NotInitialized);
        }
        Ok(())
    }

    /// Check if server supports a capability
    fn check_capability(&self, capability: &str) -> Result<()> {
        if !self.supports_capability(capability) {
            return Err(ClientError::UnsupportedCapability(capability.to_string()));
        }
        Ok(())
    }

    /// Start the receive loop.
    ///
    /// The loop is now started automatically when the client is created, so this
    /// method is kept for backwards compatibility and is a no-op.
    fn start_receive_loop(&mut self) {
        // Background receive loop is spawned in `with_handler_and_capabilities`.
    }

    /// Shutdown the client gracefully
    pub async fn shutdown(&mut self) -> Result<()> {
        debug!("Shutting down MCP client");

        // If initialized, try to send a graceful shutdown notification
        // This is a non-standard but helpful notification to let the server know
        // the client is disconnecting (some servers handle this better than EOF)
        if self.server_info.is_some() {
            debug!("Sending shutdown notification to server");
            // Use a short timeout since we don't care if it fails
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                self.notify("notifications/exit", None),
            )
            .await;
        }

        // Signal receive loop to stop
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Wait for receive task to complete
        if let Some(task) = self.receive_task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }

        // Close transport
        self.transport.close().await?;

        debug!("MCP client shut down");
        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Try to shut down gracefully
        // Note: We can't use async here, so we rely on the transport's Drop impl
        if self.server_info.is_some() {
            debug!("McpClient dropped without explicit shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::mcp::protocol::transport::InMemoryTransport;

    fn create_test_client() -> (McpClient, McpClient) {
        let (transport1, transport2) = InMemoryTransport::pair();

        // Client 1 will be our MCP client
        let client1 = McpClient::new(Box::new(transport1));

        // Client 2 will simulate the server (not a real server, just for testing transport)
        let _client2 = McpClient::new(Box::new(transport2));

        (client1, _client2)
    }

    #[tokio::test]
    async fn test_client_not_initialized() {
        let (client, _) = create_test_client();

        // Should fail because not initialized
        assert!(matches!(
            client.list_tools().await.unwrap_err(),
            ClientError::NotInitialized
        ));
    }

    #[tokio::test]
    async fn test_request_id_generation() {
        let (client, _) = create_test_client();

        let id1 = client.next_request_id();
        let id2 = client.next_request_id();
        let id3 = client.next_request_id();

        // IDs should be sequential
        assert_eq!(id1, RequestId::Number(1));
        assert_eq!(id2, RequestId::Number(2));
        assert_eq!(id3, RequestId::Number(3));
    }

    #[tokio::test]
    async fn test_check_capability_without_init() {
        let (client, _) = create_test_client();

        // Should return false when not initialized
        assert!(!client.supports_capability("tools"));
        assert!(!client.supports_capability("resources"));
        assert!(!client.supports_capability("prompts"));
    }

    // Note: Full integration tests with a mock server would go here
    // but require more setup to simulate the server's responses
}
