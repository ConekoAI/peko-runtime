//! MCP (Model Context Protocol) support for Pekobot
//!
//! This module provides an implementation of the Model Context Protocol (MCP),
//! allowing Pekobot to connect to MCP servers and use their tools, resources,
//! and prompts.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use pekobot::mcp::{McpClient, StdioTransport, McpServerConfig};
//! use std::collections::HashMap;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Create transport
//!     let transport = StdioTransport::spawn(
//!         "my-mcp-server",
//!         &["--verbose".to_string()],
//!         &HashMap::new(),
//!         None,
//!     ).await?;
//!
//!     // Create client
//!     let mut client = McpClient::new(Box::new(transport));
//!
//!     // Initialize
//!     let server_info = client.initialize().await?;
//!     println!("Connected to: {} v{}",
//!         server_info.server_info.name,
//!         server_info.server_info.version
//!     );
//!
//!     // List tools
//!     let tools = client.list_tools().await?;
//!     for tool in tools {
//!         println!("Tool: {} - {}", tool.name, tool.description);
//!     }
//!
//!     // Call a tool
//!     let result = client.call_tool("my_tool", serde_json::json!({
//!         "arg": "value"
//!     })).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! # Architecture
//!
//! The MCP module is organized into several components:
//!
//! - **types**: MCP protocol types (JSON-RPC messages, tool/resource/prompt definitions)
//! - **transport**: Transport abstractions (stdio, SSE) for communicating with MCP servers
//! - **client**: High-level MCP client for interacting with servers
//! - **manager** (Phase 2): Server lifecycle management
//! - **tool_proxy** (Phase 3): Integration with Pekobot's Tool trait
//!
//! # Protocol Version
//!
//! This implementation targets MCP protocol version `2024-11-05`.

// Public exports
pub use client::{ClientError, McpClient};
pub use config::{ConfigFormat, McpConfig, McpServerConfig, TransportType};
pub use discovery::{
    discover_servers, ensure_default_config, is_server_installed, mcp_config_path, mcp_install_dir,
    DiscoveredServer, McpServerStatus,
};
pub use injectable_proxy::InjectableMcpToolProxy;
pub use manager::{ManagerError, McpManager, ServerState};
pub use tool_proxy::{create_tool_proxies, McpToolProxy};
pub use transport::{
    InMemoryTransport, McpTransport, SseTransport, StdioTransport, TransportError,
};
pub use types::*;

// Submodules
pub mod client;
pub mod config;
pub mod discovery;
pub mod injectable_proxy;
pub mod manager;
pub mod tool_proxy;
pub mod transport;
pub mod types;

// Re-export common types for convenience
pub mod prelude {
    pub use crate::mcp::{
        ClientCapabilities, Implementation, McpClient, McpTransport, ServerCapabilities,
        ServerInfo, StdioTransport, Tool,
    };
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use transport::InMemoryTransport;

    // Note: Full mock server implementation will be added in Phase 2
    // For now, we test the transport layer directly

    #[tokio::test]
    async fn test_mcp_client_transport_pair() {
        // Create a pair of connected transports
        let (transport1, transport2) = InMemoryTransport::pair();

        // Both should be healthy initially
        assert!(transport1.is_healthy());
        assert!(transport2.is_healthy());

        // Create clients
        let client1 = McpClient::new(Box::new(transport1));
        let _client2 = McpClient::new(Box::new(transport2));

        // Client should not be initialized yet
        assert!(!client1.is_initialized());
        assert!(client1.server_info().is_none());
    }
}
