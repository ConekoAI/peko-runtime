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
//!
//! # Disabled Tools Support
//!
//! Tools can be disabled via `disabled_tools` config. Disabled tools:
//! - Are not registered with the LLM
//! - Return error if called directly
//! - Are filtered from the tool list

use crate::security::SecurityPolicy;
use crate::tools::{
    ApplyPatchConfig, ApplyPatchTool, CronTool, FileSystemTool, ProcessTool, SessionStatusTool,
    SessionsHistoryTool, SessionsListTool,
};
use std::collections::HashSet;
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
    /// Path to cron database (defaults to `workspace_dir/cron.json`)
    pub cron_db_path: Option<PathBuf>,
    /// Apply patch configuration
    pub apply_patch_config: Option<ApplyPatchConfig>,
    /// MCP configuration
    pub mcp: McpFactoryConfig,
    /// List of disabled tool names (e.g., ["process", "cron"])
    pub disabled_tools: Vec<String>,
    /// Instance ID for cron persistence
    pub instance_id: Option<String>,
    /// Team ID for team-scoped tools
    pub team_id: Option<String>,
    /// Allow cross-team access (requires explicit grant)
    pub allow_cross_team: bool,
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
            disabled_tools: Vec::new(),
            instance_id: None,
            team_id: None,
            allow_cross_team: false,
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

/// Tool creation result with metadata
pub struct ToolCreationResult {
    /// Created tools
    pub tools: Vec<Arc<dyn crate::tools::Tool>>,
    /// Names of tools that were disabled
    pub disabled: Vec<String>,
    /// MCP discovery result
    pub mcp: McpDiscoveryResult,
}

impl std::fmt::Debug for ToolCreationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCreationResult")
            .field("tool_count", &self.tools.len())
            .field("disabled", &self.disabled)
            .field("mcp", &self.mcp)
            .finish()
    }
}

impl ToolFactory {
    /// Check if a tool is disabled
    fn is_disabled(disabled_tools: &[String], tool_name: &str) -> bool {
        disabled_tools
            .iter()
            .any(|d| d.to_lowercase() == tool_name.to_lowercase())
    }

    /// Create all essential tools based on configuration (synchronous version)
    ///
    /// Respects `disabled_tools` configuration - disabled tools are excluded
    /// from the returned list and won't be shown to the LLM.
    #[must_use]
    pub fn create_tools(config: &ToolFactoryConfig) -> ToolCreationResult {
        let mut tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();
        let mut disabled = Vec::new();
        let disabled_set: HashSet<String> = config
            .disabled_tools
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        // Filesystem tool with security policy
        if config.enable_filesystem && !Self::is_disabled(&config.disabled_tools, "filesystem") {
            let policy = SecurityPolicy {
                workspace_dir: config.workspace_dir.clone(),
                workspace_only: false,
                ..Default::default()
            };
            tools.push(Arc::new(FileSystemTool::with_policy(policy)));
        } else if config.enable_filesystem {
            disabled.push("filesystem".to_string());
        }

        // Apply patch tool
        if config.enable_apply_patch && !Self::is_disabled(&config.disabled_tools, "apply_patch") {
            let patch_config = config.apply_patch_config.clone().unwrap_or_default();
            tools.push(Arc::new(ApplyPatchTool::new(
                patch_config,
                config.workspace_dir.clone(),
            )));
        } else if config.enable_apply_patch {
            disabled.push("apply_patch".to_string());
        }

        // Process tool
        if config.enable_process && !Self::is_disabled(&config.disabled_tools, "process") {
            tools.push(Arc::new(
                ProcessTool::new().with_workspace(config.workspace_dir.clone()),
            ));
        } else if config.enable_process {
            disabled.push("process".to_string());
        }

        // Session introspection tools
        if config.enable_session_tools {
            // Sessions list
            if !Self::is_disabled(&config.disabled_tools, "sessions_list") {
                tools.push(Arc::new(SessionsListTool::new(Box::new(
                    crate::tools::InMemorySessionRegistry::new("main".to_string()),
                ))));
            } else {
                disabled.push("sessions_list".to_string());
            }

            // Sessions history
            if !Self::is_disabled(&config.disabled_tools, "sessions_history") {
                tools.push(Arc::new(SessionsHistoryTool::new(Box::new(
                    crate::tools::InMemorySessionRegistry::new("main".to_string()),
                ))));
            } else {
                disabled.push("sessions_history".to_string());
            }

            // Session status
            if !Self::is_disabled(&config.disabled_tools, "session_status") {
                tools.push(Arc::new(SessionStatusTool::new(Box::new(
                    crate::tools::InMemorySessionRegistry::new("main".to_string()),
                ))));
            } else {
                disabled.push("session_status".to_string());
            }
        }

        // Cron tool for scheduled jobs
        if config.enable_cron && !Self::is_disabled(&config.disabled_tools, "cron") {
            let db_path = config
                .cron_db_path
                .clone()
                .unwrap_or_else(|| config.workspace_dir.join("cron.json"));
            let instance_id = config
                .instance_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            let cron_tool = CronTool::new(db_path, instance_id);
            tools.push(Arc::new(cron_tool));
        } else if config.enable_cron {
            disabled.push("cron".to_string());
        }

        ToolCreationResult {
            tools,
            disabled,
            mcp: McpDiscoveryResult {
                servers_found: 0,
                tools_available: 0,
                failed_servers: Vec::new(),
                auto_install_attempted: false,
            },
        }
    }

    /// Create all essential tools including MCP tools (asynchronous version)
    ///
    /// This version uses MCP-first loading:
    /// 1. Discover and connect to MCP servers
    /// 2. Load tools from MCP servers
    /// 3. Fall back to core tools if MCP unavailable
    ///
    /// Respects `disabled_tools` - disabled built-in tools are excluded.
    pub async fn create_tools_async(
        config: &ToolFactoryConfig,
    ) -> anyhow::Result<ToolCreationResult> {
        // Start with core built-in tools
        let mut result = Self::create_tools(config);
        let mut discovery_result = McpDiscoveryResult {
            servers_found: 0,
            tools_available: 0,
            failed_servers: Vec::new(),
            auto_install_attempted: false,
        };

        // Try to discover and load MCP tools if enabled
        if config.mcp.enabled {
            tracing::info!("Discovering MCP servers...");

            match Self::load_mcp_tools_with_discovery(&config.mcp, &config.disabled_tools).await {
                Ok((mcp_tools, mcp_result)) => {
                    if !mcp_tools.is_empty() {
                        tracing::info!(
                            "✅ Loaded {} tools from {} MCP servers",
                            mcp_tools.len(),
                            mcp_result.servers_found
                        );
                        result.tools.extend(mcp_tools);
                    } else {
                        tracing::info!("ℹ️ No MCP servers configured or available");
                    }
                    discovery_result = mcp_result;
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

        result.mcp = discovery_result;
        Ok(result)
    }

    /// Load MCP tools with discovery metadata
    async fn load_mcp_tools_with_discovery(
        mcp_config: &McpFactoryConfig,
        disabled_tools: &[String],
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
            return Ok((
                Vec::new(),
                McpDiscoveryResult {
                    servers_found: 0,
                    tools_available: 0,
                    failed_servers: Vec::new(),
                    auto_install_attempted: false,
                },
            ));
        }

        // Load MCP configuration
        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: McpConfig = toml::from_str(&content)?;

        if config.servers.is_empty() {
            tracing::debug!("No MCP servers configured");
            return Ok((
                Vec::new(),
                McpDiscoveryResult {
                    servers_found: 0,
                    tools_available: 0,
                    failed_servers: Vec::new(),
                    auto_install_attempted: false,
                },
            ));
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

                // Filter out disabled MCP tools
                let filtered_tools: Vec<_> = mcp_tools
                    .into_iter()
                    .filter(|tool| {
                        let name = tool.name().to_lowercase();
                        let is_disabled = disabled_tools.iter().any(|d| d.to_lowercase() == name);
                        if is_disabled {
                            tracing::debug!("MCP tool '{}' is disabled, skipping", tool.name());
                        }
                        !is_disabled
                    })
                    .collect();

                let tools_count = filtered_tools.len();

                tracing::info!(
                    "Connected to {} MCP server(s) with {} tools",
                    servers_configured,
                    tools_count
                );

                Ok((
                    filtered_tools,
                    McpDiscoveryResult {
                        servers_found: servers_configured,
                        tools_available: tools_count,
                        failed_servers: Vec::new(),
                        auto_install_attempted: mcp_config.auto_install,
                    },
                ))
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
    /// Respects disabled_tools
    #[must_use]
    pub fn create_minimal_tools(
        workspace_dir: PathBuf,
        disabled_tools: Vec<String>,
    ) -> ToolCreationResult {
        let config = ToolFactoryConfig {
            workspace_dir,
            enable_apply_patch: false,
            enable_session_tools: false,
            enable_cron: false,
            disabled_tools,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create coding tools (filesystem + apply_patch + process)
    /// Respects disabled_tools
    #[must_use]
    pub fn create_coding_tools(
        workspace_dir: PathBuf,
        disabled_tools: Vec<String>,
    ) -> ToolCreationResult {
        let config = ToolFactoryConfig {
            workspace_dir,
            enable_session_tools: false,
            disabled_tools,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create full toolset (core tools only, sync version)
    /// Respects disabled_tools
    #[must_use]
    pub fn create_full_tools(
        workspace_dir: PathBuf,
        disabled_tools: Vec<String>,
    ) -> ToolCreationResult {
        let config = ToolFactoryConfig {
            workspace_dir,
            disabled_tools,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create full toolset including MCP tools (async version)
    /// Respects disabled_tools
    pub async fn create_full_tools_async(
        workspace_dir: PathBuf,
        disabled_tools: Vec<String>,
    ) -> anyhow::Result<ToolCreationResult> {
        let config = ToolFactoryConfig {
            workspace_dir,
            disabled_tools,
            ..Default::default()
        };
        Self::create_tools_async(&config).await
    }

    /// Create full toolset with custom MCP config path
    pub async fn create_full_tools_with_mcp(
        workspace_dir: PathBuf,
        mcp_config_path: PathBuf,
        disabled_tools: Vec<String>,
    ) -> anyhow::Result<ToolCreationResult> {
        let config = ToolFactoryConfig {
            workspace_dir,
            disabled_tools,
            mcp: McpFactoryConfig {
                enabled: true,
                config_path: Some(mcp_config_path),
                ..Default::default()
            },
            ..Default::default()
        };
        Self::create_tools_async(&config).await
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

    /// Get list of all available built-in tool names
    pub fn builtin_tool_names() -> Vec<&'static str> {
        vec![
            "filesystem",
            "process",
            "apply_patch",
            "agent_spawn",
            "agent_spawn_status",
            "agent_spawn_list",
            "agents_list",
            "agent_info",
            "sessions_send",
            "sessions_list",
            "sessions_history",
            "session_status",
            "cron",
        ]
    }

    /// Validate disabled tools list
    pub fn validate_disabled_tools(disabled: &[String]) -> Vec<String> {
        let valid_tools: std::collections::HashSet<&str> =
            Self::builtin_tool_names().into_iter().collect();

        disabled
            .iter()
            .filter(|d| !valid_tools.contains(d.as_str()))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_tools_filtering() {
        let config = ToolFactoryConfig {
            workspace_dir: PathBuf::from("."),
            disabled_tools: vec!["process".to_string(), "cron".to_string()],
            ..Default::default()
        };

        let result = ToolFactory::create_tools(&config);

        // Check that process and cron are in disabled list
        assert!(result.disabled.contains(&"process".to_string()));
        assert!(result.disabled.contains(&"cron".to_string()));

        // Check that disabled tools are not in tools list
        let tool_names: Vec<_> = result.tools.iter().map(|t| t.name()).collect();
        assert!(!tool_names.contains(&"process"));
        assert!(!tool_names.contains(&"cron"));
    }

    #[test]
    fn test_is_disabled_case_insensitive() {
        let disabled = vec!["Process".to_string(), "CRON".to_string()];

        assert!(ToolFactory::is_disabled(&disabled, "process"));
        assert!(ToolFactory::is_disabled(&disabled, "PROCESS"));
        assert!(ToolFactory::is_disabled(&disabled, "cron"));
        assert!(!ToolFactory::is_disabled(&disabled, "filesystem"));
    }

    #[test]
    fn test_validate_disabled_tools() {
        let disabled = vec![
            "process".to_string(),
            "invalid_tool".to_string(),
            "filesystem".to_string(),
        ];

        let invalid = ToolFactory::validate_disabled_tools(&disabled);
        assert!(invalid.contains(&"invalid_tool".to_string()));
        assert!(!invalid.contains(&"process".to_string()));
        assert!(!invalid.contains(&"filesystem".to_string()));
    }

    #[test]
    fn test_builtin_tool_names() {
        let names = ToolFactory::builtin_tool_names();
        assert!(names.contains(&"filesystem"));
        assert!(names.contains(&"process"));
        assert!(names.contains(&"cron"));
        assert!(names.contains(&"agent_spawn"));
    }
}
