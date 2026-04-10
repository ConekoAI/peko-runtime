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
//!
//! # Extension Architecture Integration (Phase 3)
//!
//! Tools are now managed through the Extension system via ExtensionManager.
//! This module provides the core protocol implementation.

pub mod adapter;
pub mod manifest;
pub mod protocol;
pub mod transport;

#[cfg(test)]
mod tests;

pub use adapter::{UniversalToolAdapter, UniversalToolBuilder};
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

use std::path::PathBuf;

/// Get the default system-wide Universal Tools directory
///
/// This is `{data_dir}/tools` where `data_dir` is the Pekobot data directory
/// (e.g., `~/.local/share/pekobot/tools` on Linux,
///  `~/Library/Application Support/pekobot/tools` on macOS,
///  `%APPDATA%/pekobot/tools` on Windows)
pub fn default_tools_dir() -> PathBuf {
    crate::common::paths::default_data_dir().join("tools")
}
