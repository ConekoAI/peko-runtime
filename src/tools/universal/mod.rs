//! Universal Tool Protocol - Backend-agnostic tool integration
//!
//! This module implements a universal protocol for integrating tools written
//! in any language (Python, Node, Go, Rust, etc.) with Pekobot.
//!
//! # Architecture (SRP)
//!
//! - `protocol`: JSON-RPC message types (pure data)
//! - `manifest`: Tool metadata and reserved parameter definitions
//! - `transport`: Stdio communication layer
//! - `adapter`: Tool trait implementation with parameter injection
//! - `discovery`: Tool discovery from directories
//!
//! # Usage
//!
//! ```rust,ignore
//! // Discover and load tools
//! let tools = universal::load_universal_tools("./tools").await?;
//!
//! // Use in agent
//! for tool in tools {
//!     registry.register(Arc::new(tool));
//! }
//! ```
//!
//! # Extension Architecture Integration (Phase 3)
//!
//! This module now supports dual-mode operation:
//! - Legacy mode: Direct tool loading via discovery/load functions
//! - Extension mode: Registration with ExtensionCore for unified management
//!
//! The Extension mode is the preferred approach for new code.

pub mod adapter;
pub mod discovery;
pub mod manifest;
pub mod protocol;
pub mod transport;

#[cfg(test)]
mod tests;

pub use adapter::{UniversalToolAdapter, UniversalToolBuilder};
pub use discovery::{discover_universal_tools, load_universal_tools, DiscoveredTool};
pub use manifest::{
    merge_with_injection, Manifest, ParamSource, ParamSourceLegacy, 
    ProtocolConfig, ReservedParam, ReservedParamsConfig
};
pub use protocol::{ErrorObject, ExecutionContext, ExecuteParams, ExecuteResult, Request, Response, ResponseResult, DescribeResult, PROTOCOL_VERSION};

// Re-export Extension Architecture integration (Phase 3)
pub use crate::extensions::adapters::universal_tool_adapter::{
    DiscoveredUniversalTool, UniversalToolAdapter as ExtensionUniversalToolAdapter,
    load_tools_from_directory, register_tools_with_core, load_and_register_tools,
};
