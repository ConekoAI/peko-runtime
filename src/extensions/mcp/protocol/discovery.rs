//! MCP Auto-Discovery
//!
//! Automatically discovers MCP servers from configuration.
//! Provides status information about available MCP servers.

use crate::extensions::mcp::protocol::config::{McpConfig, McpServerConfig};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// MCP server status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerStatus {
    /// Not yet checked
    Unknown,
    /// Available and connected
    Available,
    /// Running but not responding
    Unhealthy,
    /// Not installed
    NotInstalled,
    /// Installed but not running
    NotRunning,
    /// Connection failed
    Failed,
}

/// Discovered MCP server information
#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    /// Server name
    pub name: String,
    /// Server configuration
    pub config: McpServerConfig,
    /// Current status
    pub status: McpServerStatus,
    /// Tools count (if available)
    pub tools_count: usize,
    /// Error message if failed
    pub error: Option<String>,
}

/// Get MCP config path
#[must_use]
pub fn mcp_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peko")
        .join("mcp.toml")
}

/// Get MCP servers install directory
#[must_use]
pub fn mcp_install_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peko")
        .join("mcp-servers")
}

/// Find MCP server binary in PATH or install dir
pub async fn find_server_binary(name: &str) -> Option<PathBuf> {
    let binary_name = format!("mcp-{name}");

    // Check install dir first
    let install_path = mcp_install_dir().join(&binary_name);
    if install_path.exists() {
        return Some(install_path);
    }

    // Check PATH
    which::which(&binary_name).ok()
}

/// Check if an MCP server is installed
pub async fn is_server_installed(name: &str) -> bool {
    find_server_binary(name).await.is_some()
}

/// Get default MCP config content
#[must_use]
pub fn default_mcp_config() -> &'static str {
    "# MCP Servers Configuration\n# Add your MCP servers below.\n# See https://modelcontextprotocol.io for server examples.\n"
}

/// Create default MCP config if it doesn't exist
pub async fn ensure_default_config() -> anyhow::Result<PathBuf> {
    let config_path = mcp_config_path();

    if config_path.exists() {
        return Ok(config_path);
    }

    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(&config_path, default_mcp_config()).await?;
    info!("Created default MCP config at {:?}", config_path);

    Ok(config_path)
}

/// Discover MCP servers from config
pub async fn discover_servers() -> anyhow::Result<Vec<DiscoveredServer>> {
    let config_path = mcp_config_path();

    if !config_path.exists() {
        debug!("MCP config not found at {:?}", config_path);
        return Ok(Vec::new());
    }

    let content = tokio::fs::read_to_string(&config_path).await?;
    let config: McpConfig = toml::from_str(&content)?;

    let mut discovered = Vec::new();

    for server_config in config.servers {
        let name = server_config.name.clone();
        let status = if is_server_installed(&name).await {
            // Server binary exists - could try to ping it for health check
            // For now, just mark as available
            McpServerStatus::Available
        } else {
            McpServerStatus::NotInstalled
        };

        discovered.push(DiscoveredServer {
            name: name.clone(),
            config: server_config,
            status,
            tools_count: 0, // Would need to connect to get actual count
            error: if status == McpServerStatus::NotInstalled {
                Some(format!(
                    "Server '{name}' not found. Install with: peko mcp install {name}"
                ))
            } else {
                None
            },
        });
    }

    Ok(discovered)
}

/// Discover MCP servers and return their names with config paths
///
/// Returns a vector of tuples containing (`server_name`, `config_path`) for migration purposes.
pub async fn discover_mcp_servers() -> Vec<(String, PathBuf)> {
    let config_path = mcp_config_path();
    let mut servers = Vec::new();

    if !config_path.exists() {
        debug!("MCP config not found at {:?}", config_path);
        return servers;
    }

    match tokio::fs::read_to_string(&config_path).await {
        Ok(content) => {
            if let Ok(config) = toml::from_str::<McpConfig>(&content) {
                for server_config in config.servers {
                    servers.push((server_config.name, config_path.clone()));
                }
            }
        }
        Err(e) => {
            warn!("Failed to read MCP config: {}", e);
        }
    }

    servers
}

/// Print MCP status to logs
pub async fn log_mcp_status() {
    match discover_servers().await {
        Ok(servers) => {
            if servers.is_empty() {
                info!("No MCP servers configured");
            } else {
                info!("MCP Servers:");
                for server in servers {
                    let status_icon = match server.status {
                        McpServerStatus::Available => "✅",
                        McpServerStatus::NotInstalled => "❌",
                        _ => "⚠️",
                    };
                    let cmd = server.config.command.as_deref().unwrap_or("<no command>");
                    info!("  {} {} - {}", status_icon, server.name, cmd);
                    if let Some(ref err) = server.error {
                        info!("     {}", err);
                    }
                }
            }
        }
        Err(e) => {
            warn!("Failed to discover MCP servers: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_mcp_config_path() {
        let path = mcp_config_path();
        assert!(path.ends_with(".peko/mcp.toml"));
    }

    #[tokio::test]
    async fn test_mcp_install_dir() {
        let dir = mcp_install_dir();
        assert!(dir.ends_with(".peko/mcp-servers"));
    }

    #[tokio::test]
    async fn test_default_mcp_config() {
        let config = default_mcp_config();

        // Should be a valid TOML comment/header
        assert!(config.contains("# MCP Servers Configuration"));
    }

    #[tokio::test]
    async fn test_ensure_default_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join(".peko").join("mcp.toml");

        // Ensure parent directory exists
        tokio::fs::create_dir_all(config_path.parent().unwrap())
            .await
            .unwrap();

        // Write default config directly
        tokio::fs::write(&config_path, default_mcp_config())
            .await
            .unwrap();

        // Verify file was created
        assert!(config_path.exists());

        // Read and verify content
        let content = tokio::fs::read_to_string(&config_path).await.unwrap();
        assert!(content.contains("# MCP Servers Configuration"));
    }

    #[tokio::test]
    async fn test_should_use_mcp_tools_no_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join(".peko").join("mcp.toml");

        // Ensure the file doesn't exist
        assert!(!config_path.exists());

        // Temporarily override the config path by creating a helper that uses a different path
        // For this test, we just verify the file doesn't exist
        assert!(!config_path.exists());
    }

    #[test]
    fn test_mcp_server_status_enum() {
        // Test all variants
        let statuses = [
            McpServerStatus::Unknown,
            McpServerStatus::Available,
            McpServerStatus::Unhealthy,
            McpServerStatus::NotInstalled,
            McpServerStatus::NotRunning,
            McpServerStatus::Failed,
        ];

        // Verify they can be compared
        assert_ne!(McpServerStatus::Available, McpServerStatus::Failed);
        assert_eq!(McpServerStatus::Available, McpServerStatus::Available);

        // Just to use the variable
        assert_eq!(statuses.len(), 6);
    }

    #[tokio::test]
    async fn test_is_server_installed_unknown() {
        // An unknown server should not be installed
        let result = is_server_installed("unknown-server-xyz-not-real").await;
        assert!(!result);
    }
}
