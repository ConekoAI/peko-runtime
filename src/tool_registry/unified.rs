//! Decentralized Tool Registry — Multi-Backend Support
//!
//! Supports:
//! - Pekohub (HTTP API) — default, cloud-hosted
//! - Self-hosted Pekohub — user's own instance
//! - Local filesystem — ~/.local/share/pekobot/tools/
//! - Local builds — cargo build from source
//! - Embedded in .agent packages

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Registry backend types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RegistryBackend {
    /// Pekohub HTTP API (our cloud instance or self-hosted)
    Pekohub {
        /// Base URL (e.g., <https://tools.coneko.ai> or <http://localhost:8787>)
        url: String,
        /// Optional API key for private registries
        api_key: Option<String>,
    },
    /// Local filesystem registry
    Local {
        /// Path to registry directory
        path: PathBuf,
    },
    /// Build from source on-demand
    Source {
        /// Path to tool source directories
        source_path: PathBuf,
        /// Build cache directory
        build_cache: PathBuf,
    },
    /// Embedded in agent package (read-only)
    Embedded {
        /// Path to extracted package
        package_path: PathBuf,
    },
}

impl Default for RegistryBackend {
    fn default() -> Self {
        // Default to public Pekohub with anonymous access
        RegistryBackend::Pekohub {
            url: "https://tools.coneko.ai".to_string(),
            api_key: None,
        }
    }
}

impl RegistryBackend {
    /// Create local-only registry (no cloud)
    #[must_use] 
    pub fn local_only() -> Self {
        RegistryBackend::Local {
            path: dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("pekobot/tools"),
        }
    }

    /// Create self-hosted Pekohub registry
    pub fn self_hosted(url: impl Into<String>) -> Self {
        RegistryBackend::Pekohub {
            url: url.into(),
            api_key: None,
        }
    }

    /// Get display name for this backend
    #[must_use] 
    pub fn display_name(&self) -> String {
        match self {
            RegistryBackend::Pekohub { url, .. } => {
                if url.contains("coneko.ai") {
                    "Pekohub (Official)".to_string()
                } else {
                    format!("Pekohub ({url})")
                }
            }
            RegistryBackend::Local { path } => format!("Local ({})", path.display()),
            RegistryBackend::Source { .. } => "Source (Build on-demand)".to_string(),
            RegistryBackend::Embedded { .. } => "Embedded (Agent Package)".to_string(),
        }
    }
}

/// Multi-registry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiRegistryConfig {
    /// Primary registry (checked first)
    pub primary: RegistryBackend,
    /// Fallback registries (checked in order)
    pub fallbacks: Vec<RegistryBackend>,
    /// Cache directory for downloaded tools
    pub cache_dir: PathBuf,
    /// Whether to allow building from source
    pub allow_source_builds: bool,
    /// Timeout for network operations
    pub timeout_secs: u64,
}

impl Default for MultiRegistryConfig {
    fn default() -> Self {
        Self {
            primary: RegistryBackend::default(),
            fallbacks: vec![
                // Fallback to local filesystem
                RegistryBackend::local_only(),
            ],
            cache_dir: dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from(".cache"))
                .join("pekobot/tools"),
            allow_source_builds: true,
            timeout_secs: 60,
        }
    }
}

impl MultiRegistryConfig {
    /// Create configuration for air-gapped/offline use
    #[must_use] 
    pub fn offline_mode() -> Self {
        Self {
            primary: RegistryBackend::local_only(),
            fallbacks: vec![],
            cache_dir: dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from(".cache"))
                .join("pekobot/tools"),
            allow_source_builds: true,
            timeout_secs: 60,
        }
    }

    /// Create configuration for self-hosted registry
    pub fn self_hosted(url: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            primary: RegistryBackend::Pekohub {
                url: url.clone(),
                api_key: None,
            },
            fallbacks: vec![RegistryBackend::local_only()],
            cache_dir: dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from(".cache"))
                .join("pekobot/tools"),
            allow_source_builds: true,
            timeout_secs: 60,
        }
    }
}

/// Unified tool registry supporting multiple backends
pub struct UnifiedToolRegistry {
    config: MultiRegistryConfig,
    http_client: reqwest::Client,
}

impl UnifiedToolRegistry {
    /// Create new unified registry
    pub fn new(config: MultiRegistryConfig) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .user_agent("Pekobot-Tool-Registry/1.0")
            .build()?;

        // Ensure cache directory exists
        std::fs::create_dir_all(&config.cache_dir)?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Get the cache directory path
    #[must_use]
    pub fn get_cache_dir(&self) -> &std::path::Path {
        &self.config.cache_dir
    }

    /// Load tool from any available backend
    pub async fn load_tool(
        &self,
        tool_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        // 1. Check cache first
        if let Some(cached) = self.find_in_cache(tool_name, version).await? {
            return Ok(cached);
        }

        // 2. Try primary registry
        match self
            .try_load_from_backend(&self.config.primary, tool_name, version)
            .await
        {
            Ok(path) => return Ok(path),
            Err(e) => {
                tracing::warn!("Primary registry failed: {}", e);
            }
        }

        // 3. Try fallbacks
        for backend in &self.config.fallbacks {
            match self
                .try_load_from_backend(backend, tool_name, version)
                .await
            {
                Ok(path) => return Ok(path),
                Err(e) => {
                    tracing::warn!("Fallback registry failed: {}", e);
                }
            }
        }

        // 4. Try source build if enabled
        if self.config.allow_source_builds {
            match self.build_from_source(tool_name).await {
                Ok(path) => return Ok(path),
                Err(e) => {
                    tracing::warn!("Source build failed: {}", e);
                }
            }
        }

        anyhow::bail!(
            "Tool '{}' not found in any registry (primary: {:?}, {} fallbacks tried)",
            tool_name,
            self.config.primary,
            self.config.fallbacks.len()
        );
    }

    /// Try to load from a specific backend
    async fn try_load_from_backend(
        &self,
        backend: &RegistryBackend,
        tool_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        match backend {
            RegistryBackend::Pekohub { url, api_key } => {
                self.load_from_pekohub(url, api_key.as_deref(), tool_name, version)
                    .await
            }
            RegistryBackend::Local { path } => self.load_from_local(path, tool_name, version).await,
            RegistryBackend::Source {
                source_path,
                build_cache,
            } => {
                self.build_from_source_path(source_path, build_cache, tool_name)
                    .await
            }
            RegistryBackend::Embedded { package_path } => {
                self.load_from_embedded(package_path, tool_name, version)
                    .await
            }
        }
    }

    /// Load from Pekohub HTTP API (anonymous or authenticated)
    async fn load_from_pekohub(
        &self,
        base_url: &str,
        api_key: Option<&str>,
        tool_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let version_str = version.unwrap_or("latest");
        let manifest_url = format!(
            "{base_url}/api/v1/tools/{tool_name}/{version_str}/manifest"
        );

        let mut request = self.http_client.get(&manifest_url);
        if let Some(key) = api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request.send().await?;
        if !response.status().is_success() {
            anyhow::bail!("Pekohub returned: {}", response.status());
        }

        let manifest: serde_json::Value = response.json().await?;
        let platforms = manifest
            .get("platforms")
            .ok_or_else(|| anyhow::anyhow!("No platforms in manifest"))?
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Invalid platforms"))?;

        // Find platform for current system
        let current_platform = detect_platform();
        let platform_info = platforms
            .get(&current_platform)
            .or_else(|| platforms.get("linux-x64")) // Fallback
            .ok_or_else(|| anyhow::anyhow!("No binary for platform: {current_platform}"))?;

        let download_path = platform_info
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow::anyhow!("No download URL"))?;

        let full_url = if download_path.starts_with("http") {
            download_path.to_string()
        } else {
            format!("{base_url}{download_path}")
        };

        // Download binary
        let binary_data = self
            .http_client
            .get(&full_url)
            .send()
            .await?
            .bytes()
            .await?;

        // Cache and return path
        let cache_path = self.save_to_cache(tool_name, version, &binary_data).await?;

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&cache_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&cache_path, perms)?;
        }

        Ok(cache_path)
    }

    /// Load from local filesystem
    async fn load_from_local(
        &self,
        base_path: &PathBuf,
        tool_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let version_str = version.unwrap_or("latest");
        let platform = detect_platform();

        let tool_path = base_path
            .join(tool_name)
            .join(version_str)
            .join(format!("{tool_name}-{platform}"));

        if tool_path.exists() {
            Ok(tool_path)
        } else {
            anyhow::bail!("Tool not found at: {}", tool_path.display())
        }
    }

    /// Build tool from source
    async fn build_from_source(&self, tool_name: &str) -> anyhow::Result<PathBuf> {
        let source_path = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(format!("pekobot/tool-sources/{tool_name}"));

        let build_cache = self.config.cache_dir.join("builds");
        self.build_from_source_path(&source_path, &build_cache, tool_name)
            .await
    }

    /// Build from specific source path
    async fn build_from_source_path(
        &self,
        source_path: &PathBuf,
        build_cache: &PathBuf,
        tool_name: &str,
    ) -> anyhow::Result<PathBuf> {
        if !source_path.exists() {
            anyhow::bail!("Source not found: {}", source_path.display());
        }

        tracing::info!("Building {} from source...", tool_name);

        // Check if cargo is available
        let cargo_check = tokio::process::Command::new("cargo")
            .arg("--version")
            .output()
            .await;

        if cargo_check.is_err() {
            anyhow::bail!("Cargo not found. Install Rust: https://rustup.rs");
        }

        // Build
        let output = tokio::process::Command::new("cargo")
            .arg("build")
            .arg("--release")
            .current_dir(source_path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Build failed: {stderr}");
        }

        // Copy to cache
        let built_binary = source_path.join("target/release").join(tool_name);

        let cache_path = build_cache.join(tool_name);
        std::fs::create_dir_all(build_cache)?;
        std::fs::copy(&built_binary, &cache_path)?;

        tracing::info!("Built and cached: {}", cache_path.display());
        Ok(cache_path)
    }

    /// Load from embedded agent package
    async fn load_from_embedded(
        &self,
        package_path: &PathBuf,
        tool_name: &str,
        _version: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let tools_dir = package_path.join("tools");
        let platform = detect_platform();
        let binary_name = format!("{tool_name}-{platform}");

        let tool_path = tools_dir.join(&binary_name);

        if tool_path.exists() {
            Ok(tool_path)
        } else {
            anyhow::bail!("Tool not found in package: {binary_name}")
        }
    }

    /// Check cache for existing tool
    async fn find_in_cache(
        &self,
        tool_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<Option<PathBuf>> {
        let version_str = version.unwrap_or("latest");
        let platform = detect_platform();
        let cache_path = self
            .config
            .cache_dir
            .join(tool_name)
            .join(version_str)
            .join(format!("{tool_name}-{platform}"));

        if cache_path.exists() {
            tracing::debug!("Found in cache: {}", cache_path.display());
            Ok(Some(cache_path))
        } else {
            Ok(None)
        }
    }

    /// Save binary to cache
    async fn save_to_cache(
        &self,
        tool_name: &str,
        version: Option<&str>,
        data: &[u8],
    ) -> anyhow::Result<PathBuf> {
        let version_str = version.unwrap_or("latest");
        let platform = detect_platform();
        let cache_dir = self.config.cache_dir.join(tool_name).join(version_str);

        std::fs::create_dir_all(&cache_dir)?;

        let cache_path = cache_dir.join(format!("{tool_name}-{platform}"));
        tokio::fs::write(&cache_path, data).await?;

        tracing::info!("Cached: {}", cache_path.display());
        Ok(cache_path)
    }

    /// List available tools from all backends
    pub async fn list_available_tools(&self) -> anyhow::Result<Vec<ToolInfo>> {
        let mut all_tools = HashMap::new();

        // Query primary
        if let Ok(tools) = self.list_from_backend(&self.config.primary).await {
            for tool in tools {
                all_tools.insert(tool.name.clone(), tool);
            }
        }

        // Query fallbacks
        for backend in &self.config.fallbacks {
            if let Ok(tools) = self.list_from_backend(backend).await {
                for tool in tools {
                    all_tools.entry(tool.name.clone()).or_insert(tool);
                }
            }
        }

        Ok(all_tools.into_values().collect())
    }

    /// List tools from specific backend
    async fn list_from_backend(&self, backend: &RegistryBackend) -> anyhow::Result<Vec<ToolInfo>> {
        match backend {
            RegistryBackend::Pekohub { url, api_key } => {
                let mut request = self.http_client.get(format!("{url}/api/v1/tools"));
                if let Some(key) = api_key {
                    request = request.header("Authorization", format!("Bearer {key}"));
                }

                let response = request.send().await?;
                let data: serde_json::Value = response.json().await?;

                let tools: Vec<ToolInfo> = data
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| serde_json::from_value(v.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();

                Ok(tools)
            }
            RegistryBackend::Local { path } => {
                let mut tools = vec![];
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            let name = entry.file_name().to_string_lossy().to_string();
                            tools.push(ToolInfo {
                                name,
                                version: "local".to_string(),
                                description: "Local tool".to_string(),
                                author: None,
                                source: "local".to_string(),
                            });
                        }
                    }
                }
                Ok(tools)
            }
            _ => Ok(vec![]),
        }
    }
}

/// Tool information
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub source: String,
}

/// Detect current platform
fn detect_platform() -> String {
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
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    format!("{os}-{arch}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_offline_config() {
        let config = MultiRegistryConfig::offline_mode();
        match config.primary {
            RegistryBackend::Local { .. } => {}
            _ => panic!("Expected local backend"),
        }
        assert!(config.fallbacks.is_empty());
    }

    #[test]
    fn test_self_hosted_config() {
        let config = MultiRegistryConfig::self_hosted("https://my-hub.example.com");
        match config.primary {
            RegistryBackend::Pekohub { url, .. } => {
                assert_eq!(url, "https://my-hub.example.com");
            }
            _ => panic!("Expected Pekohub backend"),
        }
    }
}
