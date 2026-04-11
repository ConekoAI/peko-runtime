//! Capability Catalog Implementation
//!
//! Aggregates all capability sources into a unified catalog:
//! - Built-in capabilities (from builtin registry)
//! - MCP servers from `mcp.toml`
//! - Universal Capabilities from `{data_dir}/tools/`
//! - Downloaded capabilities from `ToolRegistry` (Pekohub)

use crate::cap::builtin::BuiltInCapabilityRegistry;
use crate::common::paths::PathResolver;
use crate::extensions::adapters::parsing;
use crate::mcp::config::McpConfig;
use crate::cap::{
    CapabilityCatalog, CapabilityInfo, CapabilitySearchResult,
};
use crate::tool_registry::{
    RemoteRegistryClient, RemoteRegistryConfig, ToolRegistry, ToolRegistryConfig,
};
// Legacy discovery removed - using ExtensionManager instead
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::warn;

/// Unified capability catalog implementation
pub struct CapabilityCatalogImpl {
    path_resolver: PathResolver,
    mcp_config_path: PathBuf,
    tools_dir: PathBuf,
    local_registry: ToolRegistry,
    remote_client: Option<RemoteRegistryClient>,
    /// Cache of capabilities by name for quick lookup
    cache: RwLock<HashMap<String, CapabilityInfo>>,
}

impl CapabilityCatalogImpl {
    /// Create a new catalog with the given path resolver
    pub fn new(path_resolver: PathResolver) -> Self {
        let mcp_config_path = path_resolver.mcp_config();
        let tools_dir = path_resolver.tools_dir();

        // Initialize local registry
        let registry_config = ToolRegistryConfig::default();
        let local_registry = ToolRegistry::new(registry_config).unwrap_or_else(|_| {
            // Fallback: create in-memory registry if disk fails
            ToolRegistry::new(ToolRegistryConfig {
                cache_dir: std::env::temp_dir().join("pekobot-tools"),
                ..Default::default()
            })
            .expect("Failed to create tool registry")
        });

        // Initialize remote client (optional, may fail if network unavailable)
        let remote_client = RemoteRegistryClient::new(
            RemoteRegistryConfig::default(),
            path_resolver.cache_dir().join("tool-registry"),
        )
        .ok();

        Self {
            path_resolver,
            mcp_config_path,
            tools_dir,
            local_registry,
            remote_client,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Refresh the capability cache by aggregating all sources
    async fn refresh_cache(&self) -> anyhow::Result<()> {
        let mut cache = HashMap::new();

        // 1. Built-in capabilities
        let builtin_caps = BuiltInCapabilityRegistry::list_all();
        for cap in builtin_caps {
            cache.insert(cap.name.clone(), cap);
        }

        // 2. MCP servers from mcp.toml
        let mcp_caps = self.load_mcp_capabilities().await?;
        for cap in mcp_caps {
            cache.insert(cap.name.clone(), cap);
        }

        // 3. Universal Capabilities from tools_dir
        let universal_caps = self.load_universal_capabilities().await?;
        for cap in universal_caps {
            cache.insert(cap.name.clone(), cap);
        }

        // 4. Downloaded capabilities from local registry
        let downloaded_caps = self.load_downloaded_capabilities();
        for cap in downloaded_caps {
            cache.insert(cap.name.clone(), cap);
        }

        // 5. Skills from skills directory
        let skill_caps = self.load_skills()?;
        for cap in skill_caps {
            cache.insert(cap.name.clone(), cap);
        }

        let mut write_guard = self.cache.write().await;
        *write_guard = cache;

        Ok(())
    }

    /// Load MCP servers from configuration
    async fn load_mcp_capabilities(&self) -> anyhow::Result<Vec<CapabilityInfo>> {
        let config = McpConfig::load_with_auto_detect(Some(&self.mcp_config_path)).await?;

        Ok(config
            .servers
            .into_iter()
            .map(CapabilityInfo::mcp)
            .collect())
    }

    /// Load Universal Capabilities from tools directory
    async fn load_universal_capabilities(&self) -> anyhow::Result<Vec<CapabilityInfo>> {
        // Use ExtensionManager for unified discovery
        use crate::extensions::adapters::BuiltInAdapters;
        use crate::extensions::manager::ExtensionManager;
        let mut manager = ExtensionManager::new();
        for adapter in BuiltInAdapters::new().adapters() {
            manager.register_adapter(adapter);
        }

        let discovered = manager.scan_directory(&self.tools_dir).await?;

        let mut caps = Vec::new();
        for discovered_ext in discovered {
            let info = self.discovered_ext_to_info(discovered_ext).await;
            match info {
                Ok(c) => caps.push(c),
                Err(e) => warn!("Failed to load universal tool: {}", e),
            }
        }

        Ok(caps)
    }

    /// Convert DiscoveredExtension to CapabilityInfo
    async fn discovered_ext_to_info(
        &self,
        discovered: crate::extensions::manager::DiscoveredExtension,
    ) -> anyhow::Result<CapabilityInfo> {
        // Find executable
        let tool_name = discovered.manifest_path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        
        let tool_path = discovered.path.clone();
        let executable = self.find_executable(&tool_path, tool_name).await;

        Ok(CapabilityInfo::universal(
            tool_name.to_string(),
            executable.unwrap_or_else(|| tool_path.join(tool_name)),
            Some(discovered.manifest_path),
        ))
    }

    /// Find executable for a tool
    /// Find executable for a capability (delegates to shared utility)
    async fn find_executable(&self, tool_path: &std::path::Path, tool_name: &str) -> Option<std::path::PathBuf> {
        parsing::find_executable(tool_path, tool_name).await
    }

    /// Load downloaded capabilities from local registry
    fn load_downloaded_capabilities(&self) -> Vec<CapabilityInfo> {
        self.local_registry
            .list_installed()
            .into_iter()
            .map(|t| {
                let manifest = &t.manifest;
                CapabilityInfo::downloaded(
                    &manifest.tool.name,
                    &manifest.tool.version,
                    &manifest.tool.description,
                    t.install_path.clone(),
                    Some(t.install_path.join("tool.toml")),
                )
            })
            .collect()
    }

    /// Load skills from skills directory using ExtensionCore-based discovery
    fn load_skills(&self) -> anyhow::Result<Vec<CapabilityInfo>> {
        let skills_dir = self.path_resolver.skills_dir();
        
        if !skills_dir.exists() {
            return Ok(Vec::new());
        }

        // Use SkillAdapter for skill discovery (ExtensionCore-based)
        use crate::extensions::adapters::skill_adapter::SkillAdapter;
        let adapter = SkillAdapter::new();
        let discovered = adapter.discover_skills(&skills_dir);

        let mut caps = Vec::new();
        for skill in discovered {
            let tags = skill.manifest.get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect())
                .unwrap_or_default();
            let author = skill.manifest.get("author")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);
            
            caps.push(CapabilityInfo::skill(
                &skill.manifest.name,
                &skill.manifest.description,
                skill.base_dir,
                skill.file_path,
                tags,
                author,
            ));
        }

        Ok(caps)
    }
}

#[async_trait]
impl CapabilityCatalog for CapabilityCatalogImpl {
    async fn list_installed(&self) -> Vec<CapabilityInfo> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if !cache.is_empty() {
                return cache.values().cloned().collect();
            }
        }

        // Refresh cache if empty
        if let Err(e) = self.refresh_cache().await {
            tracing::warn!("Failed to refresh capability cache: {}", e);
        }

        let cache = self.cache.read().await;
        cache.values().cloned().collect()
    }

    async fn get(&self, name: &str) -> Option<CapabilityInfo> {
        let caps = self.list_installed().await;
        caps.into_iter().find(|c| c.name == name)
    }

    async fn search_registry(&self, query: &str) -> anyhow::Result<Vec<CapabilitySearchResult>> {
        match &self.remote_client {
            Some(client) => {
                let entries = client.search_tools(query).await?;
                Ok(entries
                    .into_iter()
                    .map(|e| CapabilitySearchResult {
                        name: e.name,
                        version: e.version,
                        description: e.description,
                        author: Some(e.author),
                        categories: e.categories,
                        downloads: e.downloads,
                        rating: e.rating,
                    })
                    .collect())
            }
            None => Ok(Vec::new()),
        }
    }

    async fn list_available(&self) -> anyhow::Result<Vec<CapabilitySearchResult>> {
        match &self.remote_client {
            Some(client) => {
                let entries = client.list_tools(None).await?;
                Ok(entries
                    .into_iter()
                    .map(|e| CapabilitySearchResult {
                        name: e.name,
                        version: e.version,
                        description: e.description,
                        author: Some(e.author),
                        categories: e.categories,
                        downloads: e.downloads,
                        rating: e.rating,
                    })
                    .collect())
            }
            None => Ok(Vec::new()),
        }
    }
}
