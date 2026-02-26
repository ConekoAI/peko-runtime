//! Pekohub Tool Registry
//!
//! Multi-backend tool management for Pekobot.
//! Supports: Pekohub (cloud/self-hosted), local filesystem, source builds, embedded packages.
#![allow(dead_code)]

mod remote;
pub use remote::*;

mod unified;
pub use unified::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Tool manifest (TOML format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    /// Tool metadata
    pub tool: ToolMetadata,
    /// Capabilities this tool provides
    pub capabilities: ToolCapabilities,
    /// Binary download URLs by platform
    pub binaries: Option<BinaryUrls>,
    /// Dependencies on other tools
    pub dependencies: Option<Vec<String>>,
    /// Installation script
    pub install: Option<InstallScript>,
    /// Security settings
    pub security: Option<SecurityConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// Unique tool name
    pub name: String,
    /// Semantic version
    pub version: String,
    /// Human-readable description
    pub description: String,
    /// Author/maintainer
    pub author: Option<String>,
    /// License (SPDX identifier)
    pub license: Option<String>,
    /// Homepage URL
    pub homepage: Option<String>,
    /// Repository URL
    pub repository: Option<String>,
    /// Tool category
    pub category: Option<String>,
    /// Keywords for search
    pub keywords: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapabilities {
    /// List of capabilities this tool provides
    pub provides: Vec<String>,
    /// Required permissions
    pub permissions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryUrls {
    /// Linux `x86_64` binary URL
    pub linux_x64: Option<String>,
    /// Linux ARM64 binary URL
    pub linux_arm64: Option<String>,
    /// macOS `x86_64` binary URL
    pub macos_x64: Option<String>,
    /// macOS ARM64 (Apple Silicon) binary URL
    pub macos_arm64: Option<String>,
    /// Windows `x86_64` binary URL
    pub windows_x64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallScript {
    /// Shell command to run (Unix)
    pub unix: Option<String>,
    /// `PowerShell` command to run (Windows)
    pub windows: Option<String>,
    /// Environment variables to set
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Ed25519 signature of the binary
    pub signature: Option<String>,
    /// SHA256 checksum of the binary
    pub checksum: Option<String>,
    /// Sandboxing level
    pub sandbox: Option<SandboxLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxLevel {
    /// No sandboxing (full system access)
    None,
    /// Restrict filesystem access
    Filesystem,
    /// Restrict network access
    Network,
    /// Full sandbox (filesystem + network)
    Full,
}

/// Installed tool information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledTool {
    /// Tool manifest
    pub manifest: ToolManifest,
    /// Installation path
    pub install_path: PathBuf,
    /// Installation time
    pub installed_at: chrono::DateTime<chrono::Utc>,
    /// Last used time
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
    /// Is currently active
    pub is_active: bool,
}

/// Tool registry configuration
#[derive(Debug, Clone)]
pub struct ToolRegistryConfig {
    /// Local cache directory for tools
    pub cache_dir: PathBuf,
    /// Registry URL (for future remote registry)
    pub registry_url: Option<String>,
    /// Auto-update enabled
    pub auto_update: bool,
    /// Allow unsigned tools
    pub allow_unsigned: bool,
}

impl Default for ToolRegistryConfig {
    fn default() -> Self {
        let cache_dir = dirs::cache_dir().map_or_else(
            || PathBuf::from("/tmp/pekobot-tools"),
            |d| d.join("pekobot").join("tools"),
        );

        Self {
            cache_dir,
            registry_url: None,
            auto_update: false,
            allow_unsigned: false,
        }
    }
}

/// Local tool registry
pub struct ToolRegistry {
    config: ToolRegistryConfig,
    installed_tools: HashMap<String, InstalledTool>,
}

impl ToolRegistry {
    /// Create new tool registry
    pub fn new(config: ToolRegistryConfig) -> anyhow::Result<Self> {
        // Ensure cache directory exists
        std::fs::create_dir_all(&config.cache_dir)?;

        let mut registry = Self {
            config,
            installed_tools: HashMap::new(),
        };

        // Load existing installed tools
        registry.load_installed_tools()?;

        Ok(registry)
    }

    /// Load tool from local filesystem
    pub fn load_tool_from_path(&self, manifest_path: &Path) -> anyhow::Result<ToolManifest> {
        let content = std::fs::read_to_string(manifest_path)?;
        let manifest: ToolManifest = toml::from_str(&content)?;

        // Validate manifest
        self.validate_manifest(&manifest)?;

        Ok(manifest)
    }

    /// Install tool from local manifest
    pub async fn install_local_tool(
        &mut self,
        manifest_path: &Path,
    ) -> anyhow::Result<InstalledTool> {
        let manifest = self.load_tool_from_path(manifest_path)?;
        let tool_name = manifest.tool.name.clone();
        let tool_version = manifest.tool.version.clone();

        // Check if already installed
        if let Some(existing) = self.installed_tools.get(&tool_name) {
            if existing.manifest.tool.version == tool_version {
                anyhow::bail!("Tool {tool_name}@{tool_version} is already installed");
            }
            println!(
                "Upgrading {} from {} to {}",
                tool_name, existing.manifest.tool.version, tool_version
            );
        }

        // Create install directory
        let install_dir = self.config.cache_dir.join(&tool_name);
        std::fs::create_dir_all(&install_dir)?;

        // Copy manifest
        let manifest_dest = install_dir.join("tool.toml");
        std::fs::copy(manifest_path, &manifest_dest)?;

        // Handle installation
        if let Some(ref binaries) = manifest.binaries {
            self.download_binaries(&manifest, binaries, &install_dir)
                .await?;
        }

        if let Some(ref install) = manifest.install {
            self.run_install_script(install, &install_dir)?;
        }

        let installed = InstalledTool {
            manifest,
            install_path: install_dir,
            installed_at: chrono::Utc::now(),
            last_used: None,
            is_active: true,
        };

        self.installed_tools
            .insert(tool_name.clone(), installed.clone());
        self.save_installed_tools()?;

        println!("✅ Installed {tool_name}@{tool_version}");

        Ok(installed)
    }

    /// List all installed tools
    #[must_use]
    pub fn list_installed(&self) -> Vec<&InstalledTool> {
        self.installed_tools.values().collect()
    }

    /// Get installed tool by name
    #[must_use]
    pub fn get_tool(&self, name: &str) -> Option<&InstalledTool> {
        self.installed_tools.get(name)
    }

    /// Uninstall a tool
    pub fn uninstall_tool(&mut self, name: &str) -> anyhow::Result<()> {
        let tool = self
            .installed_tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Tool {name} not found"))?;

        // Remove installation directory
        std::fs::remove_dir_all(&tool.install_path)?;

        // Remove from registry
        self.installed_tools.remove(name);
        self.save_installed_tools()?;

        println!("✅ Uninstalled {name}");

        Ok(())
    }

    /// Find tools by capability
    #[must_use]
    pub fn find_by_capability(&self, capability: &str) -> Vec<&InstalledTool> {
        self.installed_tools
            .values()
            .filter(|tool| {
                tool.manifest
                    .capabilities
                    .provides
                    .contains(&capability.to_string())
            })
            .collect()
    }

    /// Load tool manifests from directory
    pub fn scan_tools_directory(&self, dir: &Path) -> anyhow::Result<Vec<ToolManifest>> {
        let mut manifests = Vec::new();

        if !dir.exists() {
            return Ok(manifests);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "toml") {
                match self.load_tool_from_path(&path) {
                    Ok(manifest) => manifests.push(manifest),
                    Err(e) => eprintln!("Failed to load {path:?}: {e}"),
                }
            }
        }

        Ok(manifests)
    }

    /// Validate tool manifest
    fn validate_manifest(&self, manifest: &ToolManifest) -> anyhow::Result<()> {
        // Check required fields
        if manifest.tool.name.is_empty() {
            anyhow::bail!("Tool name is required");
        }

        if manifest.tool.version.is_empty() {
            anyhow::bail!("Tool version is required");
        }

        // Validate version format (basic check)
        if !manifest.tool.version.contains('.') {
            anyhow::bail!("Invalid version format: {}", manifest.tool.version);
        }

        // Check for capabilities
        if manifest.capabilities.provides.is_empty() {
            anyhow::bail!("Tool must provide at least one capability");
        }

        Ok(())
    }

    /// Load installed tools from cache
    fn load_installed_tools(&mut self) -> anyhow::Result<()> {
        let state_file = self.config.cache_dir.join("installed.json");

        if !state_file.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(state_file)?;
        let tools: Vec<InstalledTool> = serde_json::from_str(&content)?;

        for tool in tools {
            self.installed_tools
                .insert(tool.manifest.tool.name.clone(), tool);
        }

        Ok(())
    }

    /// Save installed tools to cache
    fn save_installed_tools(&self) -> anyhow::Result<()> {
        let state_file = self.config.cache_dir.join("installed.json");
        let tools: Vec<&InstalledTool> = self.installed_tools.values().collect();
        let content = serde_json::to_string_pretty(&tools)?;
        std::fs::write(state_file, content)?;
        Ok(())
    }

    /// Download binaries (placeholder for Phase 2)
    async fn download_binaries(
        &self,
        _manifest: &ToolManifest,
        _binaries: &BinaryUrls,
        _install_dir: &Path,
    ) -> anyhow::Result<()> {
        // Phase 1: Local only - no downloading
        // Phase 2: Will implement HTTP download with signature verification
        Ok(())
    }

    /// Run installation script
    fn run_install_script(
        &self,
        script: &InstallScript,
        _install_dir: &Path,
    ) -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            if let Some(ref unix_cmd) = script.unix {
                println!("Running install script: {unix_cmd}");
                // Would execute: std::process::Command::new("sh").arg("-c").arg(unix_cmd)
            }
        }

        #[cfg(windows)]
        {
            if let Some(ref win_cmd) = script.windows {
                println!("Running install script: {}", win_cmd);
                // Would execute: std::process::Command::new("powershell").arg(win_cmd)
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_manifest_parsing() {
        let toml = r#"
[tool]
name = "calendar"
version = "1.2.0"
description = "Google Calendar and Outlook integration"

[capabilities]
provides = ["scheduling.calendar_read", "scheduling.calendar_write"]

[binaries]
linux_x64 = "https://pekohub.io/tools/calendar/1.2.0/linux_x64.so"
"#;

        let manifest: ToolManifest = toml::from_str(toml).unwrap();
        assert_eq!(manifest.tool.name, "calendar");
        assert_eq!(manifest.tool.version, "1.2.0");
        assert!(manifest
            .capabilities
            .provides
            .contains(&"scheduling.calendar_read".to_string()));
    }

    #[test]
    fn test_default_config() {
        let config = ToolRegistryConfig::default();
        assert!(!config.cache_dir.as_os_str().is_empty());
    }
}
