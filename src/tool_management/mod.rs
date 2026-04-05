//! Tool Management System
//!
//! Unified tool management consolidating MCP servers, Universal Tools, and
//! downloaded tools into a single system.
//!
//! # Architecture
//!
//! - **ToolCatalog** (trait): Read-only registry access - discovery, metadata, search
//! - **ToolLifecycle** (trait): Runtime management - start/stop/status of tool processes
//! - **ToolManager**: Coordinates Catalog + Lifecycle
//!
//! # Example
//!
//! ```ignore
//! let catalog = Arc::new(ToolCatalogImpl::new(paths));
//! let lifecycle = Arc::new(ToolLifecycleImpl::new(mcp_manager, universal_adapter));
//! let manager = ToolManager::new(catalog, lifecycle);
//!
//! let tools = manager.list_tools().await;
//! ```

pub mod types;
pub use types::*;

pub mod commands;

mod catalog;
pub use catalog::ToolCatalogImpl;

mod lifecycle;
pub use lifecycle::ToolLifecycleImpl;

use crate::common::paths::PathResolver;
use async_trait::async_trait;
use std::sync::Arc;

/// Read-only registry access for tool discovery and metadata
///
/// Implementors provide read-only access to the tool registry,
/// aggregating tools from all sources (MCP, Universal, downloaded).
#[async_trait]
pub trait ToolCatalog: Send + Sync {
    /// List all installed tools across all sources
    async fn list_installed(&self) -> Vec<InstalledToolInfo>;

    /// Get tool info by name
    async fn get_tool(&self, name: &str) -> Option<InstalledToolInfo>;

    /// Search remote registry for tools
    async fn search_registry(&self, query: &str) -> anyhow::Result<Vec<ToolSearchResult>>;

    /// List available tools from remote registry
    async fn list_available(&self) -> anyhow::Result<Vec<ToolSearchResult>>;

    /// Get tools by type
    async fn list_by_type(&self, tool_type: ToolType) -> Vec<InstalledToolInfo> {
        self.list_installed()
            .await
            .into_iter()
            .filter(|t| t.tool_type == tool_type)
            .collect()
    }
}

/// Runtime lifecycle management for tools
///
/// Implementors handle starting, stopping, and status checking
/// of running tool processes (MCP servers, Universal Tool subprocesses).
#[async_trait]
pub trait ToolLifecycle: Send + Sync {
    /// Start a tool by name
    async fn start(&self, name: &str) -> anyhow::Result<()>;

    /// Stop a tool by name
    async fn stop(&self, name: &str) -> anyhow::Result<()>;

    /// Restart a tool by name
    async fn restart(&self, name: &str) -> anyhow::Result<()>;

    /// Get status of a specific tool
    async fn status(&self, name: &str) -> anyhow::Result<ToolStatus>;

    /// List all tools with their current status
    async fn list_with_status(&self) -> Vec<ToolWithStatus>;
}

/// Coordinates Catalog and Lifecycle for unified tool management
pub struct ToolManager {
    catalog: Arc<dyn ToolCatalog>,
    lifecycle: Arc<dyn ToolLifecycle>,
}

impl ToolManager {
    /// Create a new ToolManager
    pub fn new(catalog: Arc<dyn ToolCatalog>, lifecycle: Arc<dyn ToolLifecycle>) -> Self {
        Self { catalog, lifecycle }
    }

    /// List all installed tools
    pub async fn list_tools(&self) -> Vec<InstalledToolInfo> {
        self.catalog.list_installed().await
    }

    /// List installed tools of a specific type
    pub async fn list_tools_by_type(&self, tool_type: ToolType) -> Vec<InstalledToolInfo> {
        self.catalog.list_by_type(tool_type).await
    }

    /// Get tool info by name
    pub async fn get_tool(&self, name: &str) -> Option<InstalledToolInfo> {
        self.catalog.get_tool(name).await
    }

    /// Search remote registry
    pub async fn search_registry(&self, query: &str) -> anyhow::Result<Vec<ToolSearchResult>> {
        self.catalog.search_registry(query).await
    }

    /// List available tools from remote registry
    pub async fn list_available(&self) -> anyhow::Result<Vec<ToolSearchResult>> {
        self.catalog.list_available().await
    }

    /// Start a tool
    pub async fn start_tool(&self, name: &str) -> anyhow::Result<()> {
        self.lifecycle.start(name).await
    }

    /// Stop a tool
    pub async fn stop_tool(&self, name: &str) -> anyhow::Result<()> {
        self.lifecycle.stop(name).await
    }

    /// Restart a tool
    pub async fn restart_tool(&self, name: &str) -> anyhow::Result<()> {
        self.lifecycle.restart(name).await
    }

    /// Get tool status
    pub async fn tool_status(&self, name: &str) -> anyhow::Result<ToolStatus> {
        self.lifecycle.status(name).await
    }

    /// List tools with runtime status
    pub async fn list_with_status(&self) -> Vec<ToolWithStatus> {
        self.lifecycle.list_with_status().await
    }
}

/// Builder for ToolManager with common path configurations
impl ToolManager {
    /// Create a ToolManager with default implementations
    pub fn with_defaults(path_resolver: PathResolver) -> Self {
        let catalog = Arc::new(ToolCatalogImpl::new(path_resolver.clone()));
        let lifecycle = Arc::new(ToolLifecycleImpl::uninitialized());
        Self::new(catalog, lifecycle)
    }
}
