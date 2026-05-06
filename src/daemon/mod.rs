//! Pekobot Daemon - Long-running process for cron job execution
//!
//! The daemon provides:
//! - Cron job polling and execution (via `cron_engine`)
//! - Main session job handling (enqueue system events)
//! - Isolated job handling (spawn agent turns)
//! - Delivery/announcement of results
//! - Session maintenance (prune, cap, rotate)
//! - Graceful shutdown

pub mod background_runtime;
pub mod cron_engine;
pub mod state;

use crate::agent::stateless_service::StatelessAgentService;
use crate::common::paths::PathResolver;
use crate::cron::events::SystemEvent;
use crate::daemon::cron_engine::CronEngine;
use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tracing::{debug, error, info};

/// Daemon configuration
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Database path for cron jobs
    pub cron_db_path: PathBuf,
    /// Polling interval for checking due jobs
    pub poll_interval: Duration,
    /// Config directory for loading agents
    pub config_dir: PathBuf,
    /// Data directory for storage
    pub data_dir: PathBuf,
    /// Whether to enable isolated job execution
    pub enable_isolated_execution: bool,
    /// Session maintenance interval (0 to disable)
    pub maintenance_interval: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let config_dir =
            dirs::home_dir().map_or_else(|| PathBuf::from(".pekobot"), |d| d.join(".pekobot"));
        let data_dir = dirs::data_dir().map_or_else(|| config_dir.clone(), |d| d.join("pekobot"));

        Self {
            cron_db_path: data_dir.join("cron.json"),
            poll_interval: Duration::from_secs(15),
            config_dir,
            data_dir,
            enable_isolated_execution: true,
            maintenance_interval: Duration::from_secs(3600), // 1 hour default
        }
    }
}

/// Daemon status
#[derive(Debug, Clone)]
pub struct DaemonStatus {
    pub running: bool,
    pub jobs_checked: u64,
    pub jobs_executed: u64,
    pub last_check: Option<chrono::DateTime<Utc>>,
}

/// The Pekobot daemon
pub struct Daemon {
    config: DaemonConfig,
    status: Arc<Mutex<DaemonStatus>>,
    event_rx: Option<mpsc::Receiver<SystemEvent>>,
    event_tx: Option<mpsc::Sender<SystemEvent>>,
    cron_engine: CronEngine,
}

impl Daemon {
    /// Create a new daemon
    pub fn new(config: DaemonConfig) -> Result<Self> {
        Self::with_event_receiver(config, None)
    }

    /// Create a new daemon with event receiver for event-triggered jobs
    pub fn with_event_receiver(
        config: DaemonConfig,
        event_rx: Option<mpsc::Receiver<SystemEvent>>,
    ) -> Result<Self> {
        let status = Arc::new(Mutex::new(DaemonStatus {
            running: false,
            jobs_checked: 0,
            jobs_executed: 0,
            last_check: None,
        }));

        let cron_engine = CronEngine::new(
            std::sync::Arc::new(crate::cron::CronScheduler::new(&config.cron_db_path)?),
            std::sync::Arc::new(crate::cron::IdleDetector::new()),
            std::sync::Arc::new(crate::observability::Observability::new("daemon")),
            config.data_dir.clone(),
            config.enable_isolated_execution,
        );

        Ok(Self {
            config,
            status,
            event_rx,
            event_tx: None,
            cron_engine,
        })
    }

    /// Create a new daemon with an internal event channel.
    /// Returns the daemon and the sender half so external code can publish events.
    pub fn new_with_events(config: DaemonConfig) -> Result<(Self, mpsc::Sender<SystemEvent>)> {
        let status = Arc::new(Mutex::new(DaemonStatus {
            running: false,
            jobs_checked: 0,
            jobs_executed: 0,
            last_check: None,
        }));
        let (event_tx, event_rx) = mpsc::channel(1024);

        let cron_engine = CronEngine::new(
            std::sync::Arc::new(crate::cron::CronScheduler::new(&config.cron_db_path)?),
            std::sync::Arc::new(crate::cron::IdleDetector::new()),
            std::sync::Arc::new(crate::observability::Observability::new("daemon")),
            config.data_dir.clone(),
            config.enable_isolated_execution,
        );

        let daemon = Self {
            config,
            status,
            event_rx: Some(event_rx),
            event_tx: Some(event_tx.clone()),
            cron_engine,
        };

        Ok((daemon, event_tx))
    }

    /// Publish a system event to the internal event channel.
    pub async fn publish_event(&self, event: SystemEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event).await;
        }
    }

    /// Set the agent service for real job execution
    pub fn set_agent_service(&mut self, service: Arc<StatelessAgentService>) {
        self.cron_engine.set_agent_service(service);
    }

    /// Run the daemon (blocks until shutdown)
    pub async fn run(mut self) -> Result<()> {
        info!("🚀 Pekobot daemon starting...");
        info!("   Config dir: {}", self.config.config_dir.display());
        info!("   Data dir: {}", self.config.data_dir.display());
        info!("   Cron DB: {}", self.config.cron_db_path.display());
        info!("   Poll interval: {:?}", self.config.poll_interval);
        info!(
            "   Maintenance interval: {:?}",
            self.config.maintenance_interval
        );

        {
            let mut status = self.status.lock().await;
            status.running = true;
        }

        // Create shared AppState for daemon services
        let app_state = crate::daemon::state::AppState::new(
            &self.config.data_dir,
            "127.0.0.1", // host placeholder (HTTP API removed)
            0,           // port placeholder (HTTP API removed)
            crate::daemon::state::DaemonConfigSnapshot {
                data_dir: self.config.data_dir.clone(),
                config_dir: self.config.config_dir.clone(),
                log_level: "info".to_string(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create AppState: {e}"))?;

        // Wire agent service into daemon for real job execution
        self.set_agent_service(app_state.agent_service().clone());

        // Write our own PID file so stop commands can find us even if the parent is gone
        let pid_file = crate::ipc::default_pid_path();
        if let Some(parent) = pid_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pid_file, std::process::id().to_string());
        info!(
            "   PID file: {} (pid={})",
            pid_file.display(),
            std::process::id()
        );

        // Mark daemon as ready (server is listening)
        app_state.set_ready(true).await;
        info!("✅ Daemon ready to accept requests");

        // Start IPC server (replaces HTTP API per ADR-021)
        let ipc_shutdown_rx = app_state.subscribe_shutdown();
        let ipc_server = crate::ipc::IpcServer::new(app_state.clone())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start IPC server: {e}"))?;
        let ipc_handle = tokio::spawn(async move {
            if let Err(e) = ipc_server.run(ipc_shutdown_rx).await {
                error!("IPC server error: {}", e);
            }
        });

        // Create polling intervals
        let mut poll_tick = interval(self.config.poll_interval);
        let mut maintenance_tick = interval(self.config.maintenance_interval);
        let mut idle_check_tick = interval(Duration::from_secs(60));
        let mut janitor_tick = interval(Duration::from_secs(3600));

        // Subscribe to shutdown signals from AppState
        let mut shutdown_rx = app_state.subscribe_shutdown();

        info!("✅ Daemon ready. Waiting for cron jobs...");

        // Clone event receiver if present
        let mut event_rx = self.event_rx.take();

        // Build path resolver for session maintenance
        let resolver = PathResolver::with_dirs(
            self.config.config_dir.clone(),
            self.config.data_dir.clone(),
            self.config.data_dir.join("cache"),
        );
        let sessions_root = resolver.sessions_root();

        loop {
            tokio::select! {
                // Periodic cron check (time-based jobs)
                _ = poll_tick.tick() => {
                    if let Err(e) = self.cron_engine.check_and_run().await {
                        let msg = e.to_string();
                        if msg.contains("no such table") {
                            debug!("Cron table not initialized, skipping cron check");
                        } else {
                            error!("Error checking cron jobs: {}", e);
                        }
                    }
                    self.sync_cron_status().await;
                }

                // Periodic idle check (idle-triggered jobs)
                _ = idle_check_tick.tick() => {
                    if let Err(e) = self.cron_engine.check_idle().await {
                        let msg = e.to_string();
                        if msg.contains("no such table") {
                            debug!("Cron table not initialized, skipping idle check");
                        } else {
                            error!("Error checking idle jobs: {}", e);
                        }
                    }
                    self.sync_cron_status().await;
                }

                // Periodic session maintenance
                _ = maintenance_tick.tick() => {
                    if let Err(e) = self.run_session_maintenance(&sessions_root).await {
                        error!("Error running session maintenance: {}", e);
                    }
                }

                // Periodic async task janitor (ADR-020 Phase 6)
                _ = janitor_tick.tick() => {
                    let executor = &app_state.async_task_executor;
                    match executor.run_janitor(Duration::from_secs(24 * 3600)).await {
                        Ok((files, registry)) => {
                            if files > 0 || registry > 0 {
                                info!("Async task janitor cleaned {} task files and {} registry entries", files, registry);
                            }
                        }
                        Err(e) => {
                            error!("Error running async task janitor: {}", e);
                        }
                    }
                }

                // Handle system events (event-triggered jobs)
                Some(event) = async {
                    match &mut event_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Err(e) = self.cron_engine.handle_event(event).await {
                        error!("Error handling system event: {}", e);
                    }
                    self.sync_cron_status().await;
                }

                // Handle shutdown signal from API
                _ = shutdown_rx.recv() => {
                    info!("🛑 Daemon shutdown requested...");
                    break;
                }

                // Handle Ctrl+C / SIGTERM
                _ = tokio::signal::ctrl_c() => {
                    info!("🛑 Daemon received Ctrl+C...");
                    break;
                }
            }
        }

        // Mark daemon as not ready
        app_state.set_ready(false).await;

        // Wait for IPC server to finish
        let _ = ipc_handle.await;

        // Clean up PID file
        let pid_file = crate::ipc::default_pid_path();
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(pid_file.with_extension("lock"));

        {
            let mut status = self.status.lock().await;
            status.running = false;
        }

        info!("👋 Daemon shutdown complete");
        Ok(())
    }

    /// Copy cron-engine counters into the daemon's top-level status.
    async fn sync_cron_status(&self) {
        let cron = self.cron_engine.status().await;
        let mut status = self.status.lock().await;
        status.jobs_checked = cron.jobs_checked;
        status.jobs_executed = cron.jobs_executed;
        status.last_check = cron.last_check;
    }

    /// Run session maintenance on all agents by delegating to `session::MaintenanceScheduler`.
    async fn run_session_maintenance(&self, sessions_root: &std::path::Path) -> Result<()> {
        if !sessions_root.exists() {
            debug!("Sessions root does not exist, skipping maintenance");
            return Ok(());
        }

        let scheduler = crate::session::MaintenanceScheduler::new(sessions_root.to_path_buf());
        let report = scheduler.run_maintenance().await?;

        if report.pruned > 0 || report.total > 0 {
            info!(
                "🔧 Session maintenance complete: pruned={}, total={}",
                report.pruned, report.total
            );
        } else {
            debug!("🔧 Session maintenance complete: no action needed");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_daemon_creation() {
        let tmp = TempDir::new().unwrap();
        let config = DaemonConfig {
            cron_db_path: tmp.path().join("cron.json"),
            poll_interval: Duration::from_secs(1),
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
            enable_isolated_execution: false,
            maintenance_interval: Duration::from_secs(60),
        };

        let daemon = Daemon::new(config).unwrap();
        let status = daemon.status.lock().await.clone();
        assert!(!status.running);
    }
}
