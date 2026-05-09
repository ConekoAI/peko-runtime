//! MCP Extension Type Implementation
//!
//! This module contains all MCP-specific code:
//! - `adapter`: ExtensionTypeAdapter for MCP servers
//! - `runtime`: Runtime adapters and starters bridging to background_runtime
//! - `protocol`: MCP protocol implementation (client, transport, types, etc.)

pub mod adapter;
pub mod protocol;
pub mod runtime;

// Re-export key types for convenience
pub use adapter::{load_servers_from_directory, DiscoveredMcpServer, McpAdapter};
pub use protocol::{
    client::{ClientError, McpClient},
    config::{ConfigFormat, McpConfig, McpServerConfig, TransportType},
    discovery::{
        discover_servers, ensure_default_config, is_server_installed, mcp_config_path,
        mcp_install_dir, DiscoveredServer, McpServerStatus,
    },
    manager::{ManagerError, McpManager, ServerState},
    transport::{InMemoryTransport, McpTransport, SseTransport, StdioTransport, TransportError},
};
pub use runtime::{
    injectable_proxy::InjectableMcpToolProxy,
    starter::McpRuntimeStarter,
    tool_proxy::{create_tool_proxies, create_tool_proxy, McpToolProxy},
};
