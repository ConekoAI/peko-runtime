//! Extension runtime layer
//!
//! This module contains runtime adapters and starters that bridge extension
//! protocols to the daemon's background runtime infrastructure.
//!
//! # MCP Runtime
//! - `mcp_runtime_adapter`: Bridges MCP servers to BackgroundRuntimeManager
//! - `mcp_tool_proxy`: Adapts MCP tools to Pekobot's Tool trait
//! - `mcp_injectable_proxy`: Wraps McpToolProxy with reserved parameter injection

pub mod mcp_runtime_adapter;
pub mod mcp_tool_proxy;
pub mod mcp_injectable_proxy;

pub use mcp_runtime_adapter::{McpClientRegistry, McpRuntimeAdapter, McpRuntimeAdapterError, McpServerInfo};
pub use mcp_tool_proxy::{create_tool_proxies, create_tool_proxy, McpToolProxy};
pub use mcp_injectable_proxy::InjectableMcpToolProxy;
