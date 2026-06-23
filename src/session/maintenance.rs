//! Scheduled maintenance for session management
//!
//! Runs periodic maintenance tasks:
//! - Session pruning (remove old sessions)
//! - Session capping (limit count per agent)
//!
//! Only active in daemon mode.

use crate::common::paths::PathResolver;
use crate::session::index::MaintenanceConfig;
use crate::session::metadata_controller::MetadataController;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, error, info};

/// Maintenance scheduler for daemon mode
pub struct MaintenanceScheduler {
    config: MaintenanceConfig,
    agents_dir: PathBuf,
    interval: Duration,
}

impl MaintenanceScheduler {
    /// Create a new maintenance scheduler
    pub fn new(agents_dir: PathBuf) -> Self {
        Self {
            config: MaintenanceConfig::default(),
            agents_dir,
            interval: Duration::from_hours(1), // Run every hour by default
        }
    }

    /// Set custom maintenance config
    pub fn with_config(mut self, config: MaintenanceConfig) -> Self {
        self.config = config;
        self
    }

    /// Set custom interval
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Start the maintenance scheduler (runs indefinitely)
    pub async fn start(self) {
        info!(
            "Starting session maintenance scheduler (interval: {:?}, prune_after: {:?}, max_sessions: {})",
            self.interval, self.config.prune_after, self.config.max_sessions
        );

        let mut ticker = interval(self.interval);

        loop {
            ticker.tick().await;

            match self.run_maintenance().await {
                Ok(report) => {
                    if report.pruned > 0 || report.total > 0 {
                        info!(
                            "Maintenance complete: pruned={}, total={}",
                            report.pruned, report.total
                        );
                    } else {
                        debug!("Maintenance completed with no actions needed");
                    }
                }
                Err(e) => {
                    error!("Maintenance failed: {}", e);
                }
            }
        }
    }

    /// Run maintenance once (for manual triggering).
    ///
    /// Walks the `agents_dir` looking for `sessions/` subdirectories and
    /// runs `MetadataController::maintenance` in each one.
    pub async fn run_maintenance(
        &self,
    ) -> anyhow::Result<crate::session::index::MaintenanceReport> {
        use crate::session::index::MaintenanceReport;

        let mut total_report = MaintenanceReport::default();

        let mut entries = tokio::fs::read_dir(&self.agents_dir).await?;

        while let Ok(Some(entry)) = entries.next_entry().await {
            let metadata = entry.metadata().await?;
            if !metadata.is_dir() {
                continue;
            }

            let agent_name = entry.file_name().to_string_lossy().to_string();
            let sessions_dir = entry.path().join("sessions");

            if !sessions_dir.exists() {
                continue;
            }

            let mut controller = MetadataController::new(&sessions_dir);

            match controller.maintenance(&self.config).await {
                Ok(report) => {
                    total_report.pruned += report.pruned;
                    total_report.total += report.total;
                }
                Err(e) => {
                    error!("Maintenance failed for {}: {}", agent_name, e);
                }
            }
        }

        Ok(total_report)
    }

    /// Run maintenance once at startup
    pub async fn run_at_startup(&self) -> anyhow::Result<crate::session::index::MaintenanceReport> {
        info!("Running initial maintenance at startup");
        self.run_maintenance().await
    }
}

/// Run maintenance for a specific agent
pub async fn maintain_agent(
    agent_name: &str,
    team: Option<&str>,
    config: &MaintenanceConfig,
) -> anyhow::Result<crate::session::index::MaintenanceReport> {
    let resolver = PathResolver::new();
    let sessions_dir = resolver.agent_sessions_dir(agent_name, team);

    let mut controller = MetadataController::new(&sessions_dir);
    controller.maintenance(config).await
}

/// Spawn maintenance scheduler as a background task
pub fn spawn_scheduler(agents_dir: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let scheduler = MaintenanceScheduler::new(agents_dir);
        scheduler.start().await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_scheduler_creation() {
        let temp = TempDir::new().unwrap();
        let scheduler = MaintenanceScheduler::new(temp.path().to_path_buf());

        assert_eq!(scheduler.interval, Duration::from_hours(1));
    }

    #[tokio::test]
    async fn test_run_maintenance_empty() {
        let temp = TempDir::new().unwrap();
        let scheduler = MaintenanceScheduler::new(temp.path().to_path_buf());

        let report = scheduler.run_maintenance().await.unwrap();
        assert_eq!(report.pruned, 0);
        assert_eq!(report.total, 0);
    }
}
