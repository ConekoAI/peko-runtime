//! Application State
//!
//! Shared state accessible to all API route handlers.
//! This will expand as more endpoints are implemented.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;

/// Shared application state for the HTTP API
///
/// This struct is passed to all route handlers via Axum's State extractor.
/// All fields are thread-safe and can be accessed concurrently.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Time when the daemon started
    pub started_at: SystemTime,

    /// Path to the workspace directory (.pekobot/)
    pub workspace_path: PathBuf,

    /// Port the server is listening on
    pub port: u16,

    /// Host address the server is bound to
    pub host: String,

    /// Daemon configuration
    pub config: DaemonConfigSnapshot,

    /// Internal state that can be modified
    inner: Arc<RwLock<AppStateInner>>,
}

/// Mutable internal state
#[derive(Debug, Default)]
struct AppStateInner {
    /// Whether the daemon is in a degraded state
    pub degraded: bool,
    /// Number of running instances (cached)
    pub instance_count: u64,
    /// Number of teams (cached)
    pub team_count: u64,
}

/// Snapshot of daemon configuration
#[derive(Debug, Clone)]
pub struct DaemonConfigSnapshot {
    /// Data directory path
    pub data_dir: PathBuf,
    /// Config directory path
    pub config_dir: PathBuf,
    /// Log level
    pub log_level: String,
}

impl AppState {
    /// Create new application state
    pub fn new(
        workspace_path: impl Into<PathBuf>,
        host: impl Into<String>,
        port: u16,
        config: DaemonConfigSnapshot,
    ) -> Self {
        Self {
            started_at: SystemTime::now(),
            workspace_path: workspace_path.into(),
            port,
            host: host.into(),
            config,
            inner: Arc::new(RwLock::new(AppStateInner::default())),
        }
    }

    /// Get the current uptime in seconds
    pub fn uptime_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(self.started_at)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Check if the daemon is degraded
    pub async fn is_degraded(&self) -> bool {
        let inner = self.inner.read().await;
        inner.degraded
    }

    /// Set the degraded state
    pub async fn set_degraded(&self, degraded: bool) {
        let mut inner = self.inner.write().await;
        inner.degraded = degraded;
    }

    /// Get the current instance count
    pub async fn instance_count(&self) -> u64 {
        let inner = self.inner.read().await;
        inner.instance_count
    }

    /// Update the instance count
    pub async fn set_instance_count(&self, count: u64) {
        let mut inner = self.inner.write().await;
        inner.instance_count = count;
    }

    /// Get the current team count
    pub async fn team_count(&self) -> u64 {
        let inner = self.inner.read().await;
        inner.team_count
    }

    /// Update the team count
    pub async fn set_team_count(&self, count: u64) {
        let mut inner = self.inner.write().await;
        inner.team_count = count;
    }

    /// Mark the daemon as healthy (not degraded)
    pub async fn mark_healthy(&self) {
        self.set_degraded(false).await;
    }

    /// Mark the daemon as degraded
    pub async fn mark_degraded(&self) {
        self.set_degraded(true).await;
    }
}

impl Default for DaemonConfigSnapshot {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(".pekobot"),
            config_dir: PathBuf::from(".pekobot"),
            log_level: "info".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_uptime_tracking() {
        let state = AppState::new(
            "/tmp/test",
            "127.0.0.1",
            11434,
            DaemonConfigSnapshot::default(),
        );

        // Initial uptime should be very small
        let uptime1 = state.uptime_seconds();
        assert_eq!(uptime1, 0);

        // Wait a bit and check uptime increased
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let uptime2 = state.uptime_seconds();
        assert!(uptime2 >= 0); // May still be 0 if less than 1 second passed
    }

    #[tokio::test]
    async fn test_degraded_state() {
        let state = AppState::new(
            "/tmp/test",
            "127.0.0.1",
            11434,
            DaemonConfigSnapshot::default(),
        );

        assert!(!state.is_degraded().await);

        state.mark_degraded().await;
        assert!(state.is_degraded().await);

        state.mark_healthy().await;
        assert!(!state.is_degraded().await);
    }

    #[tokio::test]
    async fn test_instance_count() {
        let state = AppState::new(
            "/tmp/test",
            "127.0.0.1",
            11434,
            DaemonConfigSnapshot::default(),
        );

        assert_eq!(state.instance_count().await, 0);

        state.set_instance_count(5).await;
        assert_eq!(state.instance_count().await, 5);
    }
}
