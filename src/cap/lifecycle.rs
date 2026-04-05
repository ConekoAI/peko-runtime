//! Capability Lifecycle Implementation
//!
//! Coordinates runtime lifecycle of capabilities:
//! - MCP servers: managed by McpManager (start/stop/restart)
//! - Universal Capabilities: process-per-call, no persistent state
//!
//! Note: Built-in and downloaded capabilities don't have a runtime lifecycle
//! (they're either part of the binary or downloaded packages).

use crate::cap::{CapabilityStatus, CapabilityWithStatus};
use crate::mcp::manager::McpManager;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Capability lifecycle implementation
///
/// Manages the runtime lifecycle of capabilities that have persistent processes.
/// For capabilities without persistent state (Universal Capabilities, built-ins), operations
/// return errors indicating they don't support lifecycle management.
pub struct CapabilityLifecycleImpl {
    /// MCP manager for MCP server lifecycle
    mcp_manager: Arc<RwLock<Option<McpManager>>>,
}

impl CapabilityLifecycleImpl {
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
impl crate::cap::CapabilityLifecycle for CapabilityLifecycleImpl {
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

    async fn status(&self, name: &str) -> anyhow::Result<CapabilityStatus> {
        let mcp_ref = self.get_mcp_manager().await;
        match mcp_ref {
            Some(manager_ref) => {
                let guard = manager_ref.read().await;
                if let Some(manager) = guard.as_ref() {
                    let servers = manager.list_servers().await;
                    if let Some(state) = servers.iter().find(|s| s.name == name) {
                        if state.running {
                            if state.healthy {
                                Ok(CapabilityStatus::Running)
                            } else if let Some(ref err) = state.last_error {
                                Ok(CapabilityStatus::Error(err.clone()))
                            } else {
                                Ok(CapabilityStatus::Running) // Running but unhealthy
                            }
                        } else {
                            Ok(CapabilityStatus::Stopped)
                        }
                    } else {
                        Ok(CapabilityStatus::Unknown)
                    }
                } else {
                    Err(anyhow::anyhow!("MCP manager not initialized"))
                }
            }
            None => Ok(CapabilityStatus::Unknown),
        }
    }

    async fn list_with_status(&self) -> Vec<CapabilityWithStatus> {
        // For now, return empty - would need catalog access to list all configured capabilities
        // and then check status for each
        Vec::new()
    }
}
