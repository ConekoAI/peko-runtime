//! Daemon mode for background service operation
//!
//! Runs Pekobot as a background service with component supervision
//! and automatic restart on failure.

use anyhow::Result;
use std::future::Future;
use std::path::PathBuf;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tracing::{error, info, warn};

/// Daemon configuration
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path to write state file
    pub state_file: PathBuf,
    /// Initial backoff in seconds for component restarts
    pub initial_backoff_secs: u64,
    /// Maximum backoff in seconds
    pub max_backoff_secs: u64,
    /// How often to write state file
    pub state_flush_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            state_file: PathBuf::from("daemon_state.json"),
            initial_backoff_secs: 5,
            max_backoff_secs: 300,
            state_flush_secs: 10,
        }
    }
}

/// Daemon state
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DaemonState {
    pub started_at: String,
    pub components: Vec<ComponentState>,
}

/// Component state
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComponentState {
    pub name: String,
    pub status: String,
    pub restart_count: u32,
    pub last_error: Option<String>,
    pub last_updated: String,
}

/// Run the daemon with the given components
pub async fn run_daemon<F, Fut>(config: DaemonConfig, run_main: F) -> Result<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    info!("Starting Pekobot daemon");

    // Spawn state writer
    let state_handle = spawn_state_writer(config.clone());

    // Run main component
    let main_handle = tokio::spawn(async move {
        if let Err(e) = run_main().await {
            error!("Main component failed: {}", e);
        }
    });

    info!("Daemon started. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received");
        }
        _ = main_handle => {
            warn!("Main component exited");
        }
    }

    // Graceful shutdown
    info!("Shutting down daemon...");
    state_handle.abort();

    let _ = state_handle.await;

    info!("Daemon stopped");
    Ok(())
}

/// Spawn a component supervisor that restarts on failure
pub fn spawn_component_supervisor<F, Fut>(
    name: &'static str,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    mut run_component: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = initial_backoff_secs.max(1);
        let max_backoff = max_backoff_secs.max(backoff);
        let mut restart_count: u32 = 0;

        loop {
            info!("Starting component '{}'", name);

            match run_component().await {
                Ok(()) => {
                    warn!("Component '{}' exited unexpectedly", name);
                }
                Err(e) => {
                    error!("Component '{}' failed: {}", name, e);
                }
            }

            restart_count += 1;
            warn!(
                "Restarting component '{}' in {}s (restart #{})",
                name, backoff, restart_count
            );

            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = backoff.saturating_mul(2).min(max_backoff);
        }
    })
}

/// Spawn state file writer
fn spawn_state_writer(config: DaemonConfig) -> JoinHandle<()> {
    tokio::spawn(async move {
        let path = &config.state_file;
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        let started_at = chrono::Utc::now().to_rfc3339();
        let mut interval = tokio::time::interval(Duration::from_secs(config.state_flush_secs));

        loop {
            interval.tick().await;

            let state = DaemonState {
                started_at: started_at.clone(),
                components: vec![], // Components would register themselves
            };

            let data = serde_json::to_vec_pretty(&state).unwrap_or_else(|_| b"{}".to_vec());
            let _ = tokio::fs::write(path, data).await;
        }
    })
}

/// Check if daemon is already running
#[must_use] 
pub fn is_daemon_running(state_file: &PathBuf) -> bool {
    if let Ok(content) = std::fs::read_to_string(state_file) {
        if let Ok(state) = serde_json::from_str::<DaemonState>(&content) {
            // If started within last 5 minutes, consider it running
            if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&state.started_at) {
                let elapsed = chrono::Utc::now().signed_duration_since(started);
                return elapsed.num_seconds() < 300;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert_eq!(config.initial_backoff_secs, 5);
        assert_eq!(config.max_backoff_secs, 300);
        assert_eq!(config.state_flush_secs, 10);
    }

    #[test]
    fn test_is_daemon_running_no_file() {
        let temp = TempDir::new().unwrap();
        let state_file = temp.path().join("nonexistent.json");
        assert!(!is_daemon_running(&state_file));
    }

    #[tokio::test]
    async fn test_spawn_component_supervisor() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter_clone = counter.clone();

        let handle = spawn_component_supervisor("test", 1, 5, move || {
            let count = counter_clone.clone();
            async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("test error")
            }
        });

        // Let it restart a couple times
        tokio::time::sleep(Duration::from_millis(2500)).await;
        handle.abort();

        let count = counter.load(std::sync::atomic::Ordering::SeqCst);
        assert!(count >= 2, "Component should have restarted at least twice");
    }
}
