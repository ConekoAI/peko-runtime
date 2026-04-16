//! Built-in Tool Registry
//!
//! Provides a centralized registry for all built-in tools.
//! Built-in tools are registered with ExtensionCore and become discoverable
//! via the Extension Framework's hooks.
//!
//! This module provides:
//! - Definition of all built-in tools
//! - Registration with ExtensionCore via BuiltinToolAdapter
//! - Enable/disable configuration support

use crate::extensions::adapters::BuiltinToolAdapter;
use crate::extensions::core::ExtensionCore;
use crate::tools::{
    CronTool, GlobTool, GrepTool, ReadFileTool, SessionStatusTool, SessionsHistoryTool,
    SessionsListTool, ShellTool, StrReplaceFileTool, WriteFileTool,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Configuration for built-in tool registration
#[derive(Debug, Clone)]
pub struct BuiltinRegistryConfig {
    /// Workspace directory for tools
    pub workspace_dir: PathBuf,
    /// Enable granular filesystem tools (read_file, write_file, glob, grep, str_replace_file)
    pub enable_granular_fs: bool,
    /// Enable write tools (write_file, str_replace_file)
    pub enable_granular_write: bool,
    /// Enable shell tool
    pub enable_shell: bool,
    /// Enable session introspection tools
    pub enable_session_tools: bool,
    /// Enable cron tool
    pub enable_cron: bool,
    /// Path to cron database
    pub cron_db_path: Option<PathBuf>,
    /// Instance ID for cron persistence
    pub instance_id: Option<String>,
    /// List of disabled tool names
    pub disabled_tools: Vec<String>,
}

impl Default for BuiltinRegistryConfig {
    fn default() -> Self {
        Self {
            workspace_dir: PathBuf::from("."),
            enable_granular_fs: true,
            enable_granular_write: true,
            enable_shell: true,
            enable_session_tools: true,
            enable_cron: true,
            cron_db_path: None,
            instance_id: None,
            disabled_tools: Vec::new(),
        }
    }
}

/// Built-in tool registry
///
/// Centralizes registration of all built-in tools with ExtensionCore.
pub struct BuiltinRegistry;

impl BuiltinRegistry {
    /// Register all enabled built-in tools with ExtensionCore
    ///
    /// This is the single entry point for registering built-in tools.
    /// All tools are registered as hooks in ExtensionCore, making them
    /// discoverable via ToolRegister hook and executable via ToolExecute hook.
    pub async fn register(
        core: &ExtensionCore,
        config: &BuiltinRegistryConfig,
    ) -> anyhow::Result<()> {
        let disabled_set: HashSet<String> = config
            .disabled_tools
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        let workspace = config.workspace_dir.clone();

        // Shell tool
        let shell_enabled = config.enable_shell;
        let shell_disabled = disabled_set.contains("shell");
        if shell_enabled && !shell_disabled {
            let shell = Arc::new(ShellTool::new().with_workspace(&workspace));
            BuiltinToolAdapter::register_tool(core, shell).await?;
        }

        // Granular filesystem tools
        if config.enable_granular_fs {
            // read_file
            if !disabled_set.contains("read_file") {
                let tool = Arc::new(ReadFileTool::new().with_workspace(&workspace));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }

            // write_file
            if config.enable_granular_write && !disabled_set.contains("write_file") {
                let tool = Arc::new(WriteFileTool::new().with_workspace(&workspace));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }

            // glob
            if !disabled_set.contains("glob") {
                let tool = Arc::new(GlobTool::new().with_workspace(&workspace));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }

            // grep
            if !disabled_set.contains("grep") {
                let tool = Arc::new(GrepTool::new().with_workspace(&workspace));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }

            // str_replace_file
            if config.enable_granular_write && !disabled_set.contains("str_replace_file") {
                let tool = Arc::new(StrReplaceFileTool::new().with_workspace(&workspace));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }
        }

        // Session introspection tools
        if config.enable_session_tools {
            if !disabled_set.contains("sessions_list") {
                let registry = crate::tools::InMemorySessionRegistry::new("main".to_string());
                let tool = Arc::new(SessionsListTool::new(Box::new(registry)));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }

            if !disabled_set.contains("sessions_history") {
                let registry = crate::tools::InMemorySessionRegistry::new("main".to_string());
                let tool = Arc::new(SessionsHistoryTool::new(Box::new(registry)));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }

            if !disabled_set.contains("session_status") {
                let registry = crate::tools::InMemorySessionRegistry::new("main".to_string());
                let tool = Arc::new(SessionStatusTool::new(Box::new(registry)));
                BuiltinToolAdapter::register_tool(core, tool).await?;
            }
        }

        // Cron tool
        if config.enable_cron && !disabled_set.contains("cron") {
            let db_path = config
                .cron_db_path
                .clone()
                .unwrap_or_else(|| workspace.join("cron.json"));
            let instance_id = config
                .instance_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            let tool = Arc::new(CronTool::new(db_path, instance_id));
            BuiltinToolAdapter::register_tool(core, tool).await?;
        }

        Ok(())
    }

    /// Get list of all built-in tool names
    pub fn all_tool_names() -> Vec<&'static str> {
        vec![
            "shell",
            "read_file",
            "write_file",
            "glob",
            "grep",
            "str_replace_file",
            "sessions_list",
            "sessions_history",
            "session_status",
            "cron",
        ]
    }

    /// Check if a tool name is a built-in tool
    pub fn is_builtin(name: &str) -> bool {
        let name_lower = name.to_lowercase();
        Self::all_tool_names()
            .iter()
            .any(|&n| n.to_lowercase() == name_lower)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_builtin() {
        assert!(BuiltinRegistry::is_builtin("shell"));
        assert!(BuiltinRegistry::is_builtin("read_file"));
        assert!(BuiltinRegistry::is_builtin("SHELL")); // case insensitive
        assert!(!BuiltinRegistry::is_builtin("unknown_tool"));
    }

    #[test]
    fn test_all_tool_names() {
        let names = BuiltinRegistry::all_tool_names();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"read_file"));
    }
}
