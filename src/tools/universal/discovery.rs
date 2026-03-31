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

    async fn create_tool_dir(base_dir: &Path, tool_name: &str, manifest: &str, executable_content: &str) -> PathBuf {
        // Create subdirectory for tool
        let tool_dir = base_dir.join(tool_name);
        tokio::fs::create_dir(&tool_dir).await.unwrap();
        
        // Write manifest.json
        tokio::fs::write(tool_dir.join("manifest.json"), manifest).await.unwrap();
        
        // Write executable
        let exe_name = format!("{}.py", tool_name);
        let exe_path = tool_dir.join(&exe_name);
        tokio::fs::write(&exe_path, executable_content).await.unwrap();
        
        exe_path
    }

    #[tokio::test]
    async fn test_discover_with_manifest() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();

        // Create tool in subdirectory
        let manifest = r#"{
            "name": "my_tool",
            "description": "A test tool",
            "parameters": {"type": "object"}
        }"#;
        create_tool_dir(dir, "my_tool", manifest, "#!/usr/bin/env python3\nprint('hello')").await;

        let tools = discover_universal_tools(dir).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "my_tool");
        assert!(tools[0].manifest.is_some());
    }

    #[tokio::test]
    async fn test_discover_multiple_tools() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();

        // Create first tool
        let manifest1 = r#"{
            "name": "query_tool",
            "description": "Query tool",
            "parameters": {"type": "object"}
        }"#;
        create_tool_dir(dir, "query_tool", manifest1, "#!/usr/bin/env python3\nprint('query')").await;

        // Create second tool
        let manifest2 = r#"{
            "name": "calc_tool",
            "description": "Calculator tool",
            "parameters": {"type": "object"}
        }"#;
        create_tool_dir(dir, "calc_tool", manifest2, "#!/usr/bin/env python3\nprint('calc')").await;

        let tools = discover_universal_tools(dir).await.unwrap();
        assert_eq!(tools.len(), 2);
        
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"query_tool"));
        assert!(names.contains(&"calc_tool"));
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

    #[tokio::test]
    async fn test_discover_skips_hidden_dirs() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();

        // Create hidden directory
        let hidden_dir = dir.join(".hidden_tool");
        tokio::fs::create_dir(&hidden_dir).await.unwrap();
        let manifest = r#"{
            "name": "hidden_tool",
            "description": "Hidden tool",
            "parameters": {"type": "object"}
        }"#;
        tokio::fs::write(hidden_dir.join("manifest.json"), manifest).await.unwrap();

        let tools = discover_universal_tools(dir).await.unwrap();
        assert!(tools.is_empty()); // Hidden dirs should be skipped
    }

    #[tokio::test]
    async fn test_discover_skips_dirs_without_manifest() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();

        // Create directory without manifest
        let tool_dir = dir.join("incomplete_tool");
        tokio::fs::create_dir(&tool_dir).await.unwrap();
        tokio::fs::write(tool_dir.join("tool.py"), "#!/usr/bin/env python3").await.unwrap();

        let tools = discover_universal_tools(dir).await.unwrap();
        assert!(tools.is_empty()); // Dirs without manifest should be skipped
    }
}
