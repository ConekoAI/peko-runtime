//! HTTP API Server
//!
//! The main server implementation using Axum. Handles startup, routing,
//! middleware application, and graceful shutdown.

use axum::{extract::connect_info::IntoMakeServiceWithConnectInfo, middleware::from_fn, Router};
use std::net::SocketAddr;
use tracing::{error, info, warn};

use crate::api::middleware::{
    logging::logging_middleware, request_id::request_id_middleware, version::version_middleware,
};
use crate::api::routes::create_router;
use crate::api::state::{AppState, DaemonConfigSnapshot};
use crate::api::DEFAULT_HOST;
use crate::api::DEFAULT_PORT;

/// HTTP server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Host address to bind to
    pub host: String,
    /// Port to listen on
    pub port: u16,
    /// Path to workspace directory
    pub workspace_path: std::path::PathBuf,
    /// Daemon configuration snapshot
    pub daemon_config: DaemonConfigSnapshot,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            workspace_path: std::path::PathBuf::from(".pekobot"),
            daemon_config: DaemonConfigSnapshot::default(),
        }
    }
}

/// HTTP API Server
///
/// The main server struct that manages the Axum application and handles
/// the lifecycle of the HTTP API.
pub struct ApiServer {
    config: ServerConfig,
    state: AppState,
}

impl ApiServer {
    /// Create a new API server with the given configuration
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration including host, port, and paths
    ///
    /// # Warnings
    ///
    /// If the host is not a loopback address, a security warning will be logged.
    pub async fn new(config: ServerConfig) -> anyhow::Result<Self> {
        Self::log_security_warning(&config);

        let state = AppState::new(
            &config.workspace_path,
            &config.host,
            config.port,
            config.daemon_config.clone(),
        )
        .await?;

        Ok(Self { config, state })
    }

    /// Create an API server with an existing AppState
    ///
    /// Used by the daemon when it needs to share state with background tasks.
    pub fn with_state(config: ServerConfig, state: AppState) -> Self {
        Self::log_security_warning(&config);
        Self { config, state }
    }

    fn log_security_warning(config: &ServerConfig) {
        if !is_loopback(&config.host) {
            warn!(
                "\n\
╔══════════════════════════════════════════════════════════════════════╗\n\
║  SECURITY WARNING: Binding to non-loopback address '{}'             ║
║                                                                      ║
║  The Pekobot daemon is accessible from the network. Ensure proper   ║
║  firewall rules are in place and access is restricted.              ║
╚══════════════════════════════════════════════════════════════════════╝\n",
                config.host
            );
        }
    }

    /// Create the Axum router with all routes and middleware
    fn create_router(&self) -> IntoMakeServiceWithConnectInfo<Router, SocketAddr> {
        // Build base router with routes
        let router = create_router();

        // Apply middleware layers
        // Note: Layers are applied in reverse order for requests
        let router = router
            // Request ID middleware (first to process request, last to process response)
            .layer(from_fn(request_id_middleware))
            // Version header middleware
            .layer(from_fn(version_middleware))
            // Logging middleware
            .layer(from_fn(logging_middleware));

        // Add state and make service with connection info
        router
            .with_state(self.state.clone())
            .into_make_service_with_connect_info::<SocketAddr>()
    }

    /// Run the server until shutdown signal received
    ///
    /// # Arguments
    ///
    /// * `shutdown_rx` - Channel receiver for shutdown signal
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on graceful shutdown, or an error if the server failed.
    pub async fn run(self, shutdown_rx: tokio::sync::oneshot::Receiver<()>) -> anyhow::Result<()> {
        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid bind address: {e}"))?;

        let app = self.create_router();

        // Create TCP listener
        // On Unix, we use SO_REUSEADDR to handle TIME_WAIT on restart
        // On Windows, this isn't supported the same way, so we just bind normally
        let listener = {
            #[cfg(unix)]
            {
                use std::os::fd::{FromRawFd, IntoRawFd};

                // Create a std TcpListener with SO_REUSEADDR
                let std_listener = std::net::TcpListener::bind(addr)
                    .map_err(|e| anyhow::anyhow!("Failed to bind to {addr}: {e}"))?;

                // SO_REUSEADDR allows binding to a socket in TIME_WAIT state
                std_listener
                    .set_reuseaddr(true)
                    .map_err(|e| anyhow::anyhow!("Failed to set SO_REUSEADDR: {e}"))?;

                // Convert to tokio TcpListener
                tokio::net::TcpListener::from_std(std_listener)
                    .map_err(|e| anyhow::anyhow!("Failed to create tokio listener: {e}"))?
            }

            #[cfg(windows)]
            {
                // On Windows, just use tokio's bind directly
                // Note: Windows doesn't support SO_REUSEADDR the same way Unix does
                tokio::net::TcpListener::bind(addr)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to bind to {addr}: {e}"))?
            }
        };

        let actual_addr = listener.local_addr()?;
        info!(
            "HTTP API server listening on http://{} (bound to {})",
            actual_addr, addr
        );

        // Run server with graceful shutdown
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
                info!("HTTP API server shutdown signal received");
            })
            .await
            .map_err(|e| anyhow::anyhow!("Server error: {e}"))?;

        info!("HTTP API server stopped");
        Ok(())
    }

    /// Get the server address
    #[must_use] 
    pub fn address(&self) -> String {
        format!("{}:{}", self.config.host, self.config.port)
    }

    /// Get the application state
    #[must_use] 
    pub fn state(&self) -> &AppState {
        &self.state
    }
}

/// Check if a host address is a loopback address
fn is_loopback(host: &str) -> bool {
    match host {
        "127.0.0.1" | "localhost" | "::1" | "ip6-localhost" | "ip6-loopback" => true,
        _ => {
            // Try to parse as IP address
            if let Ok(addr) = host.parse::<std::net::IpAddr>() {
                addr.is_loopback()
            } else {
                // Hostname - can't determine, assume non-loopback
                false
            }
        }
    }
}

/// Create and spawn the API server
///
/// Returns a handle that can be used to trigger shutdown.
pub async fn spawn_server(config: ServerConfig) -> anyhow::Result<ServerHandle> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let server = ApiServer::new(config).await?;
    let address = server.address();

    let handle = tokio::spawn(async move {
        if let Err(e) = server.run(shutdown_rx).await {
            error!("API server error: {}", e);
        }
    });

    Ok(ServerHandle {
        address,
        shutdown_tx,
        handle,
    })
}

/// Handle for a running server
pub struct ServerHandle {
    address: String,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    /// Trigger graceful shutdown
    pub fn shutdown(self) -> anyhow::Result<()> {
        let _ = self.shutdown_tx.send(());
        Ok(())
    }

    /// Wait for the server to finish
    pub async fn wait(self) -> anyhow::Result<()> {
        self.handle
            .await
            .map_err(|e| anyhow::anyhow!("Server task failed: {e}"))
    }

    /// Get the server address
    #[must_use] 
    pub fn address(&self) -> &str {
        &self.address
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_loopback() {
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("localhost"));
        assert!(is_loopback("::1"));
        assert!(!is_loopback("0.0.0.0"));
        assert!(!is_loopback("192.168.1.1"));
        assert!(!is_loopback("10.0.0.1"));
    }

    #[test]
    fn test_server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 11435);
    }

    #[tokio::test]
    async fn test_api_server_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            workspace_path: temp_dir.path().to_path_buf(),
            daemon_config: DaemonConfigSnapshot::default(),
        };
        let server = ApiServer::new(config).await.unwrap();
        assert_eq!(server.address(), "127.0.0.1:8080");
    }
}
