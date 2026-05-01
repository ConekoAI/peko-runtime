//! Tool registry and factory
//!
//! This module provides centralized tool creation, discovery, and registration:
//! - `factory`: Creates and configures tool instances
//! - `builtin`: Registers built-in tools with the Extension Framework

pub mod builtin;
pub mod factory;

pub use builtin::{BuiltinToolRegistrar, BuiltinToolRegistrarConfig};
pub use factory::{
    CustomToolsDiscoveryResult, McpDiscoveryResult, McpFactoryConfig, ToolCreationResult,
    ToolFactory, ToolFactoryConfig,
};
