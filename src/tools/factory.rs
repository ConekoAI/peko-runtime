//! Tool factory - Creates and configures all essential tools for agents
//!
//! This module provides centralized tool creation with proper configuration
//! from the agent config or environment.

use crate::security::SecurityPolicy;
use crate::tools::{
    ApplyPatchConfig, ApplyPatchTool, BrowserTool, CronTool, FetchConfig, FetchTool,
    FileSystemTool, HttpTool, ProcessTool, SessionStatusTool, SessionsHistoryTool,
    SessionsListTool, WebSearchConfig, WebSearchTool,
};
use std::path::PathBuf;
use std::sync::Arc;

/// MCP configuration for tool factory
#[derive(Debug, Clone)]
pub struct McpFactoryConfig {
    /// Enable MCP tools
    pub enabled: bool,
    /// Path to MCP config file
    pub config_path: Option<PathBuf>,
}

impl Default for McpFactoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            config_path: None,
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
    /// Enable HTTP tool
    pub enable_http: bool,
    /// Enable fetch tool
    pub enable_fetch: bool,
    /// Enable browser tool
    pub enable_browser: bool,
    /// Enable web search tool
    pub enable_web_search: bool,
    /// Enable apply patch tool
    pub enable_apply_patch: bool,
    /// Enable process tool
    pub enable_process: bool,
    /// Enable memory tool
    pub enable_memory: bool,
    /// Enable session introspection tools
    pub enable_session_tools: bool,
    /// Enable cron tool
    pub enable_cron: bool,
    /// Path to cron database (defaults to `workspace_dir/cron.db`)
    pub cron_db_path: Option<PathBuf>,
    /// Web search configuration
    pub web_search_config: Option<WebSearchConfig>,
    /// Fetch configuration
    pub fetch_config: Option<FetchConfig>,
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
            enable_http: true,
            enable_fetch: true,
            enable_browser: true,
            enable_web_search: true,
            enable_apply_patch: true,
            enable_process: true,
            enable_memory: true,
            enable_session_tools: true,
            enable_cron: true,
            cron_db_path: None,
            web_search_config: None,
            fetch_config: None,
            apply_patch_config: None,
            mcp: McpFactoryConfig::default(),
        }
    }
}

/// Tool factory for creating configured tool instances
pub struct ToolFactory;

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

        // HTTP tool
        if config.enable_http {
            if let Ok(tool) = HttpTool::new() {
                tools.push(Arc::new(tool));
            }
        }

        // Fetch tool
        if config.enable_fetch {
            let fetch_config = config.fetch_config.clone().unwrap_or_default();
            tools.push(Arc::new(FetchTool::new(fetch_config)));
        }

        // Browser tool
        if config.enable_browser {
            tools.push(Arc::new(BrowserTool::new(vec![], None)));
        }

        // Web search tool
        if config.enable_web_search {
            let ws_config = config.web_search_config.clone().unwrap_or_default();
            tools.push(Arc::new(WebSearchTool::new(ws_config)));
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

        // Memory tool
        if config.enable_memory {
            // Memory tool needs a memory backend - for now we'll skip it
            // as it requires proper initialization with the agent's memory store
            // tools.push(Arc::new(MemoryToolFactory::create()));
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
    /// This version loads MCP tools from configured MCP servers.
    pub async fn create_tools_async(
        config: &ToolFactoryConfig,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        // Start with built-in tools
        let mut tools = Self::create_tools(config);

        // Load MCP tools if enabled
        if config.mcp.enabled {
            match Self::load_mcp_tools(&config.mcp).await {
                Ok(mcp_tools) => {
                    if !mcp_tools.is_empty() {
                        tracing::info!("Loaded {} MCP tools", mcp_tools.len());
                        tools.extend(mcp_tools);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to load MCP tools: {}", e);
                    // Continue without MCP tools - don't fail the whole tool creation
                }
            }
        }

        Ok(tools)
    }

    /// Load MCP tools from configured MCP servers
    pub async fn load_mcp_tools(
        mcp_config: &McpFactoryConfig,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        use crate::mcp::{McpConfig, McpManager};

        // Determine config path - use pekobot config dir (not system config dir)
        let config_path = mcp_config.config_path.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".pekobot")
                .join("mcp.toml")
        });

        // DEBUG

        // Check if config exists
        if !config_path.exists() {
            tracing::debug!(
                "MCP config not found at {:?}, skipping MCP tools",
                config_path
            );
            return Ok(Vec::new());
        }

        tracing::info!("Loading MCP config from {:?}", config_path);

        // Load MCP configuration
        let content = tokio::fs::read_to_string(&config_path).await?;
        let config = McpConfig::from_toml(&content)?;

        if config.servers.is_empty() {
            tracing::debug!("No MCP servers configured");

            return Ok(Vec::new());
        }

        tracing::info!(
            "Initializing MCP manager with {} servers",
            config.servers.len()
        );

        // Initialize MCP manager
        let manager = McpManager::new(config);

        manager.init().await?;

        // Get tools from all servers

        let mcp_tools = manager.get_tools().await;

        // Shutdown manager (tools hold their own client references)
        if let Err(e) = manager.shutdown().await {
            tracing::warn!("Error shutting down MCP manager: {}", e);
        }

        Ok(mcp_tools)
    }

    /// Create minimal tools (filesystem + process only)
    #[must_use]
    pub fn create_minimal_tools(workspace_dir: PathBuf) -> Vec<Arc<dyn crate::tools::Tool>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            enable_http: false,
            enable_fetch: false,
            enable_browser: false,
            enable_web_search: false,
            enable_apply_patch: false,
            enable_memory: false,
            enable_session_tools: false,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create coding tools (filesystem + `apply_patch` + process + `web_search`)
    #[must_use]
    pub fn create_coding_tools(workspace_dir: PathBuf) -> Vec<Arc<dyn crate::tools::Tool>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            enable_browser: false,
            enable_memory: false,
            enable_session_tools: false,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create full toolset
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
        Self::create_tools_async(&config).await
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
            },
            ..Default::default()
        };
        Self::create_tools_async(&config).await
    }
}
