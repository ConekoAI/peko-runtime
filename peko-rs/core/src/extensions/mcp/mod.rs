//! MCP Extension Type Implementation
//!
//! This module contains all MCP-specific code:
//! - `global`: Process-global `McpManager` accessor + daemon-shared
//!   resource initialiser
//! - `runtime`: Runtime adapters and starters bridging to background_runtime
//! - `protocol`: MCP protocol implementation (client, transport, types, etc.)
//! - `workspace`: Workspace scanner that registers `<workspace>/mcp/`
//!   servers with the global `McpManager`, plus the prompt-context
//!   text renderer that replaces the deleted framework hook
//!
//! Phase 2 PR 2 (ADR-047 §2.3) removed the framework-side
//! `ExtensionTypeAdapter` for MCP. The previous `adapter` submodule
//! held `McpAdapter`, `DiscoveredMcpServer`,
//! `McpServerInitFactory`, `McpServerShutdownFactory`,
//! `McpContextHandlerFactory`, `McpToolExecuteHandler`,
//! `load_servers_from_directory`, and `load_and_register_servers`;
//! all of that has been replaced by direct calls to the
//! [`McpManager`] runtime API plus the workspace scanner.

pub mod global;
pub mod protocol;
pub mod runtime;
pub mod workspace;

// Re-export key types for convenience
pub use global::{
    global_mcp_manager, init_global_mcp_manager_with_shared_resources,
};
pub use workspace::{
    discover_workspace_mcp_servers, load_workspace_mcp_servers,
    render_mcp_prompt_context,
};
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
