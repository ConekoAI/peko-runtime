//! MCP Protocol Module
//!
//! Contains the MCP protocol implementation: client, transport, types, config,
//! discovery, and manager.

pub mod client;
pub mod config;
pub mod discovery;
pub mod manager;
pub mod oauth;
pub mod sampling;
pub mod transport;
pub mod types;

// Re-export common types for convenience
pub use client::{ClientError, McpClient};
pub use sampling::SamplingRequestHandler;
pub use config::{ConfigFormat, McpConfig, McpServerConfig, TransportType};
pub use discovery::{
    discover_servers, ensure_default_config, is_server_installed, mcp_config_path, mcp_install_dir,
    DiscoveredServer, McpServerStatus,
};
pub use manager::{ManagerError, McpManager, ServerState};
pub use transport::{
    InMemoryTransport, McpTransport, SseTransport, StdioTransport, TransportError,
};
pub use types::*;

pub mod prelude {
    pub use super::{
        ClientCapabilities, Implementation, McpClient, McpTransport, ServerCapabilities,
        ServerInfo, StdioTransport, Tool,
    };
}
