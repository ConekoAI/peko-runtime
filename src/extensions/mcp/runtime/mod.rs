//! MCP Runtime Module
//!
//! Bridges MCP servers to the daemon's background runtime infrastructure.

pub mod adapter;
pub mod injectable_proxy;
pub mod starter;
pub mod tool_proxy;

pub use adapter::{
    McpClientRegistry, McpRuntimeAdapter, McpRuntimeAdapterError, McpServerInfo, McpToolCache,
};
pub use injectable_proxy::InjectableMcpToolProxy;
pub use starter::McpRuntimeStarter;
pub use tool_proxy::{McpToolProxy, create_tool_proxies, create_tool_proxy};
