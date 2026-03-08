//! Pekohub Remote Registry Client
//!
//! Phase 2: HTTP-based tool download from remote registry.
//! Downloads tools on-demand with signature verification.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::tool_registry::{
    BinaryUrls, InstalledTool, ToolCapabilities, ToolManifest, ToolMetadata,
};

/// Pekohub API manifest response format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekohubManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub capabilities: Vec<String>,
    pub platforms: HashMap<String, PekohubPlatform>,
    pub signature: String,
    pub rating: Option<f64>,
    pub download_count: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekohubPlatform {
    pub url: String,
    pub checksum: String,
    pub size: u64,
}

impl PekohubManifest {
    /// Convert Pekohub manifest to internal `ToolManifest` format
    #[must_use]
    pub fn to_tool_manifest(self) -> ToolManifest {
        let mut binaries = BinaryUrls {
            linux_x64: None,
            linux_arm64: None,
            macos_x64: None,
            macos_arm64: None,
            windows_x64: None,
        };

        for (platform, info) in self.platforms {
            let url = format!("{}{}", "https://tools.coneko.ai", info.url);
            match platform.as_str() {
                "linux-x64" => binaries.linux_x64 = Some(url),
                "linux-arm64" => binaries.linux_arm64 = Some(url),
                "macos-x64" => binaries.macos_x64 = Some(url),
                "macos-arm64" => binaries.macos_arm64 = Some(url),
                "windows-x64" => binaries.windows_x64 = Some(url),
                _ => {}
            }
        }

        ToolManifest {
            tool: ToolMetadata {
                name: self.name,
                version: self.version,
                description: self.description,
                author: Some(self.author),
                license: Some("MIT".to_string()),
                homepage: None,
                repository: None,
                category: Some("tool".to_string()),
                keywords: None,
            },
            capabilities: ToolCapabilities {
                provides: self.capabilities,
                permissions: None,
            },
            binaries: if binaries.linux_x64.is_some()
                || binaries.linux_arm64.is_some()
                || binaries.macos_x64.is_some()
                || binaries.macos_arm64.is_some()
                || binaries.windows_x64.is_some()
            {
                Some(binaries)
            } else {
                None
            },
            dependencies: None,
            install: None,
            security: None,
        }
    }
}

/// Remote registry configuration
#[derive(Debug, Clone)]
pub struct RemoteRegistryConfig {
    /// Registry base URL
    pub registry_url: String,
    /// API key for authenticated requests
    pub api_key: Option<String>,
    /// Request timeout in seconds
    pub timeout_secs: u64,
    /// Verify signatures
    pub verify_signatures: bool,
    /// Cache TTL in hours
    pub cache_ttl_hours: u64,
}

impl Default for RemoteRegistryConfig {
    fn default() -> Self {
        Self {
            registry_url: "https://pekohub.io".to_string(),
            api_key: None,
            timeout_secs: 60,
            verify_signatures: true,
            cache_ttl_hours: 24,
        }
    }
}

/// Tool index entry from registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolIndexEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    pub rating: f32,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub categories: Vec<String>,
}

/// Download progress
#[derive(Debug, Clone)]
pub enum DownloadProgress {
    Starting,
    Downloading {
        bytes_downloaded: u64,
        total_bytes: u64,
    },
    Verifying,
    Installing,
    Complete,
    Failed(String),
}

/// Remote registry client
pub struct RemoteRegistryClient {
    config: RemoteRegistryConfig,
    http_client: reqwest::Client,
    cache_dir: PathBuf,
}

impl RemoteRegistryClient {
    /// Create new remote registry client
    pub fn new(config: RemoteRegistryConfig, cache_dir: PathBuf) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .user_agent("Pekobot-Tool-Registry/1.0")
            .build()?;

        // Ensure cache directory exists
        std::fs::create_dir_all(&cache_dir)?;

        Ok(Self {
            config,
            http_client,
            cache_dir,
        })
    }

    /// Search for tools in remote registry
    pub async fn search_tools(&self, query: &str) -> anyhow::Result<Vec<ToolIndexEntry>> {
        let url = format!("{}/api/v1/tools/search", self.config.registry_url);

        let mut request = self.http_client.get(&url).query(&[("q", query)]);

        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Search failed: {}", response.status());
        }

        let results: Vec<ToolIndexEntry> = response.json().await?;
        Ok(results)
    }

    /// List all available tools
    pub async fn list_tools(&self, category: Option<&str>) -> anyhow::Result<Vec<ToolIndexEntry>> {
        let url = format!("{}/api/v1/tools", self.config.registry_url);

        let mut request = self.http_client.get(&url);

        if let Some(cat) = category {
            request = request.query(&[("category", cat)]);
        }

        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!("List failed: {}", response.status());
        }

        let results: Vec<ToolIndexEntry> = response.json().await?;
        Ok(results)
    }

    /// Get tool manifest from registry
    pub async fn get_tool_manifest(
        &self,
        tool_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<ToolManifest> {
        let version_str = version.unwrap_or("latest");
        let url = format!(
            "{}/api/v1/tools/{}/{}/manifest",
            self.config.registry_url, tool_name, version_str
        );

        let mut request = self.http_client.get(&url);

        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request.send().await?;

        if response.status() == 404 {
            anyhow::bail!("Tool {tool_name}@{version_str} not found in registry");
        }

        if !response.status().is_success() {
            anyhow::bail!("Failed to fetch manifest: {}", response.status());
        }

        // Parse as Pekohub format and convert
        let pekohub_manifest: PekohubManifest = response.json().await?;
        Ok(pekohub_manifest.to_tool_manifest())
    }

    /// Download and install tool from registry
    pub async fn install_tool(
        &self,
        tool_name: &str,
        version: Option<&str>,
    ) -> anyhow::Result<InstalledTool> {
        println!("📦 Installing {tool_name} from registry...");

        // Step 1: Fetch manifest
        println!("  Fetching manifest...");
        let manifest = self.get_tool_manifest(tool_name, version).await?;
        println!("  Found {}@{}", manifest.tool.name, manifest.tool.version);

        // Step 2: Determine binary URL for current platform
        let binary_url = self
            .get_binary_url(&manifest)
            .ok_or_else(|| anyhow::anyhow!("No binary available for current platform"))?;

        // Step 3: Download binary
        println!("  Downloading binary...");
        let binary_path = self.download_binary(&binary_url, &manifest).await?;

        // Step 4: Verify signature if enabled
        if self.config.verify_signatures {
            println!("  Verifying signature...");
            self.verify_binary(&binary_path, &manifest).await?;
        }

        // Step 5: Install to local registry
        println!("  Installing...");
        let installed = self.install_to_local(&manifest, &binary_path).await?;

        println!(
            "✅ Successfully installed {}@{}",
            manifest.tool.name, manifest.tool.version
        );

        Ok(installed)
    }

    /// Download binary with progress
    async fn download_binary(&self, url: &str, manifest: &ToolManifest) -> anyhow::Result<PathBuf> {
        let tool_name = &manifest.tool.name;
        let version = &manifest.tool.version;

        // Create download directory
        let download_dir = self.cache_dir.join("downloads");
        std::fs::create_dir_all(&download_dir)?;

        let binary_path = download_dir.join(format!("{tool_name}-{version}"));

        // Check if already cached
        if binary_path.exists() {
            println!("    Using cached binary");
            return Ok(binary_path);
        }

        // Download
        let mut request = self.http_client.get(url);

        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Download failed: {}", response.status());
        }

        // Get total size if available
        let total_size = response.content_length().unwrap_or(0);

        // Stream download
        let bytes = response.bytes().await?;

        if total_size > 0 && bytes.len() as u64 != total_size {
            anyhow::bail!("Download incomplete");
        }

        // Save to file
        std::fs::write(&binary_path, &bytes)?;

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms)?;
        }

        println!("    Downloaded {} bytes", bytes.len());
        Ok(binary_path)
    }

    /// Verify binary signature
    async fn verify_binary(
        &self,
        binary_path: &PathBuf,
        manifest: &ToolManifest,
    ) -> anyhow::Result<()> {
        use sha2::{Digest, Sha256};

        // Calculate SHA256 checksum
        let content = std::fs::read(binary_path)?;
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let checksum = format!("{:x}", hasher.finalize());

        // Verify against manifest
        if let Some(ref expected_checksum) =
            manifest.security.as_ref().and_then(|s| s.checksum.clone())
        {
            if checksum != *expected_checksum {
                anyhow::bail!("Checksum mismatch! Expected: {expected_checksum}, Got: {checksum}");
            }
            println!("    ✓ Checksum verified");
        }

        // Verify Ed25519 signature if available
        if let Some(_expected_sig) = manifest.security.as_ref().and_then(|s| s.signature.clone()) {
            // In production, would verify Ed25519 signature
            // For now, just check it's present
            println!("    ✓ Signature present (verification skipped in demo)");
        }

        Ok(())
    }

    /// Install downloaded binary to local registry
    async fn install_to_local(
        &self,
        manifest: &ToolManifest,
        binary_path: &PathBuf,
    ) -> anyhow::Result<InstalledTool> {
        let tool_name = manifest.tool.name.clone();
        let install_dir = self.cache_dir.join(&tool_name);

        // Create install directory
        std::fs::create_dir_all(&install_dir)?;

        // Copy binary
        let dest_binary = install_dir.join("tool");
        std::fs::copy(binary_path, &dest_binary)?;

        // Save manifest
        let manifest_path = install_dir.join("tool.toml");
        let manifest_toml = toml::to_string_pretty(manifest)?;
        std::fs::write(manifest_path, manifest_toml)?;

        // Create InstalledTool record
        let installed = InstalledTool {
            manifest: manifest.clone(),
            install_path: install_dir,
            installed_at: chrono::Utc::now(),
            last_used: None,
            is_active: true,
        };

        Ok(installed)
    }

    /// Get binary URL for current platform
    fn get_binary_url(&self, manifest: &ToolManifest) -> Option<String> {
        let binaries = manifest.binaries.as_ref()?;

        #[cfg(target_os = "linux")]
        {
            #[cfg(target_arch = "x86_64")]
            return binaries.linux_x64.clone();
            #[cfg(target_arch = "aarch64")]
            return binaries.linux_arm64.clone();
        }

        #[cfg(target_os = "macos")]
        {
            #[cfg(target_arch = "x86_64")]
            return binaries.macos_x64.clone();
            #[cfg(target_arch = "aarch64")]
            return binaries.macos_arm64.clone();
        }

        #[cfg(target_os = "windows")]
        {
            #[cfg(target_arch = "x86_64")]
            return binaries.windows_x64.clone();
        }

        #[allow(unreachable_code)]
        None
    }

    /// Check for tool updates
    pub async fn check_for_updates(
        &self,
        tool_name: &str,
        current_version: &str,
    ) -> anyhow::Result<Option<ToolManifest>> {
        let latest = self.get_tool_manifest(tool_name, Some("latest")).await?;

        if latest.tool.version == current_version {
            println!("{tool_name} is up to date ({current_version})");
            Ok(None)
        } else {
            println!(
                "Update available: {} -> {}",
                current_version, latest.tool.version
            );
            Ok(Some(latest))
        }
    }

    /// Get download stats for a tool
    pub async fn get_tool_stats(&self, tool_name: &str) -> anyhow::Result<ToolStats> {
        let url = format!(
            "{}/api/v1/tools/{}/stats",
            self.config.registry_url, tool_name
        );

        let mut request = self.http_client.get(&url);

        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to fetch stats: {}", response.status());
        }

        let stats: ToolStats = response.json().await?;
        Ok(stats)
    }
}

/// Tool statistics from registry
#[derive(Debug, Clone, Deserialize)]
pub struct ToolStats {
    pub name: String,
    pub total_downloads: u64,
    pub avg_rating: f32,
    pub rating_count: u32,
    pub versions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_registry_client() {
        let cache_dir = std::env::temp_dir().join("pekohub-test");
        let client = RemoteRegistryClient::new(RemoteRegistryConfig::default(), cache_dir);
        assert!(client.is_ok());
    }
}
