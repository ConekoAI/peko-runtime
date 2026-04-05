//! Unified types for tool management
//!
//! These types provide a common interface across all tool sources:
//! - Built-in tools (compiled into runtime)
//! - MCP tools (from MCP servers)
//! - Universal tools (local executables with JSON-RPC protocol)
//! - Downloaded tools (from Pekohub registry)

use crate::mcp::config::McpServerConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Tool type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolType {
    /// Built-in compiled tools (shell, filesystem, cron, etc.)
    BuiltIn,
    /// MCP server tools (remote JSON-RPC via stdio or SSE)
    Mcp,
    /// Universal Tool (local executable with JSON-RPC stdio)
    Universal,
    /// Downloaded from Pekohub registry
    Downloaded,
}

impl std::fmt::Display for ToolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolType::BuiltIn => write!(f, "built-in"),
            ToolType::Mcp => write!(f, "mcp"),
            ToolType::Universal => write!(f, "universal"),
            ToolType::Downloaded => write!(f, "downloaded"),
        }
    }
}

/// Runtime status of a tool
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Tool is running (for MCP/Universal processes)
    Running,
    /// Tool is stopped
    Stopped,
    /// Tool is in an error state
    Error(String),
    /// Status is unknown
    Unknown,
}

/// Installed tool information (unified view across all tool types)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledToolInfo {
    /// Tool name (unique identifier)
    pub name: String,
    /// Tool type classification
    pub tool_type: ToolType,
    /// Tool version (if available)
    pub version: String,
    /// Human-readable description
    pub description: String,
    /// Installation path (for local tools)
    pub install_path: PathBuf,
    /// Path to manifest file (if applicable)
    pub manifest_path: Option<PathBuf>,
    /// Whether the tool is active
    pub is_active: bool,
    /// MCP-specific server configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_config: Option<McpServerConfig>,
}

impl InstalledToolInfo {
    /// Create info for a built-in tool
    pub fn builtin(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tool_type: ToolType::BuiltIn,
            version: String::new(),
            description: description.into(),
            install_path: PathBuf::new(),
            manifest_path: None,
            is_active: true,
            server_config: None,
        }
    }

    /// Create info for an MCP server
    pub fn mcp(config: McpServerConfig) -> Self {
        // Derive description from command or transport type
        let description = config
            .command
            .as_ref()
            .map(|c| format!("MCP server: {}", c))
            .or_else(|| config.endpoint.as_ref().map(|e| format!("MCP server: {}", e)))
            .unwrap_or_else(|| format!("MCP server ({:?})", config.transport));

        Self {
            name: config.name.clone(),
            tool_type: ToolType::Mcp,
            version: String::new(),
            description,
            install_path: PathBuf::new(),
            manifest_path: None,
            is_active: config.auto_start,
            server_config: Some(config),
        }
    }

    /// Create info for a Universal Tool
    pub fn universal(
        name: impl Into<String>,
        executable: PathBuf,
        manifest_path: Option<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            tool_type: ToolType::Universal,
            version: String::new(),
            description: String::new(),
            install_path: executable,
            manifest_path,
            is_active: true,
            server_config: None,
        }
    }

    /// Create info for a downloaded tool
    pub fn downloaded(
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
        install_path: PathBuf,
        manifest_path: Option<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            tool_type: ToolType::Downloaded,
            version: version.into(),
            description: description.into(),
            install_path,
            manifest_path,
            is_active: true,
            server_config: None,
        }
    }
}

/// Tool installation source specification
#[derive(Debug, Clone)]
pub enum ToolSpec {
    /// From local TOML manifest file
    Manifest {
        path: PathBuf,
    },
    /// From MCP server configuration
    Mcp {
        name: String,
        transport: crate::mcp::config::TransportType,
        command: Option<String>,
        args: Vec<String>,
        env: std::collections::HashMap<String, String>,
        cwd: Option<PathBuf>,
        endpoint: Option<String>,
        auto_start: bool,
    },
    /// From Pekohub registry by name
    Registry {
        name: String,
        version: Option<String>,
    },
    /// From directory containing tool files
    Directory {
        path: PathBuf,
    },
}

/// Search result from remote registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSearchResult {
    /// Tool name
    pub name: String,
    /// Latest version
    pub version: String,
    /// Tool description
    pub description: String,
    /// Author/maintainer
    pub author: Option<String>,
    /// Categories
    pub categories: Vec<String>,
    /// Download count
    pub downloads: u64,
    /// User rating (0-5)
    pub rating: f32,
}

/// Tool with runtime status
#[derive(Debug, Clone)]
pub struct ToolWithStatus {
    pub info: InstalledToolInfo,
    pub status: ToolStatus,
}

impl ToolWithStatus {
    pub fn new(info: InstalledToolInfo, status: ToolStatus) -> Self {
        Self { info, status }
    }
}
