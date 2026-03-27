//! Scheduled maintenance for session management
//!
//! Runs periodic maintenance tasks:
//! - Session pruning (remove old sessions)
//! - Session capping (limit count per agent)
//! - Index rotation (prevent large files)
//!
//! Only active in daemon mode.

use crate::common::paths::PathResolver;
use crate::session::index::{MaintenanceConfig, MaintenanceMode};
use crate::session::metadata_controller::MetadataController;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

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
            interval: Duration::from_secs(3600), // Run every hour by default
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
            "Starting session maintenance scheduler (interval: {:?}, mode: {:?})",
            self.interval, self.config.mode
        );

        let mut ticker = interval(self.interval);

        loop {
            ticker.tick().await;

            if self.config.mode == MaintenanceMode::Off {
                debug!("Maintenance mode is Off, skipping");
                continue;
            }

            match self.run_maintenance().await {
                Ok(report) => {
                    if !report.is_empty() {
                        info!(
                            "Maintenance complete: pruned={}, capped={}, rotated={}, reclaimed={} bytes",
                            report.pruned, report.capped, if report.rotated { 1 } else { 0 }, report.bytes_reclaimed
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

    /// Run maintenance once (for manual triggering)
    pub async fn run_maintenance(&self
) -> anyhow::Result<crate::session::index::MaintenanceReport> {
        use crate::session::index::MaintenanceReport;

        let mut total_report = MaintenanceReport {
            pruned: 0,
            capped: 0,
            rotated: false,
            bytes_reclaimed: 0,
        };

        // List all agents
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

            // Use MetadataController instead of direct SessionIndex access
            let mut controller = MetadataController::new(&sessions_dir);

            // Run maintenance via controller
            match controller.maintenance(&self.config).await {
                Ok(report) => {
                    total_report.pruned += report.pruned;
                    total_report.capped += report.capped;
                    if report.rotated {
                        total_report.rotated = true;
                    }
                    total_report.bytes_reclaimed += report.bytes_reclaimed;
                }
                Err(e) => {
                    error!("Maintenance failed for {}: {}", agent_name, e);
                }
            }
        }

        Ok(total_report)
    }

    /// Run maintenance once at startup
    pub async fn run_at_startup(&self
) -> anyhow::Result<crate::session::index::MaintenanceReport> {
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

    // Use MetadataController instead of direct SessionIndex access
    let mut controller = MetadataController::new(&sessions_dir);

    // Run maintenance via controller
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

        assert_eq!(scheduler.interval, Duration::from_secs(3600));
    }

    #[tokio::test]
    async fn test_run_maintenance_empty() {
        let temp = TempDir::new().unwrap();
        let scheduler = MaintenanceScheduler::new(temp.path().to_path_buf());

        let report = scheduler.run_maintenance().await.unwrap();
        assert!(report.is_empty());
    }
}
