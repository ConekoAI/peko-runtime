//! Tool factory - Creates and configures all essential tools for agents
//!
//! This module provides centralized tool creation with proper configuration
//! from the agent config or environment.
//!
//! Note: Heavy tools (web_search, fetch, http, browser, memory) have been
//! migrated to standalone MCP servers. Use MCP configuration to enable them.
//!
//! # Tool Loading Strategy (Resolution Order)
//!
//! Per CAPABILITY_INTERFACE.md §9.1, tools are resolved in this order:
//!
//! 1. **Built-in tools** - Compiled into the runtime, checked first
//! 2. **Custom tools** - From agent's `tools/` directory
//! 3. **MCP tools** - From configured MCP servers
//!
//! Built-in tools take precedence. Name conflicts are resolved by this order.
//!
//! # Disabled Tools Support
//!
//! Tools can be disabled via `disabled_tools` config. Disabled tools:
//! - Are not registered with the LLM
//! - Return error if called directly
//! - Are filtered from the tool list
//!
//! Note: Custom tools can also be disabled by name.

use crate::tools::traits::Tool;
use crate::tools::{
    CronTool, GlobTool, GrepTool, ReadFileTool,
    ShellTool, SessionStatusTool, SessionsHistoryTool, SessionsListTool, StrReplaceFileTool,
    WriteFileTool,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

// Extension Framework integration
use crate::extensions::async_integration::ExtensionAsyncTool;
use crate::extensions::core::{global_core, HookPoint};
use crate::extensions::{HookInput, HookOutput, HookResult};

/// Helper for filtering disabled tools and building the final tool list
///
/// This eliminates the repetitive pattern of checking if a tool is disabled
/// and adding it to either the tools list or the disabled list.
///
/// Note: This is unrelated to the `tool_registry` module which handles
/// downloading and installing tools from remote registries.
struct DisabledToolFilter {
    tools: Vec<Arc<dyn Tool>>,
    disabled: Vec<String>,
    disabled_set: HashSet<String>,
}

impl DisabledToolFilter {
    fn new(disabled_tools: &[String]) -> Self {
        let disabled_set: HashSet<String> = disabled_tools
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        Self {
            tools: Vec::new(),
            disabled: Vec::new(),
            disabled_set,
        }
    }

    /// Check if a tool name is disabled (case-insensitive)
    fn is_disabled(&self, name: &str) -> bool {
        self.disabled_set.contains(&name.to_lowercase())
    }

    /// Register a tool if it's not disabled
    ///
    /// # Arguments
    /// * `name` - The tool name to check against disabled list
    /// * `enabled` - Whether this tool category is enabled in config
    /// * `factory` - Closure that creates the tool
    fn register<F>(&mut self, name: &str, enabled: bool, factory: F)
    where
        F: FnOnce() -> Arc<dyn Tool>,
    {
        if !enabled {
            return;
        }

        if self.is_disabled(name) {
            self.disabled.push(name.to_string());
        } else {
            self.tools.push(factory());
        }
    }

    /// Register multiple tools with the same enabled condition
    fn register_all<F>(&mut self, enabled: bool, factory: F)
    where
        F: FnOnce() -> Vec<(String, Arc<dyn Tool>)>,
    {
        if !enabled {
            return;
        }

        for (name, tool) in factory() {
            if self.is_disabled(&name) {
                self.disabled.push(name);
            } else {
                self.tools.push(tool);
            }
        }
    }

    fn build(self) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
        (self.tools, self.disabled)
    }
}

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
    /// Workspace directory (default for relative paths)
    pub workspace_dir: PathBuf,
    /// Enable granular filesystem tools
    /// When true, enables ReadFile, Glob, Grep, WriteFile, StrReplaceFile
    pub enable_granular_fs: bool,
    /// Enable granular write tools (WriteFile, StrReplaceFile)
    /// Only effective when enable_granular_fs is true
    /// Defaults to true for full functionality, set to false for read-only
    pub enable_granular_write: bool,
    /// Enable shell tool (replaces process tool)
    pub enable_shell: bool,

    /// Enable session introspection tools
    pub enable_session_tools: bool,
    /// Enable cron tool
    pub enable_cron: bool,
    /// Path to cron database (defaults to `workspace_dir/cron.json`)
    pub cron_db_path: Option<PathBuf>,
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
    /// Path to custom tools directory (defaults to `workspace_dir/tools/`)
    pub custom_tools_dir: Option<PathBuf>,
    /// Enable custom tools from `tools/` directory
    pub enable_custom_tools: bool,
    /// Enable ToolWrapper for reserved parameter support
    /// When true, all tools are wrapped with ToolWrapper
    pub enable_wrapper: bool,
    /// Configuration for ToolWrapper (used when enable_wrapper is true)
    pub wrapper_config: crate::tools::WrapperConfig,
}

impl Default for ToolFactoryConfig {
    fn default() -> Self {
        Self {
            workspace_dir: PathBuf::from("."),
            enable_granular_fs: true, // Enabled by default
            enable_granular_write: true, // Enable write tools by default
            enable_shell: true,
            enable_session_tools: true,
            enable_cron: true,
            cron_db_path: None,
            mcp: McpFactoryConfig::default(),
            disabled_tools: Vec::new(),
            instance_id: None,
            team_id: None,
            allow_cross_team: false,
            custom_tools_dir: None,
            enable_custom_tools: true,
            enable_wrapper: true, // Enable by default for reserved parameter support
            wrapper_config: crate::tools::WrapperConfig::default(),
        }
    }
}

impl ToolFactoryConfig {
    /// Create a minimal configuration (read-only filesystem + shell)
    ///
    /// Use this for restricted environments where only basic file reading
    /// and shell operations are needed.
    pub fn minimal(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            enable_granular_fs: true,
            enable_granular_write: false, // Read-only: no WriteFile or StrReplaceFile
            enable_shell: true,
            enable_session_tools: false,
            enable_cron: false,
            mcp: McpFactoryConfig::disabled(),
            ..Default::default()
        }
    }

    /// Create a coding configuration (granular filesystem tools + shell)
    ///
    /// Use this for code editing tasks where targeted file modifications
    /// are preferred over full file rewrites.
    pub fn coding(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            enable_granular_fs: true,
            enable_session_tools: false,
            enable_cron: false,
            mcp: McpFactoryConfig::disabled(),
            ..Default::default()
        }
    }

    /// Create a full configuration (all built-in tools)
    ///
    /// This enables all built-in tools except MCP. Use `create_tools_async`
    /// if you need MCP tools as well.
    pub fn full(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            enable_granular_fs: true,
            mcp: McpFactoryConfig::disabled(),
            ..Default::default()
        }
    }

}

impl McpFactoryConfig {
    /// Create a disabled MCP configuration
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// Tool factory for creating configured tool instances
pub struct ToolFactory;

/// Result of MCP discovery
#[derive(Debug, Clone)]
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

/// Result of custom tools discovery
#[derive(Debug, Clone)]
pub struct CustomToolsDiscoveryResult {
    /// Number of custom tools discovered
    pub tools_discovered: usize,
    /// Number of custom tools loaded
    pub tools_loaded: usize,
    /// Tool names that failed to load
    pub failed_tools: Vec<(String, String)>,
}

impl CustomToolsDiscoveryResult {
    /// Check if any custom tools were loaded
    #[must_use]
    pub fn has_custom_tools(&self) -> bool {
        self.tools_loaded > 0
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
    /// Custom tools discovery result
    pub custom: CustomToolsDiscoveryResult,
}

impl std::fmt::Debug for ToolCreationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCreationResult")
            .field("tool_count", &self.tools.len())
            .field("disabled", &self.disabled)
            .field("mcp", &self.mcp)
            .field("custom", &self.custom)
            .finish()
    }
}

impl ToolFactory {
    /// Check if a tool is disabled (case-insensitive)
    fn is_disabled(disabled_tools: &[String], tool_name: &str) -> bool {
        disabled_tools
            .iter()
            .any(|d| d.to_lowercase() == tool_name.to_lowercase())
    }

    /// Register built-in tools with ExtensionCore
    ///
    /// Uses BuiltinRegistry to register all enabled built-in tools,
    /// making them discoverable via the ToolRegister hook and executable via
    /// the ToolExecute hook.
    async fn register_builtin_tools_with_extension_core(
        config: &ToolFactoryConfig,
    ) -> anyhow::Result<()> {
        let core = match global_core() {
            Some(core) => core,
            None => {
                tracing::debug!("ExtensionCore not initialized, skipping built-in tool registration");
                return Ok(());
            }
        };

        // Use BuiltinRegistry for centralized registration
        use crate::tools::builtin_registry::{BuiltinRegistry, BuiltinRegistryConfig};
        
        let registry_config = BuiltinRegistryConfig {
            workspace_dir: config.workspace_dir.clone(),
            enable_granular_fs: config.enable_granular_fs,
            enable_granular_write: config.enable_granular_write,
            enable_shell: config.enable_shell,
            enable_session_tools: config.enable_session_tools,
            enable_cron: config.enable_cron,
            cron_db_path: config.cron_db_path.clone(),
            instance_id: config.instance_id.clone(),
            disabled_tools: config.disabled_tools.clone(),
        };

        BuiltinRegistry::register(&core, &registry_config).await?;

        tracing::info!("Registered built-in tools with ExtensionCore");
        Ok(())
    }

    /// Load extension tools from the unified ExtensionCore registry
    async fn load_extension_tools(
        disabled_tools: &[String],
        existing_names: &std::collections::HashSet<String>,
    ) -> Vec<Arc<dyn crate::tools::Tool>> {
        use crate::extensions::core::global_core;
        
        // Get the global extension core
        let core = match global_core() {
            Some(core) => core,
            None => {
                tracing::debug!("No ExtensionCore initialized, skipping extension tools");
                return vec![];
            }
        };
        
        // Get all registered tools from the unified registry
        let tool_defs = core.list_tool_definitions().await;
        
        let mut tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();
        
        for def in tool_defs {
            let name = def.name.to_lowercase();
            
            // Skip disabled tools
            if disabled_tools.iter().any(|d| d.to_lowercase() == name) {
                tracing::debug!("Extension tool '{}' is disabled, skipping", def.name);
                continue;
            }
            
            // Skip tools that conflict with existing tools
            if existing_names.contains(&name) {
                tracing::debug!(
                    "Extension tool '{}' conflicts with existing tool, skipping",
                    def.name
                );
                continue;
            }
            
            // Create an ExtensionAsyncTool that will invoke the ToolExecute hook
            tools.push(ToolFactory::create_extension_tool(&core, def).await);
        }
        
        tracing::info!("Loaded {} tools from Extension Framework", tools.len());
        tools
    }
    
    /// Create an ExtensionAsyncTool that invokes the Extension Framework
    async fn create_extension_tool(
        _core: &Arc<crate::extensions::core::ExtensionCore>,
        def: crate::providers::ToolDefinition,
    ) -> Arc<dyn crate::tools::Tool> {
        // For now, create a simple wrapper that will invoke the ToolExecute hook
        // The actual execution will be handled by the ExtensionAsyncAdapter
        use crate::extensions::async_integration::ExtensionAsyncTool;
        use crate::extensions::core::ExtensionAsyncAdapter;
        
        let adapter = ExtensionAsyncAdapter::new(_core.clone());
        let tool = ExtensionAsyncTool::new(
            adapter,
            def.name,
            def.description,
            def.parameters,
        );
        
        Arc::new(tool) as Arc<dyn crate::tools::Tool>
    }

    /// Create all essential tools based on configuration (synchronous version)
    ///
    /// Respects `disabled_tools` configuration - disabled tools are excluded
    /// from the returned list and won't be shown to the LLM.
    #[must_use]
    pub fn create_tools(config: &ToolFactoryConfig) -> ToolCreationResult {
        let mut registry = DisabledToolFilter::new(&config.disabled_tools);

        // Filesystem tool
        // Granular filesystem tools
        registry.register("read_file", config.enable_granular_fs, || {
            Arc::new(ReadFileTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register("write_file", config.enable_granular_fs && config.enable_granular_write, || {
            Arc::new(WriteFileTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register("glob", config.enable_granular_fs, || {
            Arc::new(GlobTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register("grep", config.enable_granular_fs, || {
            Arc::new(GrepTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register("str_replace_file", config.enable_granular_fs && config.enable_granular_write, || {
            Arc::new(StrReplaceFileTool::new().with_workspace(config.workspace_dir.clone()))
        });

        // Shell tool
        let shell_enabled = config.enable_shell;
        let shell_disabled = registry.is_disabled("shell");
        if shell_enabled {
            if shell_disabled {
                registry.disabled.push("shell".to_string());
            } else {
                registry.tools.push(Arc::new(
                    ShellTool::new().with_workspace(config.workspace_dir.clone()),
                ));
            }
        }

        // Session introspection tools (grouped)
        if config.enable_session_tools {
            registry.register("sessions_list", true, || {
                Arc::new(SessionsListTool::new(Box::new(
                    crate::tools::InMemorySessionRegistry::new("main".to_string()),
                )))
            });

            registry.register("sessions_history", true, || {
                Arc::new(SessionsHistoryTool::new(Box::new(
                    crate::tools::InMemorySessionRegistry::new("main".to_string()),
                )))
            });

            registry.register("session_status", true, || {
                Arc::new(SessionStatusTool::new(Box::new(
                    crate::tools::InMemorySessionRegistry::new("main".to_string()),
                )))
            });
        }

        // Cron tool for scheduled jobs
        registry.register("cron", config.enable_cron, || {
            let db_path = config
                .cron_db_path
                .clone()
                .unwrap_or_else(|| config.workspace_dir.join("cron.json"));
            let instance_id = config
                .instance_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            Arc::new(CronTool::new(db_path, instance_id))
        });

        let (tools, disabled) = registry.build();

        // Apply ToolWrapper if enabled
        let tools = if config.enable_wrapper {
            Self::wrap_tools(tools, &config.wrapper_config)
        } else {
            tools
        };

        ToolCreationResult {
            tools,
            disabled,
            mcp: McpDiscoveryResult {
                servers_found: 0,
                tools_available: 0,
                failed_servers: Vec::new(),
                auto_install_attempted: false,
            },
            custom: CustomToolsDiscoveryResult {
                tools_discovered: 0,
                tools_loaded: 0,
                failed_tools: Vec::new(),
            },
        }
    }

    /// Create all essential tools including custom and MCP tools (asynchronous version)
    ///
    /// This version follows the capability resolution order (CAPABILITY_INTERFACE.md §9.1):
    /// 1. Built-in tools (registered with ExtensionCore)
    /// 2. Custom tools from `tools/` directory (override built-ins if same name)
    /// 3. MCP tools from configured servers (override neither built-in nor custom)
    ///
    /// Note: Built-in tools take precedence. If a custom tool has the same name as
    /// a built-in tool, the built-in is kept and the custom tool is logged as skipped.
    ///
    /// Respects `disabled_tools` - disabled tools are excluded from all sources.
    pub async fn create_tools_async(
        config: &ToolFactoryConfig,
    ) -> anyhow::Result<ToolCreationResult> {
        // Step 0: Register built-in tools with ExtensionCore
        // This makes them discoverable via hooks and manageable via ExtensionManager
        Self::register_builtin_tools_with_extension_core(config).await?;

        // Step 1: Start with core built-in tools (synchronous creation for backward compat)
        let mut result = Self::create_tools(config);

        // Track built-in tool names for conflict resolution
        let builtin_names: std::collections::HashSet<String> = result
            .tools
            .iter()
            .map(|t| t.name().to_lowercase())
            .collect();

        // Step 2: Load custom tools from tools/ directory
        let mut custom_discovery = CustomToolsDiscoveryResult {
            tools_discovered: 0,
            tools_loaded: 0,
            failed_tools: Vec::new(),
        };

        if config.enable_custom_tools {
            let custom_tools_dir = config
                .custom_tools_dir
                .clone()
                .unwrap_or_else(|| config.workspace_dir.join("tools"));

            tracing::debug!("Discovering custom tools in {:?}", custom_tools_dir);

            match Self::load_custom_tools_with_discovery(
                &custom_tools_dir,
                &config.disabled_tools,
                &builtin_names,
            )
            .await
            {
                Ok((custom_tools, discovery)) => {
                    custom_discovery = discovery;
                    if custom_discovery.tools_loaded > 0 {
                        tracing::info!(
                            "✅ Loaded {} custom tools ({} discovered)",
                            custom_discovery.tools_loaded,
                            custom_discovery.tools_discovered
                        );
                        result.tools.extend(custom_tools);
                    } else if custom_discovery.tools_discovered > 0 {
                        tracing::info!(
                            "ℹ️ Discovered {} custom tools but none loaded",
                            custom_discovery.tools_discovered
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("❌ Custom tools discovery failed: {}", e);
                }
            }
        } else {
            tracing::debug!("Custom tools disabled");
        }

        result.custom = custom_discovery;

        // Step 3: Load MCP tools
        let mut mcp_discovery = McpDiscoveryResult {
            servers_found: 0,
            tools_available: 0,
            failed_servers: Vec::new(),
            auto_install_attempted: false,
        };

        if config.mcp.enabled {
            tracing::info!("Discovering MCP servers...");

            // Get current tool names (built-in + custom) for conflict resolution
            let existing_names: std::collections::HashSet<String> = result
                .tools
                .iter()
                .map(|t| t.name().to_lowercase())
                .collect();

            match Self::load_mcp_tools_with_discovery(
                &config.mcp,
                &config.disabled_tools,
                &existing_names,
            )
            .await
            {
                Ok((mcp_tools, mcp_result)) => {
                    let servers_found = mcp_result.servers_found;
                    mcp_discovery = mcp_result;
                    if !mcp_tools.is_empty() {
                        tracing::info!(
                            "✅ Loaded {} tools from {} MCP servers",
                            mcp_tools.len(),
                            servers_found
                        );
                        result.tools.extend(mcp_tools);
                    } else {
                        tracing::info!("ℹ️ No MCP servers configured or available");
                    }
                }
                Err(e) => {
                    tracing::warn!("❌ MCP discovery failed: {}", e);
                    tracing::info!("💡 Continuing without MCP tools. Install with:");
                    tracing::info!("   pekobot mcp install <web|browser|memory>");
                }
            }
        } else {
            tracing::debug!("MCP tools disabled");
        }

        result.mcp = mcp_discovery;
        
        // Step 4: Load tools from Extension Framework
        // Get current tool names (built-in + custom + MCP) for conflict resolution
        let existing_names: std::collections::HashSet<String> = result
            .tools
            .iter()
            .map(|t| t.name().to_lowercase())
            .collect();
        
        let ext_tools = Self::load_extension_tools(&config.disabled_tools, &existing_names).await;
        if !ext_tools.is_empty() {
            tracing::info!(
                "✅ Loaded {} tools from Extension Framework",
                ext_tools.len()
            );
            result.tools.extend(ext_tools);
        } else {
            tracing::debug!("ℹ️ No Extension Framework tools loaded");
        }
        
        // Apply ToolWrapper if enabled
        if config.enable_wrapper {
            result.tools = Self::wrap_tools(result.tools, &config.wrapper_config);
        }
        
        Ok(result)
    }

    /// Wrap tools with ToolWrapper for reserved parameter support
    fn wrap_tools(
        tools: Vec<Arc<dyn crate::tools::Tool>>,
        wrapper_config: &crate::tools::WrapperConfig,
    ) -> Vec<Arc<dyn crate::tools::Tool>> {
        let factory = crate::tools::ToolWrapperFactory::with_config(wrapper_config.clone());
        tools
            .into_iter()
            .map(|tool| Arc::new(factory.wrap(tool)) as Arc<dyn crate::tools::Tool>)
            .collect()
    }

    /// Load custom tools with discovery metadata (using Universal Tool Protocol)
    async fn load_custom_tools_with_discovery(
        tools_dir: &PathBuf,
        disabled_tools: &[String],
        existing_names: &std::collections::HashSet<String>,
    ) -> anyhow::Result<(Vec<Arc<dyn crate::tools::Tool>>, CustomToolsDiscoveryResult)> {
        if !tools_dir.exists() {
            tracing::debug!("Custom tools directory does not exist: {:?}", tools_dir);
            return Ok((
                Vec::new(),
                CustomToolsDiscoveryResult {
                    tools_discovered: 0,
                    tools_loaded: 0,
                    failed_tools: Vec::new(),
                },
            ));
        }

        // Use ExtensionManager for unified tool discovery
        use crate::extensions::adapters::BuiltInAdapters;
        use crate::extensions::manager::ExtensionManager;
        let mut manager = ExtensionManager::new();
        for adapter in BuiltInAdapters::new().adapters() {
            manager.register_adapter(adapter);
        }

        // Scan directory first to get count
        let discovered = manager.scan_directory(tools_dir).await?;
        let discovered_count = discovered.len();

        // Load extensions
        let loaded_ids = manager.load_from_directory(tools_dir).await?;

        // Get tools as Arc<dyn Tool>, filtering out disabled tools
        let disabled_set: std::collections::HashSet<String> = disabled_tools
            .iter()
            .map(|d| d.to_lowercase())
            .collect();
        let universal_tools = manager.get_tool_instances(|tool_name| {
            let name_lower = tool_name.to_lowercase();
            // Filter out disabled tools and tools that conflict with built-in tools
            !disabled_set.contains(&name_lower) && !existing_names.contains(&name_lower)
        }).await;

        // Tools are already filtered by get_tool_instances
        let loaded_tools: Vec<Arc<dyn crate::tools::Tool>> = universal_tools;
        let failed_tools: Vec<(String, String)> = Vec::new();

        let loaded_count = loaded_tools.len();

        Ok((
            loaded_tools,
            CustomToolsDiscoveryResult {
                tools_discovered: discovered_count,
                tools_loaded: loaded_count,
                failed_tools,
            },
        ))
    }

    /// Load MCP tools with discovery metadata
    async fn load_mcp_tools_with_discovery(
        mcp_config: &McpFactoryConfig,
        disabled_tools: &[String],
        existing_names: &std::collections::HashSet<String>,
    ) -> anyhow::Result<(Vec<Arc<dyn crate::tools::Tool>>, McpDiscoveryResult)> {
        use crate::mcp::{create_tool_proxies, ConfigFormat, McpConfig, McpManager};

        // Use centralized config path resolution
        let (config_path, format) = McpConfig::resolve_config_path(mcp_config.config_path.as_ref());

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
        tracing::debug!(
            "Loading MCP config from {:?} (format: {:?})",
            config_path,
            format
        );
        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: McpConfig = match format {
            ConfigFormat::Json => McpConfig::from_json(&content)?,
            ConfigFormat::Toml => toml::from_str(&content)?,
        };

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
            let manager_guard = manager.write().await;
            manager_guard.init().await
        };

        match init_result {
            Ok(()) => {
                // Create tool proxies
                let mcp_tools = create_tool_proxies(manager).await;

                // Filter out disabled MCP tools and handle name conflicts
                let mut filtered_tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();
                let mut conflict_count = 0;

                for tool in mcp_tools {
                    let name = tool.name().to_lowercase();

                    // Check if disabled
                    if disabled_tools.iter().any(|d| d.to_lowercase() == name) {
                        tracing::debug!("MCP tool '{}' is disabled, skipping", tool.name());
                        continue;
                    }

                    // Check for conflicts with existing tools (built-in or custom)
                    if existing_names.contains(&name) {
                        tracing::debug!(
                            "MCP tool '{}' conflicts with existing tool, skipping",
                            tool.name()
                        );
                        conflict_count += 1;
                        continue;
                    }

                    filtered_tools.push(tool);
                }

                let tools_count = filtered_tools.len();

                if conflict_count > 0 {
                    tracing::debug!("Skipped {} MCP tools due to name conflicts", conflict_count);
                }

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
        use crate::mcp::{create_tool_proxies, ConfigFormat, McpConfig, McpManager};

        // Use centralized config path resolution
        let (config_path, format) = McpConfig::resolve_config_path(mcp_config.config_path.as_ref());

        if !config_path.exists() {
            tracing::debug!("MCP config not found at {:?}", config_path);
            return Ok(Vec::new());
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: McpConfig = match format {
            ConfigFormat::Json => McpConfig::from_json(&content)?,
            ConfigFormat::Toml => toml::from_str(&content)?,
        };

        if config.servers.is_empty() {
            return Ok(Vec::new());
        }

        let manager = Arc::new(RwLock::new(McpManager::new(config)));
        
        // Initialize the manager (this starts auto-start servers)
        tracing::info!("Initializing MCP manager from {:?}...", config_path);
        manager.write().await.init().await?;
        tracing::info!("MCP manager initialized successfully");
        
        // List all tools from running servers
        let manager_guard = manager.read().await;
        let all_tools = manager_guard.list_all_tools().await;
        tracing::info!("Found {} tools from MCP servers: {:?}", all_tools.len(), 
            all_tools.iter().map(|(s, t)| format!("{}.{}", s, t.name)).collect::<Vec<_>>());
        drop(manager_guard);

        let tools = create_tool_proxies(manager).await;
        tracing::info!("Created {} MCP tool proxies", tools.len());
        Ok(tools)
    }

    /// Create minimal tools (filesystem + process only)
    /// Respects disabled_tools
    ///
    /// DEPRECATED: Use `create_tools` with `ToolFactoryConfig::minimal()` instead.
    #[deprecated(
        since = "0.9.0",
        note = "Use create_tools with ToolFactoryConfig::minimal() instead"
    )]
    #[must_use]
    pub fn create_minimal_tools(
        workspace_dir: PathBuf,
        disabled_tools: Vec<String>,
    ) -> ToolCreationResult {
        let config = ToolFactoryConfig {
            workspace_dir,
            enable_session_tools: false,
            enable_cron: false,
            disabled_tools,
            ..Default::default()
        };
        Self::create_tools(&config)
    }

    /// Create coding tools (granular filesystem + shell)
    /// Respects disabled_tools
    ///
    /// DEPRECATED: Use `create_tools` with `ToolFactoryConfig::coding()` instead.
    #[deprecated(
        since = "0.9.0",
        note = "Use create_tools with ToolFactoryConfig::coding() instead"
    )]
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
    ///
    /// DEPRECATED: Use `create_tools` with `ToolFactoryConfig::full()` instead.
    #[deprecated(
        since = "0.9.0",
        note = "Use create_tools with ToolFactoryConfig::full() instead"
    )]
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
    ///
    /// DEPRECATED: Use `create_tools_async` with `ToolFactoryConfig::full()` instead.
    #[deprecated(
        since = "0.9.0",
        note = "Use create_tools_async with ToolFactoryConfig::full() instead"
    )]
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
    ///
    /// Checks for both mcp.toml and mcp.json files.
    pub async fn should_use_mcp() -> bool {
        let base_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("mcp");

        // Check for mcp.toml
        let toml_path = base_path.with_extension("toml");
        if toml_path.exists() {
            match tokio::fs::read_to_string(&toml_path).await {
                Ok(content) => {
                    if let Ok(config) = toml::from_str::<crate::mcp::McpConfig>(&content) {
                        if !config.servers.is_empty() {
                            return true;
                        }
                    }
                }
                Err(_) => {}
            }
        }

        // Check for mcp.json
        let json_path = base_path.with_extension("json");
        if json_path.exists() {
            match tokio::fs::read_to_string(&json_path).await {
                Ok(content) => {
                    if let Ok(config) = crate::mcp::McpConfig::from_json(&content) {
                        return !config.servers.is_empty();
                    }
                }
                Err(_) => {}
            }
        }

        false
    }

    /// Get MCP config path (TOML format)
    pub fn mcp_config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("mcp.toml")
    }

    /// Get MCP config path (JSON format)
    pub fn mcp_config_json_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("mcp.json")
    }

    /// Get list of all available built-in tool names
    pub fn builtin_tool_names() -> Vec<&'static str> {
        vec![
            // Granular filesystem tools
            "read_file",
            "write_file",
            "glob",
            "grep",
            "str_replace_file",
            // Other tools
            "shell",
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
            disabled_tools: vec!["shell".to_string(), "cron".to_string()],
            ..Default::default()
        };

        let result = ToolFactory::create_tools(&config);

        // Check that shell and cron are in disabled list
        assert!(result.disabled.contains(&"shell".to_string()));
        assert!(result.disabled.contains(&"cron".to_string()));

        // Check that disabled tools are not in tools list
        let tool_names: Vec<_> = result.tools.iter().map(|t| t.name()).collect();
        assert!(!tool_names.contains(&"shell"));
        assert!(!tool_names.contains(&"cron"));
    }

    #[test]
    fn test_is_disabled_case_insensitive() {
        let disabled = vec!["Shell".to_string(), "CRON".to_string()];

        assert!(ToolFactory::is_disabled(&disabled, "shell"));
        assert!(ToolFactory::is_disabled(&disabled, "SHELL"));
        assert!(ToolFactory::is_disabled(&disabled, "cron"));
        assert!(!ToolFactory::is_disabled(&disabled, "read_file"));
    }

    #[test]
    fn test_validate_disabled_tools() {
        let disabled = vec![
            "shell".to_string(),
            "invalid_tool".to_string(),
            "read_file".to_string(),
        ];

        let invalid = ToolFactory::validate_disabled_tools(&disabled);
        assert!(invalid.contains(&"invalid_tool".to_string()));
        assert!(!invalid.contains(&"shell".to_string()));
        assert!(!invalid.contains(&"read_file".to_string()));
    }

    #[test]
    fn test_builtin_tool_names() {
        let names = ToolFactory::builtin_tool_names();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"cron"));
        assert!(names.contains(&"agent_spawn"));
        // Granular filesystem tools
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"str_replace_file"));
    }

    #[test]
    fn test_tool_wrapper_integration_enabled() {
        let config = ToolFactoryConfig {
            workspace_dir: PathBuf::from("."),
            enable_wrapper: true,
            ..Default::default()
        };

        let result = ToolFactory::create_tools(&config);

        // Tools should be wrapped (ToolWrapper implements Tool)
        // We can't directly check the type, but we can verify tools are created
        assert!(!result.tools.is_empty(), "Tools should be created");
        
        // Verify tools have proper names (wrapper delegates to inner)
        let tool_names: Vec<_> = result.tools.iter().map(|t| t.name().to_string()).collect();
        assert!(tool_names.contains(&"read_file".to_string()));
        assert!(tool_names.contains(&"glob".to_string()));
    }

    #[test]
    fn test_tool_wrapper_integration_disabled() {
        let config = ToolFactoryConfig {
            workspace_dir: PathBuf::from("."),
            enable_wrapper: false,
            ..Default::default()
        };

        let result = ToolFactory::create_tools(&config);

        // Tools should still be created, just not wrapped
        assert!(!result.tools.is_empty(), "Tools should be created");
        
        // Verify tools have proper names
        let tool_names: Vec<_> = result.tools.iter().map(|t| t.name().to_string()).collect();
        assert!(tool_names.contains(&"read_file".to_string()));
    }

    #[test]
    fn test_wrap_tools_helper() {
        use crate::tools::{ToolWrapper, WrapperConfig};

        // Create a simple tool
        let tools: Vec<Arc<dyn crate::tools::Tool>> = vec![
            Arc::new(ReadFileTool::new()),
        ];

        let config = WrapperConfig::default();
        let wrapped = ToolFactory::wrap_tools(tools, &config);

        assert_eq!(wrapped.len(), 1);
        // The wrapped tool should still expose the original name
        assert_eq!(wrapped[0].name(), "read_file");
        
        // Verify it's actually a ToolWrapper by checking type
        // ToolWrapper's parameters() should delegate to inner tool
        let params = wrapped[0].parameters();
        assert!(params.get("properties").is_some());
    }
}
