//! Principal-owned MCP service (ADR-075).
//!
//! `service` drives native workspace discovery, connections, OAuth and scoped calls.
//! `protocol`, `runtime`, and `workspace` retain legacy adapter APIs; native
//! principal wiring does not use their shared-manager installation path.
//!
//! Phase 2 PR 2 (ADR-047 §2.3) removed the framework-side
//! `ExtensionTypeAdapter` for MCP. The previous `adapter` submodule
//! held `McpAdapter`, `DiscoveredMcpServer`,
//! `McpServerInitFactory`, `McpServerShutdownFactory`,
//! `McpContextHandlerFactory`, `McpToolExecuteHandler`,
//! `load_servers_from_directory`, and `load_and_register_servers`;
//! all of that has been replaced by direct calls to the
//! [`McpManager`] runtime API plus the workspace scanner.

pub mod protocol;
pub mod runtime;
pub mod service;
pub mod workspace;

// Re-export key types for convenience
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
pub use workspace::{
    discover_workspace_mcp_servers, load_workspace_mcp_servers, render_mcp_prompt_context,
};

pub mod reserved_params;
