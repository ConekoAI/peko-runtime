//! Tool registry and factory
//!
//! This module provides centralized tool creation and discovery:
//! - `factory`: Creates and configures tool instances

pub mod extension_async_tool;
pub mod factory;

pub use extension_async_tool::ExtensionAsyncTool;
pub use factory::{
    CustomToolsDiscoveryResult, McpDiscoveryResult, McpFactoryConfig, ToolCreationResult,
    ToolFactory, ToolFactoryConfig,
};
