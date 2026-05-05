//! Extension runtime layer
//!
//! This module contains runtime adapters and starters that bridge extension
//! protocols to the daemon's background runtime infrastructure.
//!
//! # MCP Runtime
//! - `mcp_runtime_adapter`: Bridges MCP servers to BackgroundRuntimeManager
//! - `mcp_tool_proxy`: Adapts MCP tools to Pekobot's Tool trait
//! - `mcp_injectable_proxy`: Wraps McpToolProxy with reserved parameter injection
//! - `mcp_starter`: ExtensionRuntimeStarter implementation for MCP extensions
//!
//! # Gateway Runtime
//! - `gateway_runtime_adapter`: BackgroundRuntimeAdapter implementation for gateways
//! - `gateway_router`: Routes incoming gateway messages to agents
//! - `gateway_starter`: ExtensionRuntimeStarter implementation for gateway extensions

pub mod gateway_runtime_adapter;
pub mod gateway_router;
pub mod gateway_starter;
pub mod mcp_runtime_adapter;
pub mod mcp_starter;
pub mod mcp_tool_proxy;
pub mod mcp_injectable_proxy;

pub use gateway_runtime_adapter::{GatewayFlavor, GatewayRuntimeAdapter};
pub use gateway_router::{GatewayRouter, GatewayRoutingConfig, QueuedMessage};
pub use gateway_starter::GatewayRuntimeStarter;
pub use mcp_runtime_adapter::{McpClientRegistry, McpRuntimeAdapter, McpRuntimeAdapterError, McpServerInfo};
pub use mcp_starter::McpRuntimeStarter;
pub use mcp_tool_proxy::{create_tool_proxies, create_tool_proxy, McpToolProxy};
pub use mcp_injectable_proxy::InjectableMcpToolProxy;
