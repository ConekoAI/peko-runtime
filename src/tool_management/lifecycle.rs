//! Tool Lifecycle Implementation
//!
//! Coordinates runtime lifecycle of tools:
//! - MCP servers: managed by McpManager (start/stop/restart)
//! - Universal Tools: process-per-call, no persistent state
//!
//! Note: Built-in and downloaded tools don't have a runtime lifecycle
//! (they're either part of the binary or downloaded packages).

use crate::mcp::manager::{ManagerError, McpManager};
use crate::tool_management::{ToolStatus, ToolWithStatus};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Tool lifecycle implementation
///
/// Manages the runtime lifecycle of tools that have persistent processes.
/// For tools without persistent state (Universal Tools, built-ins), operations
/// return errors indicating they don't support lifecycle management.
pub struct ToolLifecycleImpl {
    /// MCP manager for MCP server lifecycle
    mcp_manager: Arc<RwLock<Option<McpManager>>>,
}

impl ToolLifecycleImpl {
    /// Create an uninitialized lifecycle manager
    ///
    /// The MCP manager must be set before lifecycle operations can be performed.
    pub fn uninitialized() -> Self {
        Self {
            mcp_manager: Arc::new(RwLock::new(None)),
        }
    }

    /// Create with an existing MCP manager
    pub fn with_mcp_manager(mcp_manager: McpManager) -> Self {
        Self {
            mcp_manager: Arc::new(RwLock::new(Some(mcp_manager))),
        }
    }

    /// Set the MCP manager
    pub async fn set_mcp_manager(&self, manager: McpManager) {
        let mut guard = self.mcp_manager.write().await;
        *guard = Some(manager);
    }

    /// Check if an MCP manager is available
    pub async fn has_mcp_manager(&self) -> bool {
        let guard = self.mcp_manager.read().await;
        guard.is_some()
    }

    /// Get a cloned reference to the MCP manager if available
    async fn get_mcp_manager(&self) -> Option<Arc<RwLock<Option<McpManager>>>> {
        let guard = self.mcp_manager.read().await;
        if guard.is_some() {
            Some(Arc::clone(&self.mcp_manager))
        } else {
            None
        }
    }
}

#[async_trait]
impl crate::tool_management::ToolLifecycle for ToolLifecycleImpl {
    async fn start(&self, name: &str) -> anyhow::Result<()> {
        let mcp_ref = self.get_mcp_manager().await;
        match mcp_ref {
            Some(manager_ref) => {
                let guard = manager_ref.read().await;
                if let Some(manager) = guard.as_ref() {
                    manager.start_server(name).await.map_err(|e| anyhow::anyhow!("{}", e))
                } else {
                    Err(anyhow::anyhow!("MCP manager not initialized"))
                }
            }
            None => Err(anyhow::anyhow!(
                "MCP manager not available. Start/stop only supported for MCP servers."
            )),
        }
    }

    async fn stop(&self, name: &str) -> anyhow::Result<()> {
        let mcp_ref = self.get_mcp_manager().await;
        match mcp_ref {
            Some(manager_ref) => {
                let guard = manager_ref.read().await;
                if let Some(manager) = guard.as_ref() {
                    manager.stop_server(name).await.map_err(|e| anyhow::anyhow!("{}", e))
                } else {
                    Err(anyhow::anyhow!("MCP manager not initialized"))
                }
            }
            None => Err(anyhow::anyhow!(
                "MCP manager not available. Start/stop only supported for MCP servers."
            )),
        }
    }

    async fn restart(&self, name: &str) -> anyhow::Result<()> {
        let mcp_ref = self.get_mcp_manager().await;
        match mcp_ref {
            Some(manager_ref) => {
                let guard = manager_ref.read().await;
                if let Some(manager) = guard.as_ref() {
                    manager.restart_server(name).await.map_err(|e| anyhow::anyhow!("{}", e))
                } else {
                    Err(anyhow::anyhow!("MCP manager not initialized"))
                }
            }
            None => Err(anyhow::anyhow!(
                "MCP manager not available. Start/stop only supported for MCP servers."
            )),
        }
    }

    async fn status(&self, name: &str) -> anyhow::Result<ToolStatus> {
        let mcp_ref = self.get_mcp_manager().await;
        match mcp_ref {
            Some(manager_ref) => {
                let guard = manager_ref.read().await;
                if let Some(manager) = guard.as_ref() {
                    let servers = manager.list_servers().await;
                    if let Some(state) = servers.iter().find(|s| s.name == name) {
                        if state.running {
                            if state.healthy {
                                Ok(ToolStatus::Running)
                            } else if let Some(ref err) = state.last_error {
                                Ok(ToolStatus::Error(err.clone()))
                            } else {
                                Ok(ToolStatus::Running) // Running but unhealthy
                            }
                        } else {
                            Ok(ToolStatus::Stopped)
                        }
                    } else {
                        Ok(ToolStatus::Unknown)
                    }
                } else {
                    Err(anyhow::anyhow!("MCP manager not initialized"))
                }
            }
            None => Ok(ToolStatus::Unknown),
        }
    }

    async fn list_with_status(&self) -> Vec<ToolWithStatus> {
        // For now, return empty - would need catalog access to list all configured tools
        // and then check status for each
        Vec::new()
    }
}

