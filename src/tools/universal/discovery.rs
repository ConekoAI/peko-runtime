//! Universal Tool Discovery
//!
//! SRP: This module ONLY handles discovering universal tools from directories.
//! No protocol logic, no execution.
//!
//! Universal Tools are installed system-wide in `{data_dir}/tools/` and are
//! enabled per-agent via the `tools.enabled` configuration.

use super::adapter::UniversalToolAdapter;
use crate::tools::traits::Tool;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, trace, warn};

/// Get the default system-wide Universal Tools directory
///
/// This is `{data_dir}/tools` where `data_dir` is the Pekobot data directory
/// (e.g., `~/.local/share/pekobot/tools` on Linux,
///  `~/Library/Application Support/pekobot/tools` on macOS,
///  `%APPDATA%/pekobot/tools` on Windows)
pub fn default_tools_dir() -> PathBuf {
    crate::common::paths::default_data_dir().join("tools")
}

/// Discovered tool info
#[derive(Debug, Clone)]
pub struct DiscoveredTool {
    pub name: String,
    pub executable: PathBuf,
    pub manifest: Option<PathBuf>,
}

/// Discover universal tools in a directory
///
/// Tools are organized in subdirectories, one per tool:
/// `{tools_dir}/calculator_tool/manifest.json`
/// `{tools_dir}/calculator_tool/calculator_tool.py`
pub async fn discover_universal_tools(
    dir: impl AsRef<Path>,
) -> Result<Vec<DiscoveredTool>> {
    let dir = dir.as_ref();
    let mut tools = Vec::new();

    if !dir.exists() {
        trace!("Tools directory does not exist: {:?}", dir);
        return Ok(tools);
    }

    let mut entries = tokio::fs::read_dir(dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden files and non-directories
        if name_str.starts_with('.') || !path.is_dir() {
            continue;
        }

        // Look for manifest.json in the tool directory
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            trace!("No manifest.json found in {}", path.display());
            continue;
        }

        // Try to read manifest to get the actual tool name
        let manifest_content = match tokio::fs::read_to_string(&manifest_path).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read manifest {}: {}", manifest_path.display(), e);
                continue;
            }
        };

        let manifest: super::Manifest = match serde_json::from_str(&manifest_content) {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to parse manifest {}: {}", manifest_path.display(), e);
                continue;
            }
        };

        let tool_name = manifest.name;

        // Look for executable - try various naming patterns
        let executable = 
            // Try {tool_name}.py
            if path.join(format!("{}.py", tool_name)).exists() {
                path.join(format!("{}.py", tool_name))
            } else if path.join(format!("{}.js", tool_name)).exists() {
                path.join(format!("{}.js", tool_name))
            } else if path.join(format!("{}.sh", tool_name)).exists() {
                path.join(format!("{}.sh", tool_name))
            } else if path.join(tool_name.clone()).exists() {
                // Try exact name (binary)
                path.join(tool_name.clone())
            } else {
                // Look for any executable file
                let mut found = None;
                let mut tool_entries = match tokio::fs::read_dir(&path).await {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                while let Some(tool_entry) = tool_entries.next_entry().await.ok().flatten() {
                    let tool_path = tool_entry.path();
                    if tool_path.is_file() && is_executable(&tool_path).await {
                        // Skip manifest.json
                        if tool_path.file_name() != Some(std::ffi::OsStr::new("manifest.json")) {
                            found = Some(tool_path);
                            break;
                        }
                    }
                }
                match found {
                    Some(p) => p,
                    None => {
                        trace!("No executable found for tool '{}' in {}", tool_name, path.display());
                        continue;
                    }
                }
            };

        debug!("Discovered tool '{}' at {}", tool_name, path.display());
        tools.push(DiscoveredTool {
            name: tool_name,
            executable,
            manifest: Some(manifest_path),
        });
    }

    Ok(tools)
}

/// Load discovered tools as adapters
pub async fn load_universal_tools(dir: impl AsRef<Path>) -> Result<Vec<UniversalToolAdapter>> {
    let discovered = discover_universal_tools(dir).await?;
    let mut adapters = Vec::new();

    for tool in discovered {
        match tool.manifest {
            Some(manifest_path) => {
                match UniversalToolAdapter::from_manifest(&manifest_path, &tool.executable).await {
                    Ok(adapter) => {
                        debug!("Loaded universal tool: {}", adapter.name());
                        adapters.push(adapter);
                    }
                    Err(e) => {
                        warn!("Failed to load tool '{}': {}", tool.name, e);
                    }
                }
            }
            None => {
                // Create with auto-generated manifest
                warn!(
                    "Tool '{}' has no manifest, skipping (manifest required for universal tools)",
                    tool.name
                );
            }
        }
    }

    Ok(adapters)
}

/// Check if a file is executable (platform-specific)
#[cfg(unix)]
async fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    
    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            let perms = meta.permissions();
            perms.mode() & 0o111 != 0
        }
        Err(_) => false,
    }
}

#[cfg(windows)]
async fn is_executable(path: &Path) -> bool {
    // On Windows, check for known executable extensions
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    
    match ext.as_deref() {
        Some("exe") | Some("bat") | Some("cmd") | Some("ps1") => true,
        Some("py") | Some("js") | Some("sh") => {
            // Script files - check if they have shebang or are associated
            has_shebang(path).await
        }
        // Extensionless files - check if they have shebang (Unix compat)
        None => has_shebang(path).await,
        _ => false,
    }
}

/// Check if file has a shebang line
async fn has_shebang(path: &Path) -> bool {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => content.starts_with("#!"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_executable(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        tokio::fs::write(&path, content).await.unwrap();
        
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&path).await.unwrap().permissions();
            perms.set_mode(0o755);
            tokio::fs::set_permissions(&path, perms).await.unwrap();
        }
        
        path
    }

    #[tokio::test]
    async fn test_discover_with_manifest() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();

        // Create executable
        create_executable(dir, "my_tool", "#!/bin/sh\necho hello").await;

        // Create manifest
        let manifest = r#"{
            "name": "my_tool",
            "description": "A test tool",
            "parameters": {"type": "object"}
        }"#;
        tokio::fs::write(dir.join("my_tool.json"), manifest).await.unwrap();

        let tools = discover_universal_tools(dir).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "my_tool");
        assert!(tools[0].manifest.is_some());
    }

    #[tokio::test]
    async fn test_discover_python_extension() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();

        // Create Python script
        create_executable(dir, "query_tool.py", "#!/usr/bin/env python3\nprint('hi')").await;

        // Create manifest (without extension)
        let manifest = r#"{
            "name": "query_tool",
            "description": "Query tool",
            "parameters": {"type": "object"}
        }"#;
        tokio::fs::write(dir.join("query_tool.json"), manifest).await.unwrap();

        let tools = discover_universal_tools(dir).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "query_tool");
    }

    #[tokio::test]
    async fn test_discover_empty_dir() {
        let temp = TempDir::new().unwrap();
        let tools = discover_universal_tools(temp.path()).await.unwrap();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_discover_nonexistent_dir() {
        let tools = discover_universal_tools("/nonexistent/path/12345").await.unwrap();
        assert!(tools.is_empty());
    }
}
