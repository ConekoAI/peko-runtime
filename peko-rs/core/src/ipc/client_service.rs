//! Daemon Client Service
//!
//! High-level service wrapper around [`DaemonClient`] that provides
//! ergonomic async methods for extension runtime lifecycle operations.
//! This keeps IPC concerns out of the commands layer.

use super::{DaemonClient, ResponsePacket};

/// Runtime status for an extension background process
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    /// Current state (e.g., "running", "stopped", "crashed")
    pub state: String,
    /// Number of times the runtime has been restarted
    pub restart_count: u32,
    /// Last error message, if any
    pub last_error: Option<String>,
}

/// High-level service for daemon IPC operations
pub struct DaemonClientService;

impl DaemonClientService {
    /// Start a background runtime for an extension
    ///
    /// Returns the extension ID on success.
    pub async fn ext_start(id: &str) -> anyhow::Result<String> {
        let client = DaemonClient::connect().await?;
        match client.ext_start(id).await? {
            ResponsePacket::ExtStarted { extension_id, .. } => Ok(extension_id),
            ResponsePacket::Error { message, .. } => {
                anyhow::bail!("Failed to start '{}': {}", id, message)
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }

    /// Stop a background runtime for an extension
    ///
    /// Returns the extension ID on success.
    pub async fn ext_stop(id: &str) -> anyhow::Result<String> {
        let client = DaemonClient::connect().await?;
        match client.ext_stop(id).await? {
            ResponsePacket::ExtStopped { extension_id, .. } => Ok(extension_id),
            ResponsePacket::Error { message, .. } => {
                anyhow::bail!("Failed to stop '{}': {}", id, message)
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }

    /// Restart a background runtime for an extension
    ///
    /// Returns the extension ID on success.
    pub async fn ext_restart(id: &str) -> anyhow::Result<String> {
        let client = DaemonClient::connect().await?;
        match client.ext_restart(id).await? {
            ResponsePacket::ExtRestarted { extension_id, .. } => Ok(extension_id),
            ResponsePacket::Error { message, .. } => {
                anyhow::bail!("Failed to restart '{}': {}", id, message)
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }

    /// Get background runtime status for an extension
    pub async fn ext_status(id: &str) -> anyhow::Result<RuntimeStatus> {
        let client = DaemonClient::connect().await?;
        match client.ext_status(id).await? {
            ResponsePacket::ExtStatus {
                extension_id: _,
                state,
                restart_count,
                last_error,
                ..
            } => Ok(RuntimeStatus {
                state,
                restart_count,
                last_error,
            }),
            ResponsePacket::Error { message, .. } => {
                anyhow::bail!("Failed to get status for '{}': {}", id, message)
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }
}
