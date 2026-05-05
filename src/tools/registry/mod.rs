//! Tool registry and factory
//!
//! This module provides centralized tool creation and discovery:
//! - `factory`: Creates and configures tool instances

pub mod factory;

pub use factory::{
    CustomToolsDiscoveryResult, McpDiscoveryResult, McpFactoryConfig, ToolCreationResult,
    ToolFactory, ToolFactoryConfig,
};
