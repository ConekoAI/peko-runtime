//! Tool Catalog Implementation
//!
//! Aggregates all tool sources into a unified catalog:
//! - MCP servers from `mcp.toml`
//! - Universal Tools from `{data_dir}/tools/`
//! - Downloaded tools from `ToolRegistry` (Pekohub)

use crate::common::paths::PathResolver;
use crate::mcp::config::{McpConfig, McpServerConfig};
use crate::tool_management::{
    InstalledToolInfo, ToolSearchResult, ToolType,
};
use crate::tool_registry::{
    InstalledTool as RegistryInstalledTool, RemoteRegistryClient,
    RemoteRegistryConfig, ToolRegistry, ToolRegistryConfig,
};
// Legacy discovery removed - using ExtensionManager instead
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::tool_management::ToolCatalog;

/// Unified tool catalog implementation
pub struct ToolCatalogImpl {
    path_resolver: PathResolver,
    mcp_config_path: PathBuf,
    tools_dir: PathBuf,
    local_registry: ToolRegistry,
    remote_client: Option<RemoteRegistryClient>,
    /// Cache of tools by name for quick lookup
    tools_cache: RwLock<HashMap<String, InstalledToolInfo>>,
}

impl ToolCatalogImpl {
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
            tools_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Refresh the tool cache by aggregating all sources
    async fn refresh_cache(&self) -> anyhow::Result<()> {
        let mut cache = HashMap::new();

        // 1. MCP servers from mcp.toml
        let mcp_tools = self.load_mcp_tools().await?;
        for tool in mcp_tools {
            cache.insert(tool.name.clone(), tool);
        }

        // 2. Universal Tools from tools_dir
        let universal_tools = self.load_universal_tools().await?;
        for tool in universal_tools {
            cache.insert(tool.name.clone(), tool);
        }

        // 3. Downloaded tools from local registry
        let downloaded_tools = self.load_downloaded_tools();
        for tool in downloaded_tools {
            cache.insert(tool.name.clone(), tool);
        }

        let mut write_guard = self.tools_cache.write().await;
        *write_guard = cache;

        Ok(())
    }

    /// Load MCP servers from configuration
    async fn load_mcp_tools(&self) -> anyhow::Result<Vec<InstalledToolInfo>> {
        let config = McpConfig::load_with_auto_detect(Some(&self.mcp_config_path)).await?;

        Ok(config
            .servers
            .into_iter()
            .map(InstalledToolInfo::mcp)
            .collect())
    }

    /// Load Universal Tools from tools directory
    async fn load_universal_tools(&self) -> anyhow::Result<Vec<InstalledToolInfo>> {
        // Use ExtensionManager for unified discovery
        use crate::extensions::adapters::BuiltInAdapters;
        use crate::extensions::manager::ExtensionManager;
        let mut manager = ExtensionManager::new();
        for adapter in BuiltInAdapters::new().adapters() {
            manager.register_adapter(adapter);
        }

        let discovered = manager.scan_directory(&self.tools_dir).await?;

        let mut tools = Vec::new();
        for discovered_ext in discovered {
            let info = self.discovered_ext_to_info(discovered_ext).await;
            match info {
                Ok(t) => tools.push(t),
                Err(e) => warn!("Failed to load universal tool: {}", e),
            }
        }

        Ok(tools)
    }

    /// Convert DiscoveredExtension to InstalledToolInfo
    async fn discovered_ext_to_info(
        &self,
        discovered: crate::extensions::manager::DiscoveredExtension,
    ) -> anyhow::Result<InstalledToolInfo> {
        let tool_name = discovered.path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Find executable
        let executable = self.find_executable(&discovered.path, &tool_name).await;

        Ok(InstalledToolInfo::universal(
            tool_name,
            executable.unwrap_or_else(|| discovered.path.join(&tool_name)),
            Some(discovered.manifest_path),
        ))
    }

    /// Find executable for a tool
    async fn find_executable(&self, tool_path: &std::path::Path, tool_name: &str) -> Option<std::path::PathBuf> {
        // Try common patterns
        let candidates = vec![
            tool_path.join(format!("{}.py", tool_name)),
            tool_path.join(format!("{}.js", tool_name)),
            tool_path.join(format!("{}.sh", tool_name)),
            tool_path.join(tool_name),
        ];

        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
        }

        // Fallback: find any file that's not manifest.json
        if let Ok(mut entries) = tokio::fs::read_dir(tool_path).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_name() {
                        if name != "manifest.json" {
                            return Some(path);
                        }
                    }
                }
            }
        }

        None
    }

    /// Load downloaded tools from local registry
    fn load_downloaded_tools(&self) -> Vec<InstalledToolInfo> {
        self.local_registry
            .list_installed()
            .into_iter()
            .map(|t| {
                let manifest = &t.manifest;
                InstalledToolInfo::downloaded(
                    &manifest.tool.name,
                    &manifest.tool.version,
                    &manifest.tool.description,
                    t.install_path.clone(),
                    Some(t.install_path.join("tool.toml")),
                )
            })
            .collect()
    }
}

#[async_trait]
impl ToolCatalog for ToolCatalogImpl {
    async fn list_installed(&self) -> Vec<InstalledToolInfo> {
        // Check cache first
        {
            let cache = self.tools_cache.read().await;
            if !cache.is_empty() {
                return cache.values().cloned().collect();
            }
        }

        // Refresh cache if empty
        if let Err(e) = self.refresh_cache().await {
            tracing::warn!("Failed to refresh tool cache: {}", e);
        }

        let cache = self.tools_cache.read().await;
        cache.values().cloned().collect()
    }

    async fn get_tool(&self, name: &str) -> Option<InstalledToolInfo> {
        let tools = self.list_installed().await;
        tools.into_iter().find(|t| t.name == name)
    }

    async fn search_registry(&self, query: &str) -> anyhow::Result<Vec<ToolSearchResult>> {
        match &self.remote_client {
            Some(client) => {
                let entries = client.search_tools(query).await?;
                Ok(entries
                    .into_iter()
                    .map(|e| ToolSearchResult {
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

    async fn list_available(&self) -> anyhow::Result<Vec<ToolSearchResult>> {
        match &self.remote_client {
            Some(client) => {
                let entries = client.list_tools(None).await?;
                Ok(entries
                    .into_iter()
                    .map(|e| ToolSearchResult {
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

impl From<McpServerConfig> for InstalledToolInfo {
    fn from(config: McpServerConfig) -> Self {
        InstalledToolInfo::mcp(config)
    }
}
