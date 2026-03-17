//! Custom tool discovery
//!
//! Discovers executable tools from the `tools/` directory in an agent image.
//! Supports platform-specific executables and optional `.json` manifest sidecars.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, trace};

/// Information about a discovered custom tool
#[derive(Debug, Clone)]
pub struct DiscoveredTool {
    /// Tool name (derived from filename)
    pub name: String,
    /// Full path to the executable
    pub executable_path: PathBuf,
    /// Optional path to the manifest sidecar
    pub manifest_path: Option<PathBuf>,
    /// Whether this is a platform-specific binary
    pub is_platform_specific: bool,
}

/// Discovers custom tools in a `tools/` directory
pub struct ToolDiscovery {
    /// The tools directory path
    tools_dir: PathBuf,
    /// Platform suffix for this system (e.g., "linux-x64")
    platform_suffix: String,
}

impl ToolDiscovery {
    /// Create a new tool discovery for the given tools directory
    pub fn new(tools_dir: impl Into<PathBuf>) -> Self {
        Self {
            tools_dir: tools_dir.into(),
            platform_suffix: detect_platform_suffix(),
        }
    }

    /// Create with custom platform suffix (for testing)
    #[cfg(test)]
    pub fn with_platform(tools_dir: impl Into<PathBuf>, platform: impl Into<String>) -> Self {
        Self {
            tools_dir: tools_dir.into(),
            platform_suffix: platform.into(),
        }
    }

    /// Discover all tools in the tools directory
    ///
    /// Returns a map of tool name -> discovered tool info
    pub async fn discover(&self) -> anyhow::Result<HashMap<String, DiscoveredTool>> {
        let mut tools = HashMap::new();

        if !self.tools_dir.exists() {
            trace!("Tools directory does not exist: {:?}", self.tools_dir);
            return Ok(tools);
        }

        let mut entries = tokio::fs::read_dir(&self.tools_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;

            // Skip directories (except for special cases)
            if file_type.is_dir() {
                trace!("Skipping directory: {:?}", path);
                continue;
            }

            // Skip hidden files
            if let Some(name) = path.file_name() {
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
            }

            // Skip manifest files (handled with their executables)
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                continue;
            }

            // Skip source files (Python, JS, etc. - they need special handling)
            // We process them as platform-specific tools
            if let Some(tool) = self.process_file(&path).await? {
                let name = tool.name.clone();

                // If we already have this tool, prefer platform-specific version
                if let Some(existing) = tools.get(&name) {
                    if tool.is_platform_specific && !existing.is_platform_specific {
                        debug!(
                            "Replacing generic tool '{}' with platform-specific version",
                            name
                        );
                        tools.insert(name, tool);
                    } else {
                        trace!("Skipping duplicate tool: {}", name);
                    }
                } else {
                    tools.insert(name, tool);
                }
            }
        }

        debug!("Discovered {} tools in {:?}", tools.len(), self.tools_dir);
        Ok(tools)
    }

    /// Process a single file to determine if it's a valid tool
    async fn process_file(&self, path: &Path) -> anyhow::Result<Option<DiscoveredTool>> {
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid file name"))?
            .to_string_lossy();

        // Check if this is a platform-specific binary
        // Supports formats: "tool-linux-x64" or "tool-darwin-arm64"
        let (base_name, is_platform_specific) =
            if let Some(stem) = path.file_stem().map(|s| s.to_string_lossy()) {
                // Check for platform suffix - look for patterns like "name-os-arch"
                if let Some(first_dash) = stem.rfind('-') {
                    if let Some(second_dash) = stem[..first_dash].rfind('-') {
                        // Found pattern like "name-os-arch" (two dashes in stem)
                        let suffix = &stem[second_dash + 1..];
                        let name_part = &stem[..second_dash];
                        // It's platform-specific if the suffix looks like a platform (contains os-arch patterns)
                        let is_platform = suffix.contains("linux")
                            || suffix.contains("darwin")
                            || suffix.contains("windows");
                        (name_part.to_string(), is_platform)
                    } else {
                        // Only one dash - could be "name-platform" like "tool-darwin-arm64"
                        // where platform itself contains a dash. Let's check if suffix looks like a platform.
                        let suffix = &stem[first_dash + 1..];
                        // Platform suffixes typically contain "linux", "darwin", "windows", "x64", or "arm64"
                        let looks_like_platform = suffix.contains("linux")
                            || suffix.contains("darwin")
                            || suffix.contains("windows")
                            || suffix.contains("x64")
                            || suffix.contains("arm64");

                        if looks_like_platform {
                            let name_part = &stem[..first_dash];
                            (name_part.to_string(), true)
                        } else {
                            // Not a platform suffix, just a regular name with dash
                            (stem.to_string(), false)
                        }
                    }
                } else {
                    (stem.to_string(), false)
                }
            } else {
                (file_name.to_string(), false)
            };

        // Normalize tool name: lowercase, underscores
        let tool_name = normalize_tool_name(&base_name);

        // Check for manifest sidecar
        let manifest_path = self.tools_dir.join(format!("{}.json", base_name));
        let manifest_exists = manifest_path.exists();

        // If this is a platform-specific tool but doesn't match our platform, skip it
        if is_platform_specific {
            let stem = path.file_stem().unwrap().to_string_lossy();
            // Extract platform suffix from stem (e.g., "my_tool-linux-x64" -> "linux-x64")
            // Handle both "name-os-arch" (two dashes) and "name-platform" (one dash with platform like "darwin-arm64")
            let file_platform = if let Some(first_dash) = stem.rfind('-') {
                if let Some(second_dash) = stem[..first_dash].rfind('-') {
                    // Two dashes: pattern is "name-os-arch"
                    Some(&stem[second_dash + 1..])
                } else {
                    // One dash: pattern is "name-platform"
                    Some(&stem[first_dash + 1..])
                }
            } else {
                None
            };

            if file_platform != Some(&self.platform_suffix) {
                trace!(
                    "Skipping tool '{}' - wrong platform (expected {}, got {:?})",
                    tool_name,
                    self.platform_suffix,
                    file_platform
                );
                return Ok(None);
            }
        }

        // Verify executable permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = tokio::fs::metadata(path).await?;
            let permissions = metadata.permissions();
            if permissions.mode() & 0o111 == 0 {
                warn!("Tool '{}' is not executable: {:?}", tool_name, path);
                // Still include it - might be a script handled by shebang
            }
        }

        trace!(
            "Discovered tool '{}' at {:?} (platform_specific: {}, manifest: {})",
            tool_name,
            path,
            is_platform_specific,
            manifest_exists
        );

        Ok(Some(DiscoveredTool {
            name: tool_name,
            executable_path: path.to_path_buf(),
            manifest_path: if manifest_exists {
                Some(manifest_path)
            } else {
                None
            },
            is_platform_specific,
        }))
    }

    /// Get the tools directory path
    pub fn tools_dir(&self) -> &Path {
        &self.tools_dir
    }
}

/// Detect the current platform suffix
fn detect_platform_suffix() -> String {
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };

    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    format!("{}-{}", os, arch)
}

/// Normalize a tool name for consistent lookup
///
/// Rules:
/// - Convert to lowercase
/// - Replace hyphens with underscores
/// - Prefix with underscore if starts with a digit
fn normalize_tool_name(name: &str) -> String {
    let normalized = name.to_lowercase().replace('-', "_");

    // If the name starts with a digit, prefix with underscore
    if normalized
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        format!("_{}", normalized)
    } else {
        normalized
    }
}

/// Discover tools from a tools directory (convenience function)
pub async fn discover_tools(
    tools_dir: impl AsRef<Path>,
) -> anyhow::Result<HashMap<String, DiscoveredTool>> {
    let discovery = ToolDiscovery::new(tools_dir.as_ref());
    discovery.discover().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    async fn create_test_tool_dir() -> anyhow::Result<TempDir> {
        let temp_dir = TempDir::new()?;
        let tools_dir = temp_dir.path().join("tools");
        tokio::fs::create_dir_all(&tools_dir).await?;
        Ok(temp_dir)
    }

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
    async fn test_discover_empty_directory() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_discover_missing_directory() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path().join("nonexistent");

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_discover_simple_tool() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create a simple tool
        let tool_path = tools_dir.join("my_tool");
        create_executable(&tool_path).unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert_eq!(tools.len(), 1);
        assert!(tools.contains_key("my_tool"));

        let tool = &tools["my_tool"];
        assert_eq!(tool.name, "my_tool");
        assert!(!tool.is_platform_specific);
    }

    #[tokio::test]
    async fn test_discover_with_manifest() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create tool and manifest
        let tool_path = tools_dir.join("my_tool");
        create_executable(&tool_path).unwrap();

        let manifest_path = tools_dir.join("my_tool.json");
        std::fs::write(&manifest_path, r#"{"name": "my_tool"}"#).unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert_eq!(tools.len(), 1);
        let tool = &tools["my_tool"];
        assert!(tool.manifest_path.is_some());
    }

    #[tokio::test]
    async fn test_platform_specific_tools() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create platform-specific tool
        let tool_linux = tools_dir.join("my_tool-linux-x64");
        create_executable(&tool_linux).unwrap();

        // Create generic tool
        let tool_generic = tools_dir.join("my_tool");
        create_executable(&tool_generic).unwrap();

        let discovery = ToolDiscovery::with_platform(&tools_dir, "linux-x64");
        let tools = discovery.discover().await.unwrap();

        // Should prefer platform-specific version
        assert_eq!(tools.len(), 1);
        assert!(tools["my_tool"].is_platform_specific);
    }

    #[tokio::test]
    async fn test_skip_wrong_platform() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create macOS-specific tool
        let tool_macos = tools_dir.join("my_tool-darwin-arm64");
        create_executable(&tool_macos).unwrap();

        // Discover as Linux
        let discovery = ToolDiscovery::with_platform(&tools_dir, "linux-x64");
        let tools = discovery.discover().await.unwrap();

        // Should not include macOS tool
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_normalize_names() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create tool with hyphen
        let tool_path = tools_dir.join("my-tool");
        create_executable(&tool_path).unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        // Should normalize to underscore
        assert!(tools.contains_key("my_tool"));
    }

    #[tokio::test]
    async fn test_discover_multiple_tools() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create multiple tools
        create_executable(&tools_dir.join("tool_a")).unwrap();
        create_executable(&tools_dir.join("tool_b")).unwrap();
        create_executable(&tools_dir.join("tool_c")).unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert_eq!(tools.len(), 3);
        assert!(tools.contains_key("tool_a"));
        assert!(tools.contains_key("tool_b"));
        assert!(tools.contains_key("tool_c"));
    }

    #[tokio::test]
    async fn test_skip_hidden_files() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create regular tool
        create_executable(&tools_dir.join("visible_tool")).unwrap();

        // Create hidden file (should be skipped)
        std::fs::write(&tools_dir.join(".hidden_tool"), "secret").unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert_eq!(tools.len(), 1);
        assert!(tools.contains_key("visible_tool"));
        assert!(!tools.contains_key("hidden_tool"));
        assert!(!tools.contains_key(".hidden_tool"));
    }

    #[tokio::test]
    async fn test_skip_directories() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create regular tool
        create_executable(&tools_dir.join("valid_tool")).unwrap();

        // Create subdirectory (should be skipped)
        tokio::fs::create_dir(&tools_dir.join("subdir"))
            .await
            .unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert_eq!(tools.len(), 1);
        assert!(tools.contains_key("valid_tool"));
    }

    #[tokio::test]
    async fn test_manifest_without_tool() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create manifest without corresponding tool
        std::fs::write(&tools_dir.join("orphan.json"), r#"{"name": "orphan"}"#).unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        // Should not include orphaned manifest
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_tool_name_extraction() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create tools with various naming patterns
        create_executable(&tools_dir.join("simple")).unwrap();
        create_executable(&tools_dir.join("with_underscore")).unwrap();
        create_executable(&tools_dir.join("with-hyphen")).unwrap();
        create_executable(&tools_dir.join("123numeric")).unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        assert_eq!(tools.len(), 4);
        assert!(tools.contains_key("simple"));
        assert!(tools.contains_key("with_underscore"));
        assert!(tools.contains_key("with_underscore")); // hyphen normalized
        assert!(tools.contains_key("_123numeric")); // leading digit becomes underscore
    }

    #[test]
    fn test_detect_platform_suffix() {
        let suffix = detect_platform_suffix();

        // Should contain OS and arch
        assert!(suffix.contains('-'));

        // Check known patterns
        let valid_os = ["linux", "darwin", "windows", "unknown"];
        let valid_arch = ["x64", "arm64", "unknown"];

        let parts: Vec<_> = suffix.split('-').collect();
        assert_eq!(parts.len(), 2);
        assert!(valid_os.contains(&parts[0]));
        assert!(valid_arch.contains(&parts[1]));
    }

    #[tokio::test]
    async fn test_discover_tools_convenience_function() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        create_executable(&tools_dir.join("test_tool")).unwrap();

        let tools = discover_tools(&tools_dir).await.unwrap();

        assert_eq!(tools.len(), 1);
        assert!(tools.contains_key("test_tool"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_tool_with_script_extension() {
        let temp_dir = create_test_tool_dir().await.unwrap();
        let tools_dir = temp_dir.path().join("tools");

        // Create script files (should be discovered)
        std::fs::write(&tools_dir.join("script.py"), "#!/usr/bin/env python3").unwrap();
        std::fs::write(&tools_dir.join("script.sh"), "#!/bin/bash").unwrap();
        std::fs::write(&tools_dir.join("script.js"), "#!/usr/bin/env node").unwrap();

        let discovery = ToolDiscovery::new(&tools_dir);
        let tools = discovery.discover().await.unwrap();

        // All should be discovered (they're files, not directories)
        assert_eq!(tools.len(), 3);
        assert!(tools.contains_key("script_py"));
        assert!(tools.contains_key("script_sh"));
        assert!(tools.contains_key("script_js"));
    }
}
