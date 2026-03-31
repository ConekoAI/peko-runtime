//! Universal Tool Discovery
//!
//! SRP: This module ONLY handles discovering universal tools from directories.
//! No protocol logic, no execution.

use super::adapter::UniversalToolAdapter;
use crate::tools::traits::Tool;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, trace, warn};

/// Discovered tool info
#[derive(Debug, Clone)]
pub struct DiscoveredTool {
    pub name: String,
    pub executable: PathBuf,
    pub manifest: Option<PathBuf>,
}

/// Discover universal tools in a directory
///
/// Looks for:
/// - `*.json` manifest files with corresponding executables
/// - Executables without manifests (will use defaults)
pub async fn discover_universal_tools(
    dir: impl AsRef<Path>,
) -> Result<Vec<DiscoveredTool>> {
    let dir = dir.as_ref();
    let mut tools = Vec::new();
    let mut found_executables: HashMap<String, PathBuf> = HashMap::new();
    let mut found_manifests: HashMap<String, PathBuf> = HashMap::new();

    if !dir.exists() {
        trace!("Tools directory does not exist: {:?}", dir);
        return Ok(tools);
    }

    let mut entries = tokio::fs::read_dir(dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden files
        if name_str.starts_with('.') {
            continue;
        }

        if path.is_file() {
            if name_str.ends_with(".json") {
                // Manifest file
                let base_name = name_str.trim_end_matches(".json").to_string();
                trace!("Found manifest: {} -> {}", base_name, name_str);
                found_manifests.insert(base_name, path.clone());
            } else if is_executable(&path).await {
                // Executable file
                let base_name = name_str.to_string();
                trace!("Found executable: {}", base_name);
                found_executables.insert(base_name, path.clone());
            }
        }
    }

    // Match manifests to executables
    for (name, manifest_path) in &found_manifests {
        // Look for executable with same base name
        let executable = found_executables
            .get(name)
            .or_else(|| {
                // Try with common extensions
                found_executables.get(&format!("{}.py", name))
                    .or_else(|| found_executables.get(&format!("{}.js", name)))
                    .or_else(|| found_executables.get(&format!("{}.sh", name)))
            })
            .cloned();

        if let Some(exec) = executable {
            debug!("Matched tool '{}' with manifest", name);
            tools.push(DiscoveredTool {
                name: name.clone(),
                executable: exec,
                manifest: Some(manifest_path.clone()),
            });
        } else {
            trace!("Manifest '{}' has no matching executable", name);
        }
    }

    // Add executables without manifests
    for (name, exec) in found_executables {
        if !found_manifests.contains_key(&name) {
            // Check if this is an extension variant (e.g., foo.py for foo.json)
            let base = name
                .trim_end_matches(".py")
                .trim_end_matches(".js")
                .trim_end_matches(".sh");
            
            if !found_manifests.contains_key(base) {
                debug!("Found standalone executable: {}", name);
                tools.push(DiscoveredTool {
                    name: name.clone(),
                    executable: exec,
                    manifest: None,
                });
            }
        }
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
