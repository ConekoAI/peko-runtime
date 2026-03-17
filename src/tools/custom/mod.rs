//! Custom tools support for Pekobot
//!
//! This module implements custom tool discovery and execution from the `tools/`
//! directory in agent images. Custom tools are executables that communicate
//! via a JSON protocol over stdin/stdout.
//!
//! # Tool Discovery
//!
//! The `tools/` directory is scanned for executable files. Tools can be:
//! - Platform-agnostic (e.g., `my_tool`, `my_tool.py`)
//! - Platform-specific (e.g., `my_tool-linux-x64`, `my_tool-darwin-arm64`)
//!
//! Platform-specific tools take precedence when they match the current platform.
//!
//! # Tool Manifest
//!
//! Optional `<toolname>.json` sidecar files provide metadata:
//! - `name`: Tool name
//! - `description`: Human-readable description
//! - `parameters`: JSON Schema for tool arguments
//!
//! # Protocol
//!
//! Tools receive a JSON request on stdin and must respond with JSON on stdout:
//!
//! **Request:**
//! ```json
//! {
//!   "tool_call_id": "tc_abc123",
//!   "tool": "my_tool",
//!   "args": {"query": "hello"},
//!   "timeout_ms": 30000,
//!   "context": {
//!     "instance_id": "inst_xyz",
//!     "session_id": "sess_abc",
//!     "workspace": "/path/to/workspace"
//!   }
//! }
//! ```
//!
//! **Response:**
//! ```json
//! {
//!   "tool_call_id": "tc_abc123",
//!   "output": "result data",
//!   "error": null
//! }
//! ```
//!
//! **Exit codes:**
//! - 0: Success
//! - 1: Tool error (response contains error message)
//! - 2: Protocol error

pub mod discovery;
pub mod executor;
pub mod manifest;
pub mod protocol;

pub use discovery::{discover_tools, DiscoveredTool, ToolDiscovery};
pub use executor::{create_custom_tools, CustomTool};
pub use manifest::{parse_manifest, ToolManifest};
pub use protocol::{execute_tool, ExecutionContext, ToolRequest, ToolResponse};

use crate::tools::traits::Tool;
use std::path::Path;

/// Load custom tools from a directory
///
/// This is the main entry point for loading custom tools. It discovers
/// tools and creates CustomTool instances ready for use.
pub async fn load_custom_tools(tools_dir: impl AsRef<Path>) -> anyhow::Result<Vec<CustomTool>> {
    create_custom_tools(tools_dir).await
}

/// Load custom tools with filtering by name
///
/// Only loads tools whose names are in the allow list.
pub async fn load_custom_tools_filtered(
    tools_dir: impl AsRef<Path>,
    allowed: &[String],
) -> anyhow::Result<Vec<CustomTool>> {
    let all_tools = load_custom_tools(tools_dir).await?;
    let allowed_set: std::collections::HashSet<_> =
        allowed.iter().map(|s| s.to_lowercase()).collect();

    Ok(all_tools
        .into_iter()
        .filter(|tool| allowed_set.contains(&tool.name().to_lowercase()))
        .collect())
}

/// Load custom tools excluding certain names
///
/// Loads all tools except those in the exclude list.
pub async fn load_custom_tools_excluding(
    tools_dir: impl AsRef<Path>,
    excluded: &[String],
) -> anyhow::Result<Vec<CustomTool>> {
    let all_tools = load_custom_tools(tools_dir).await?;
    let excluded_set: std::collections::HashSet<_> =
        excluded.iter().map(|s| s.to_lowercase()).collect();

    Ok(all_tools
        .into_iter()
        .filter(|tool| !excluded_set.contains(&tool.name().to_lowercase()))
        .collect())
}

/// Get tool names from a directory without loading full tools
pub async fn list_custom_tool_names(tools_dir: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    let discovered = discover_tools(tools_dir).await?;
    Ok(discovered.into_keys().collect())
}

/// Check if a specific custom tool exists
pub async fn custom_tool_exists(
    tools_dir: impl AsRef<Path>,
    tool_name: &str,
) -> anyhow::Result<bool> {
    let discovered = discover_tools(tools_dir).await?;
    Ok(discovered.contains_key(&tool_name.to_lowercase()))
}

/// Result of loading custom tools
#[derive(Debug)]
pub struct CustomToolsLoadResult {
    /// Successfully loaded tools
    pub tools: Vec<CustomTool>,
    /// Tools that failed to load (name, error)
    pub failed: Vec<(String, String)>,
    /// Total discovered
    pub discovered_count: usize,
}

/// Load custom tools with detailed result
pub async fn load_custom_tools_detailed(tools_dir: impl AsRef<Path>) -> CustomToolsLoadResult {
    let mut tools = Vec::new();
    let failed = Vec::new();

    let discovered = match discover_tools(&tools_dir).await {
        Ok(d) => d,
        Err(e) => {
            return CustomToolsLoadResult {
                tools,
                failed: vec![("discovery".to_string(), e.to_string())],
                discovered_count: 0,
            }
        }
    };

    let discovered_count = discovered.len();

    for (name, info) in discovered {
        // Load manifest if exists
        let manifest = if let Some(manifest_path) = info.manifest_path {
            match ToolManifest::from_file(&manifest_path).await {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!("Failed to load manifest for '{}': {}", name, e);
                    None
                }
            }
        } else {
            None
        };

        tools.push(CustomTool::new(name, info.executable_path, manifest));
    }

    CustomToolsLoadResult {
        tools,
        failed,
        discovered_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_executable(path: &Path) -> anyhow::Result<()> {
        let mut file = std::fs::File::create(path)?;
        file.write_all(b"#!/bin/sh\necho test")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file.metadata()?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms)?;
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_load_custom_tools() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        std::fs::create_dir(&tools_dir).unwrap();

        // Create some tools
        create_executable(&tools_dir.join("tool1")).unwrap();
        create_executable(&tools_dir.join("tool2")).unwrap();

        let tools = load_custom_tools(&tools_dir).await.unwrap();
        assert_eq!(tools.len(), 2);
    }

    #[tokio::test]
    async fn test_list_custom_tool_names() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        std::fs::create_dir(&tools_dir).unwrap();

        create_executable(&tools_dir.join("alpha_tool")).unwrap();
        create_executable(&tools_dir.join("beta_tool")).unwrap();

        let names = list_custom_tool_names(&tools_dir).await.unwrap();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"alpha_tool".to_string()));
        assert!(names.contains(&"beta_tool".to_string()));
    }

    #[tokio::test]
    async fn test_custom_tool_exists() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        std::fs::create_dir(&tools_dir).unwrap();

        create_executable(&tools_dir.join("existing_tool")).unwrap();

        assert!(custom_tool_exists(&tools_dir, "existing_tool")
            .await
            .unwrap());
        assert!(!custom_tool_exists(&tools_dir, "nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_load_custom_tools_filtered() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        std::fs::create_dir(&tools_dir).unwrap();

        create_executable(&tools_dir.join("allowed")).unwrap();
        create_executable(&tools_dir.join("blocked")).unwrap();

        let tools = load_custom_tools_filtered(&tools_dir, &["allowed".to_string()])
            .await
            .unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "allowed");
    }
}
