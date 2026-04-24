//! Tool factory - Creates and configures all essential tools for agents
//!
//! This module provides centralized tool creation with proper configuration
//! from the agent config or environment.
//!
//! Note: Heavy tools (`web_search`, fetch, http, browser, memory) have been
//! migrated to standalone MCP servers. Use MCP configuration to enable them.
//!
//! # Tool Loading Strategy (Resolution Order)
//!
//! Per `CAPABILITY_INTERFACE.md` §9.1, tools are resolved in this order:
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
    CronTool, GlobTool, GrepTool, ReadFileTool, SessionStatusTool, SessionsHistoryTool,
    SessionsListTool, ShellTool, StrReplaceFileTool, WriteFileTool,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

// Extension Framework integration

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
        let disabled_set: HashSet<String> =
            disabled_tools.iter().map(|s| s.to_lowercase()).collect();

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
    /// When true, enables `ReadFile`, Glob, Grep, `WriteFile`, `StrReplaceFile`
    pub enable_granular_fs: bool,
    /// Enable granular write tools (`WriteFile`, `StrReplaceFile`)
    /// Only effective when `enable_granular_fs` is true
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
}

impl Default for ToolFactoryConfig {
    fn default() -> Self {
        Self {
            workspace_dir: PathBuf::from("."),
            enable_granular_fs: true,    // Enabled by default
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
        }
    }
}

impl ToolFactoryConfig {
    /// Create a minimal configuration (read-only filesystem + shell)
    ///
    /// Use this for restricted environments where only basic file reading
    /// and shell operations are needed.
    #[must_use]
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
    #[must_use]
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
    /// This enables all built-in tools except MCP.
    #[must_use]
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
    #[must_use]
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

        registry.register(
            "write_file",
            config.enable_granular_fs && config.enable_granular_write,
            || Arc::new(WriteFileTool::new().with_workspace(config.workspace_dir.clone())),
        );

        registry.register("glob", config.enable_granular_fs, || {
            Arc::new(GlobTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register("grep", config.enable_granular_fs, || {
            Arc::new(GrepTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register(
            "str_replace_file",
            config.enable_granular_fs && config.enable_granular_write,
            || Arc::new(StrReplaceFileTool::new().with_workspace(config.workspace_dir.clone())),
        );

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
                    crate::tools::SessionCache::new("main"),
                )))
            });

            registry.register("sessions_history", true, || {
                Arc::new(SessionsHistoryTool::new(Box::new(
                    crate::tools::SessionCache::new("main"),
                )))
            });

            registry.register("session_status", true, || {
                Arc::new(SessionStatusTool::new(Box::new(
                    crate::tools::SessionCache::new("main"),
                )))
            });
        }

        // Cron tool for scheduled jobs
        registry.register("cron", config.enable_cron, || {
            Arc::new(CronTool::new())
        });

        let (tools, disabled) = registry.build();

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

    // Dead tests for removed methods (is_disabled, validate_disabled_tools,
    // builtin_tool_names, ToolWrapper) have been deleted in ADR-019 cleanup.
}
