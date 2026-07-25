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

use crate::tools::builtin::{
    BashTool, CronCreateTool, CronDeleteTool, CronListTool, EditTool, GlobTool, GrepTool, ReadTool,
    SessionTool, WriteTool,
};
use peko_tools_core::traits::Tool;
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
    /// When true, enables `Read`, Glob, Grep, `Write`, `Edit`
    pub enable_granular_fs: bool,
    /// Enable granular write tools (`Write`, `Edit`)
    /// Only effective when `enable_granular_fs` is true
    /// Defaults to true for full functionality, set to false for read-only
    pub enable_granular_write: bool,
    /// Enable shell tool (replaces process tool)
    pub enable_shell: bool,

    /// Enable session introspection tools
    pub enable_session_tools: bool,
    /// Enable cron tool
    pub enable_cron: bool,
    /// Enable async execution control tools
    pub enable_async_tools: bool,
    /// Enable planning todo tools
    pub enable_task_tools: bool,
    /// Path to cron database (defaults to `workspace_dir/cron.json`)
    pub cron_db_path: Option<PathBuf>,
    /// MCP configuration
    pub mcp: McpFactoryConfig,
    /// List of disabled tool names (e.g., ["Bash", "CronCreate"])
    pub disabled_tools: Vec<String>,
    /// Instance ID for cron persistence
    pub instance_id: Option<String>,
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
            enable_async_tools: true,
            enable_task_tools: true,
            cron_db_path: None,
            mcp: McpFactoryConfig::default(),
            disabled_tools: Vec::new(),
            instance_id: None,
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
            enable_granular_write: false, // Read-only: no Write or Edit
            enable_shell: true,
            enable_session_tools: false,
            enable_cron: false,
            enable_async_tools: false,
            enable_task_tools: false,
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
    pub tools: Vec<Arc<dyn peko_tools_core::Tool>>,
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
        registry.register("Read", config.enable_granular_fs, || {
            Arc::new(ReadTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register(
            "Write",
            config.enable_granular_fs && config.enable_granular_write,
            || Arc::new(WriteTool::new().with_workspace(config.workspace_dir.clone())),
        );

        registry.register("Glob", config.enable_granular_fs, || {
            Arc::new(GlobTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register("Grep", config.enable_granular_fs, || {
            Arc::new(GrepTool::new().with_workspace(config.workspace_dir.clone()))
        });

        registry.register(
            "Edit",
            config.enable_granular_fs && config.enable_granular_write,
            || Arc::new(EditTool::new().with_workspace(config.workspace_dir.clone())),
        );

        // Shell tool (Bash)
        let bash_enabled = config.enable_shell;
        let bash_disabled = registry.is_disabled("bash");
        if bash_enabled {
            if bash_disabled {
                registry.disabled.push("bash".to_string());
            } else {
                registry.tools.push(Arc::new(
                    BashTool::new().with_workspace(config.workspace_dir.clone()),
                ));
            }
        }

        // Session introspection tool (unified)
        if config.enable_session_tools {
            registry.register("session", true, || {
                // Phase 10d: `SessionTool` now lives in peko_tools_builtin and
                // takes a `SharedSessionRuntime` (Arc<dyn SessionRuntime>).
                // The legacy placeholder `SessionCache` is provided by
                // peko_tools_builtin and exposed here for back-compat.
                let cache = std::sync::Arc::new(crate::tools::SessionCache::new("main"));
                Arc::new(SessionTool::new(
                    cache.as_shared() as peko_tools_builtin::session::SharedSessionRuntime
                ))
            });
        }

        // Cron family for scheduled jobs
        if config.enable_cron {
            let cron_disabled = registry.is_disabled("cron");
            let create_disabled = registry.is_disabled("croncreate");
            let delete_disabled = registry.is_disabled("crondelete");
            let list_disabled = registry.is_disabled("cronlist");

            if cron_disabled || create_disabled {
                registry.disabled.push("CronCreate".to_string());
            } else {
                registry.tools.push(Arc::new(CronCreateTool::new()));
            }

            if cron_disabled || delete_disabled {
                registry.disabled.push("CronDelete".to_string());
            } else {
                registry.tools.push(Arc::new(CronDeleteTool::new()));
            }

            if cron_disabled || list_disabled {
                registry.disabled.push("CronList".to_string());
            } else {
                registry.tools.push(Arc::new(CronListTool::new()));
            }
        }

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
            disabled_tools: vec!["bash".to_string(), "cron".to_string()],
            ..Default::default()
        };

        let result = ToolFactory::create_tools(&config);

        // Check that bash and cron family are in disabled list
        assert!(result.disabled.contains(&"bash".to_string()));
        assert!(result.disabled.contains(&"CronCreate".to_string()));
        assert!(result.disabled.contains(&"CronDelete".to_string()));
        assert!(result.disabled.contains(&"CronList".to_string()));

        // Check that disabled tools are not in tools list
        let tool_names: Vec<_> = result.tools.iter().map(|t| t.name()).collect();
        assert!(!tool_names.contains(&"bash"));
        assert!(!tool_names.contains(&"Bash"));
        assert!(!tool_names.contains(&"CronCreate"));
        assert!(!tool_names.contains(&"CronDelete"));
        assert!(!tool_names.contains(&"CronList"));
    }

    // Dead tests for removed methods (is_disabled, validate_disabled_tools,
    // builtin_tool_names, ToolWrapper) have been deleted in ADR-019 cleanup.
}
