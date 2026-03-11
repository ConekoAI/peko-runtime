//! MCP client implementation
//!
//! Provides a high-level client for communicating with MCP servers.
//! Handles JSON-RPC request/response correlation, initialization, and lifecycle.

use crate::mcp::transport::{McpTransport, TransportError};
use crate::mcp::types::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
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

/// MCP client for communicating with MCP servers
///
/// Manages the connection lifecycle and provides high-level methods
/// for MCP protocol operations.
pub struct McpClient {
    /// The underlying transport
    transport: Box<dyn McpTransport>,
    /// Server information after initialization
    server_info: Option<ServerInfo>,
    /// Request counter for generating unique IDs
    request_counter: AtomicU64,
    /// Client capabilities
    client_capabilities: ClientCapabilities,
    /// Client implementation info
    client_info: Implementation,
    /// Receive task handle (used in Phase 2 for background message handling)
    #[allow(dead_code)]
    receive_task: Option<tokio::task::JoinHandle<()>>,
    /// Shutdown signal (used in Phase 2)
    #[allow(dead_code)]
    shutdown_tx: Option<tokio::sync::mpsc::Sender<()>>,
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
    /// A new un-initialized MCP client
    pub fn new(transport: Box<dyn McpTransport>) -> Self {
        Self {
            transport,
            server_info: None,
            request_counter: AtomicU64::new(1),
            client_capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "pekobot".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            receive_task: None,
            shutdown_tx: None,
        }
    }

    /// Create a new MCP client with custom capabilities
    ///
    /// # Arguments
    /// * `transport` - The transport to use
    /// * `capabilities` - Client capabilities to advertise
    /// * `client_info` - Client implementation information
    pub fn with_capabilities(
        transport: Box<dyn McpTransport>,
        capabilities: ClientCapabilities,
        client_info: Implementation,
    ) -> Self {
        Self {
            transport,
            server_info: None,
            request_counter: AtomicU64::new(1),
            client_capabilities: capabilities,
            client_info,
            receive_task: None,
            shutdown_tx: None,
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
        let request = JsonRpcMessage::Request(JsonRpcRequest::new(id.clone(), method, params));

        // Send request
        trace!("Sending request {}: {}", id, method);
        self.transport.send(request).await?;

        // Wait for response with timeout
        let timeout = Duration::from_secs(30);

        loop {
            match tokio::time::timeout(timeout, self.transport.recv(timeout)).await {
                Ok(Ok(Some(JsonRpcMessage::Response(response)))) => {
                    // Check if this is the response we're waiting for
                    if response.id() == &id {
                        trace!("Received response for request {}", id);
                        return match response.into_result() {
                            Ok(result) => serde_json::from_value(result).map_err(|e| {
                                ClientError::InvalidResponse(format!(
                                    "Failed to deserialize response for {}: {}",
                                    method, e
                                ))
                            }),
                            Err(error) => Err(ClientError::JsonRpc(error.code, error.message)),
                        };
                    } else {
                        // Response for a different request (shouldn't happen in Phase 1)
                        warn!(
                            "Received response for unexpected request: {:?}",
                            response.id()
                        );
                        continue;
                    }
                }
                Ok(Ok(Some(JsonRpcMessage::Notification(notification)))) => {
                    // Handle server-initiated notifications (Phase 2)
                    trace!("Received notification: {}", notification.method);
                    continue;
                }
                Ok(Ok(Some(JsonRpcMessage::Request(request)))) => {
                    // Server-initiated request (Phase 2)
                    trace!("Received server request: {}", request.method);
                    continue;
                }
                Ok(Ok(None)) => {
                    // Timeout waiting for message
                    return Err(ClientError::Timeout);
                }
                Ok(Err(e)) => {
                    return Err(ClientError::Transport(e));
                }
                Err(_) => {
                    // Overall timeout
                    return Err(ClientError::Timeout);
                }
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

    /// Start the receive loop
    fn start_receive_loop(&mut self) {
        // For Phase 1, we use synchronous receive in request() for simplicity
        // Phase 2 will add a proper background receive loop for handling
        // server-initiated messages (notifications, requests)
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
    use crate::mcp::transport::InMemoryTransport;

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
