//! Tool factory - Creates and configures all essential tools for agents
//!
//! This module provides centralized tool creation with proper configuration
//! from the agent config or environment.

use crate::security::SecurityPolicy;
use crate::tools::{
    ApplyPatchConfig, ApplyPatchTool, BrowserTool, FetchConfig, FetchTool,
    FileSystemTool, HttpTool, MemoryToolFactory, ProcessTool, SessionStatusTool,
    SessionsHistoryTool, SessionsListTool, WebSearchConfig, WebSearchTool,
};
use std::path::PathBuf;
use std::sync::Arc;

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
    /// Web search configuration
    pub web_search_config: Option<WebSearchConfig>,
    /// Fetch configuration
    pub fetch_config: Option<FetchConfig>,
    /// Apply patch configuration
    pub apply_patch_config: Option<ApplyPatchConfig>,
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
            web_search_config: None,
            fetch_config: None,
            apply_patch_config: None,
        }
    }
}

/// Tool factory for creating configured tool instances
pub struct ToolFactory;

impl ToolFactory {
    /// Create all essential tools based on configuration
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
                crate::tools::InMemorySessionRegistry::new("main".to_string())
            ))));
            tools.push(Arc::new(SessionsHistoryTool::new(Box::new(
                crate::tools::InMemorySessionRegistry::new("main".to_string())
            ))));
            tools.push(Arc::new(SessionStatusTool::new(Box::new(
                crate::tools::InMemorySessionRegistry::new("main".to_string())
            ))));
        }

        tools
    }

    /// Create minimal tools (filesystem + process only)
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

    /// Create coding tools (filesystem + apply_patch + process + web_search)
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
    pub fn create_full_tools(workspace_dir: PathBuf) -> Vec<Arc<dyn crate::tools::Tool>> {
        let config = ToolFactoryConfig {
            workspace_dir,
            ..Default::default()
        };
        Self::create_tools(&config)
    }
}
