//! Tool factory - Creates and configures all essential tools for agents
//!
//! This module provides centralized tool creation with proper configuration
//! from the agent config or environment.
//!
//! Note: Heavy tools (web_search, fetch, http, browser, memory) have been
//! migrated to standalone MCP servers. Use MCP configuration to enable them.
//!
//! # Tool Loading Strategy (MCP-First)
//!
//! 1. Try to load tools from MCP servers first (external processes)
//! 2. If MCP unavailable, log warning and continue with core tools only
//! 3. Users can install MCP servers via `pekobot mcp install <server>`

use crate::security::SecurityPolicy;
use crate::tools::{
    ApplyPatchConfig, ApplyPatchTool, CronTool, FileSystemTool, ProcessTool,
    SessionStatusTool, SessionsHistoryTool, SessionsListTool,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// MCP configuration for tool factory
#[derive(Debug, Clone)]
pub struct McpFactoryConfig {
    /// Enable MCP tools
    pub enabled: bool,
    /// Path to MCP config file
    pub config_path: Option<PathBuf>,
    /// Auto-install missing MCP servers
    pub auto_install: bool,
    /// MCP-first mode: prefer MCP over embedded tools
    pub mcp_first: bool,
}

impl Default for McpFactoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            config_path: None,
            auto_install: false, // Disabled by default for security
            mcp_first: true,     // MCP-first by default
        }
    }
}

/// Configuration for tool factory
#[derive(Debug, Clone)]
pub struct ToolFactoryConfig {
    /// Workspace directory (for filesystem security)
    pub workspace_dir: PathBuf,
    /// Enable filesystem tool
    pub enable_filesystem: bool,
    /// Enable apply patch tool
    pub enable_apply_patch: bool,
    /// Enable process tool
    pub enable_process: bool,
    /// Enable session introspection tools
    pub enable_session_tools: bool,
    /// Enable cron tool
    pub enable_cron: bool,
    /// Path to cron database (defaults to `workspace_dir/cron.db`)
    pub cron_db_path: Option<PathBuf>,
    /// Apply patch configuration
    pub apply_patch_config: Option<ApplyPatchConfig>,
    /// MCP configuration
    pub mcp: McpFactoryConfig,
}

impl Default for ToolFactoryConfig {
    fn default() -> Self {
        Self {
            workspace_dir: PathBuf::from("."),
            enable_filesystem: true,
            enable_apply_patch: true,
            enable_process: true,
            enable_session_tools: true,
            enable_cron: true,
            cron_db_path: None,
            apply_patch_config: None,
            mcp: McpFactoryConfig::default(),
        }
    }
}

/// Tool factory for creating configured tool instances
pub struct ToolFactory;

/// Result of MCP discovery
#[derive(Debug)]
pub struct McpDiscoveryResult {
    /// Number of MCP servers discovered
    pub servers_found: usize,
    /// Number of tools available via MCP
    pub tools_available: usize,
    /// Server names that failed to connect
    pub failed_servers: Vec<String>,
    /// Whether auto-install was attempted
    pub auto_install_attempted: bool,
}

impl McpDiscoveryResult {
    /// Check if any MCP servers were found
    #[must_use]
    pub fn has_mcp_tools(&self) -> bool {
        self.tools_available > 0
    }
}

impl ToolFactory {
    /// Create all essential tools based on configuration (synchronous version)
    ///
    /// Note: This does NOT include MCP tools. Use `create_tools_async` for full
    /// tool loading including MCP servers.
    #[must_use]
    pub fn create_tools(config: &ToolFactoryConfig) -> Vec<Arc<dyn crate::tools::Tool>> {
        let mut tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();

        // Filesystem tool with security policy
        if config.enable_filesystem {
            let policy = SecurityPolicy {
                workspace_dir: config.workspace_dir.clone(),
                workspace_only: false,
                ..Default::default()
            };
            tools.push(Arc::new(FileSystemTool::with_policy(policy)));
        }

        // Apply patch tool
        if config.enable_apply_patch {
            let patch_config = config.apply_patch_config.clone().unwrap_or_default();
            tools.push(Arc::new(ApplyPatchTool::new(
                patch_config,
                config.workspace_dir.clone(),
            )));
        }

        // Process tool
        if config.enable_process {
            tools.push(Arc::new(ProcessTool::new()));
        }

        // Session introspection tools
        if config.enable_session_tools {
            // These need a session registry - create a placeholder for now
            // In production, this would be shared across all agents
            tools.push(Arc::new(SessionsListTool::new(Box::new(
                crate::tools::InMemorySessionRegistry::new("main".to_string()),
            ))));
            tools.push(Arc::new(SessionsHistoryTool::new(Box::new(
                crate::tools::InMemorySessionRegistry::new("main".to_string()),
            ))));
            tools.push(Arc::new(SessionStatusTool::new(Box::new(
                crate::tools::InMemorySessionRegistry::new("main".to_string()),
            ))));
        }

        // Cron tool for scheduled jobs
        if config.enable_cron {
            let db_path = config
                .cron_db_path
                .clone()
                .unwrap_or_else(|| config.workspace_dir.join("cron.db"));
            match CronTool::new(db_path) {
                Ok(cron_tool) => {
                    tools.push(Arc::new(cron_tool));
                }
                Err(e) => {
                    tracing::warn!("Failed to create CronTool: {}", e);
                    // Continue without cron tool - don't fail whole tool creation
                }
            }
        }

        tools
    }

    /// Create all essential tools including MCP tools (asynchronous version)
    ///
    /// This version uses MCP-first loading:
    /// 1. Discover and connect to MCP servers
    /// 2. Load tools from MCP servers
    /// 3. Fall back to core tools if MCP unavailable
    pub async fn create_tools_async(
        config: &ToolFactoryConfig,
    ) -> anyhow::Result<(Vec<Arc<dyn crate::tools::Tool>>, McpDiscoveryResult)> {
        // Start with core built-in tools
        let mut tools = Self::create_tools(config);
        let mut discovery_result = McpDiscoveryResult {
            servers_found: 0,
            tools_available: 0,
            failed_servers: Vec::new(),
            auto_install_attempted: false,
        };

        // Try to discover and load MCP tools if enabled
        if config.mcp.enabled {
            tracing::info!("Discovering MCP servers...");
            
            match Self::load_mcp_tools_with_discovery(&config.mcp).await {
                Ok((mcp_tools, result)) => {
                    if !mcp_tools.is_empty() {
                        tracing::info!(
                            "✅ Loaded {} tools from {} MCP servers",
                            mcp_tools.len(),
                            result.servers_found
                        );
                        tools.extend(mcp_tools);
                    } else {
                        tracing::info!("ℹ️ No MCP servers configured or available");
                    }
                    discovery_result = result;
                }
                Err(e) => {
                    tracing::warn!("❌ MCP discovery failed: {}", e);
                    tracing::info!("💡 Continuing with core tools only. Install MCP servers with:");
                    tracing::info!("   pekobot mcp install <web|browser|memory>");
                }
            }
        } else {
            tracing::debug!("MCP tools disabled, using core tools only");
        }

        Ok((tools, discovery_result))
    }

    /// Load MCP tools with discovery metadata
    async fn load_mcp_tools_with_discovery(
        mcp_config: &McpFactoryConfig,
    ) -> anyhow::Result<(Vec<Arc<dyn crate::tools::Tool>>, McpDiscoveryResult)> {
        use crate::mcp::{create_tool_proxies, McpConfig, McpManager};

        let config_path = mcp_config.config_path.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".pekobot")
                .join("mcp.toml")
        });

        // Check if config exists
        if !config_path.exists() {
            tracing::debug!("MCP config not found at {:?}", config_path);
            return Ok((Vec::new(), McpDiscoveryResult {
                servers_found: 0,
                tools_available: 0,
                failed_servers: Vec::new(),
                auto_install_attempted: false,
            }));
        }

        // Load MCP configuration
        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: McpConfig = toml::from_str(&content)?;

        if config.servers.is_empty() {
            tracing::debug!("No MCP servers configured");
            return Ok((Vec::new(), McpDiscoveryResult {
                servers_found: 0,
                tools_available: 0,
                failed_servers: Vec::new(),
                auto_install_attempted: false,
            }));
        }

        let servers_configured = config.servers.len();
        tracing::info!("Found {} MCP server(s) in config", servers_configured);

        // Initialize MCP manager
        let manager = Arc::new(RwLock::new(McpManager::new(config)));
        
        // Try to initialize (connect to servers)
        let init_result = {
            let mut manager_guard = manager.write().await;
            manager_guard.init().await
        };
        
        match init_result {
            Ok(()) => {
                // Create tool proxies
                let mcp_tools = create_tool_proxies(manager).await;
                let tools_count = mcp_tools.len();
                
                tracing::info!("Connected to {} MCP server(s) with {} tools", servers_configured, tools_count);
                
                Ok((mcp_tools, McpDiscoveryResult {
                    servers_found: servers_configured,
                    tools_available: tools_count,
                    failed_servers: Vec::new(),
                    auto_install_attempted: mcp_config.auto_install,
                }))
            }
            Err(e) => {
                tracing::warn!("Failed to initialize MCP manager: {}", e);
                Err(e.into())
            }
        }
    }

    /// Legacy: Load MCP tools from configured MCP servers using manager
    pub async fn load_mcp_tools(
        mcp_config: &McpFactoryConfig,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        use crate::mcp::{create_tool_proxies, McpConfig, McpManager};

        let config_path = mcp_config.config_path.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".pekobot")
                .join("mcp.toml")
        });

        if !config_path.exists() {
            tracing::debug!("MCP config not found at {:?}", config_path);
            return Ok(Vec::new());
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: McpConfig = toml::from_str(&content)?;

        if config.servers.is_empty() {
            return Ok(Vec::new());
        }

        let manager = Arc::new(RwLock::new(McpManager::new(config)));
        manager.write().await.init().await?;
        
        let tools = create_tool_proxies(manager).await;
        Ok(tools)
    }

    /// Create minimal tools (filesystem + process only)
    #[must_use]
    pub fn create_minimal_tools(workspace_dir: PathBuf) -> Vec<Arc<dyn crate::tools::Tool>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            enable_apply_patch: false,
            enable_session_tools: false,
            enable_cron: false,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create coding tools (filesystem + `apply_patch` + process)
    #[must_use]
    pub fn create_coding_tools(workspace_dir: PathBuf) -> Vec<Arc<dyn crate::tools::Tool>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            enable_session_tools: false,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create full toolset (core tools only, sync version)
    #[must_use]
    pub fn create_full_tools(workspace_dir: PathBuf) -> Vec<Arc<dyn crate::tools::Tool>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create full toolset including MCP tools (async version)
    pub async fn create_full_tools_async(
        workspace_dir: PathBuf,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            ..Default::default()
        };
        let (tools, _) = Self::create_tools_async(&config).await?;
        Ok(tools)
    }

    /// Create full toolset with custom MCP config path
    pub async fn create_full_tools_with_mcp(
        workspace_dir: PathBuf,
        mcp_config_path: PathBuf,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            mcp: McpFactoryConfig {
                enabled: true,
                config_path: Some(mcp_config_path),
                ..Default::default()
            },
            ..Default::default()
        };
        let (tools, _) = Self::create_tools_async(&config).await?;
        Ok(tools)
    }

    /// Check if MCP tools should be used (config exists)
    pub async fn should_use_mcp() -> bool {
        let config_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("mcp.toml");
        
        if !config_path.exists() {
            return false;
        }
        
        match tokio::fs::read_to_string(&config_path).await {
            Ok(content) => {
                if let Ok(config) = toml::from_str::<crate::mcp::McpConfig>(&content) {
                    !config.servers.is_empty()
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    /// Get MCP config path
    pub fn mcp_config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("mcp.toml")
    }
}
