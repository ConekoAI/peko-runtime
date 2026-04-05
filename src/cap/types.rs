//! Unified types for capability management
//!
//! These types provide a common interface across all capability sources:
//! - Built-in capabilities (compiled into runtime)
//! - MCP capabilities (from MCP servers)
//! - Universal capabilities (local executables with JSON-RPC protocol)
//! - Downloaded capabilities (from Pekohub registry)

use crate::mcp::config::McpServerConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Capability type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityType {
    /// Built-in compiled capabilities (shell, filesystem, cron, etc.)
    BuiltIn,
    /// MCP server capabilities (remote JSON-RPC via stdio or SSE)
    Mcp,
    /// Universal Capability (local executable with JSON-RPC stdio)
    Universal,
    /// Downloaded from Pekohub registry
    Downloaded,
}

impl std::fmt::Display for CapabilityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapabilityType::BuiltIn => write!(f, "built-in"),
            CapabilityType::Mcp => write!(f, "mcp"),
            CapabilityType::Universal => write!(f, "universal"),
            CapabilityType::Downloaded => write!(f, "downloaded"),
        }
    }
}

/// Runtime status of a capability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    /// Capability is running (for MCP/Universal processes)
    Running,
    /// Capability is stopped
    Stopped,
    /// Capability is in an error state
    Error(String),
    /// Status is unknown
    Unknown,
}

/// Installed capability information (unified view across all capability types)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInfo {
    /// Capability name (unique identifier)
    pub name: String,
    /// Capability type classification
    pub cap_type: CapabilityType,
    /// Capability version (if available)
    pub version: String,
    /// Human-readable description
    pub description: String,
    /// Installation path (for local capabilities)
    pub install_path: PathBuf,
    /// Path to manifest file (if applicable)
    pub manifest_path: Option<PathBuf>,
    /// Whether the capability is active
    pub is_active: bool,
    /// MCP-specific server configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_config: Option<McpServerConfig>,
}

impl CapabilityInfo {
    /// Create info for a built-in capability
    pub fn builtin(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            cap_type: CapabilityType::BuiltIn,
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
            cap_type: CapabilityType::Mcp,
            version: String::new(),
            description,
            install_path: PathBuf::new(),
            manifest_path: None,
            is_active: config.auto_start,
            server_config: Some(config),
        }
    }

    /// Create info for a Universal Capability
    pub fn universal(
        name: impl Into<String>,
        executable: PathBuf,
        manifest_path: Option<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            cap_type: CapabilityType::Universal,
            version: String::new(),
            description: String::new(),
            install_path: executable,
            manifest_path,
            is_active: true,
            server_config: None,
        }
    }

    /// Create info for a downloaded capability
    pub fn downloaded(
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
        install_path: PathBuf,
        manifest_path: Option<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            cap_type: CapabilityType::Downloaded,
            version: version.into(),
            description: description.into(),
            install_path,
            manifest_path,
            is_active: true,
            server_config: None,
        }
    }
}

/// Capability installation source specification
#[derive(Debug, Clone)]
pub enum CapabilitySpec {
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
    /// From directory containing capability files
    Directory {
        path: PathBuf,
    },
}

/// Search result from remote registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySearchResult {
    /// Capability name
    pub name: String,
    /// Latest version
    pub version: String,
    /// Capability description
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

/// Capability with runtime status
#[derive(Debug, Clone)]
pub struct CapabilityWithStatus {
    pub info: CapabilityInfo,
    pub status: CapabilityStatus,
}

impl CapabilityWithStatus {
    pub fn new(info: CapabilityInfo, status: CapabilityStatus) -> Self {
        Self { info, status }
    }
}

// Backward compatibility aliases
#[deprecated(since = "0.12.0", note = "Use CapabilityType instead")]
pub type ToolType = CapabilityType;

#[deprecated(since = "0.12.0", note = "Use CapabilityStatus instead")]
pub type ToolStatus = CapabilityStatus;

#[deprecated(since = "0.12.0", note = "Use CapabilityInfo instead")]
pub type InstalledToolInfo = CapabilityInfo;

#[deprecated(since = "0.12.0", note = "Use CapabilitySearchResult instead")]
pub type ToolSearchResult = CapabilitySearchResult;

#[deprecated(since = "0.12.0", note = "Use CapabilityWithStatus instead")]
pub type ToolWithStatus = CapabilityWithStatus;

#[deprecated(since = "0.12.0", note = "Use CapabilitySpec instead")]
pub type ToolSpec = CapabilitySpec;
