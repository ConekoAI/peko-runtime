//! Pekobot Daemon - Long-running process for cron job execution
//!
//! The daemon provides:
//! - Cron job polling and execution
//! - Main session job handling (enqueue system events)
//! - Isolated job handling (spawn agent turns)
//! - Delivery/announcement of results
//! - Session maintenance (prune, cap, rotate)
//! - Graceful shutdown

use crate::cron::{CronJob, CronRun, CronScheduler, DeliveryMode, ExecutionTarget, IdleDetector};
use crate::orchestration::events::SystemEvent;
use crate::session::index::{MaintenanceConfig, MaintenanceMode, SessionIndex};
use crate::types::agent::AgentConfig;
use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

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
            cron_db_path: data_dir.join("cron.db"),
            poll_interval: Duration::from_secs(15),
            config_dir,
            data_dir,
            enable_isolated_execution: true,
            maintenance_interval: Duration::from_secs(3600), // 1 hour default
        }
    }
}

/// Commands sent to the daemon
#[derive(Debug)]
pub enum DaemonCommand {
    /// Shutdown the daemon gracefully
    Shutdown,
    /// Trigger immediate cron check
    CheckCron,
    /// Get daemon status
    GetStatus,
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
    scheduler: Arc<CronScheduler>,
    command_rx: mpsc::Receiver<DaemonCommand>,
    status: Arc<Mutex<DaemonStatus>>,
    /// Idle detector for idle-triggered jobs
    idle_detector: Arc<IdleDetector>,
    /// Event receiver for event-triggered jobs
    event_rx: Option<mpsc::Receiver<SystemEvent>>,
}

impl Daemon {
    /// Create a new daemon
    pub fn new(config: DaemonConfig, command_rx: mpsc::Receiver<DaemonCommand>) -> Result<Self> {
        Self::with_event_receiver(config, command_rx, None)
    }

    /// Create a new daemon with event receiver for event-triggered jobs
    pub fn with_event_receiver(
        config: DaemonConfig,
        command_rx: mpsc::Receiver<DaemonCommand>,
        event_rx: Option<mpsc::Receiver<SystemEvent>>,
    ) -> Result<Self> {
        let scheduler = Arc::new(CronScheduler::new(&config.cron_db_path)?);

        let status = Arc::new(Mutex::new(DaemonStatus {
            running: false,
            jobs_checked: 0,
            jobs_executed: 0,
            last_check: None,
        }));

        let idle_detector = Arc::new(IdleDetector::new());

        Ok(Self {
            config,
            scheduler,
            command_rx,
            status,
            idle_detector,
            event_rx,
        })
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

        // Create polling intervals
        let mut poll_tick = interval(self.config.poll_interval);
        let mut maintenance_tick = interval(self.config.maintenance_interval);
        let mut idle_check_tick = interval(Duration::from_secs(60)); // Check idle jobs every minute

        info!("✅ Daemon ready. Waiting for cron jobs...");

        // Clone event receiver if present
        let mut event_rx = self.event_rx.take();

        loop {
            tokio::select! {
                // Periodic cron check (time-based jobs)
                _ = poll_tick.tick() => {
                    if let Err(e) = self.check_and_run_jobs().await {
                        error!("Error checking cron jobs: {}", e);
                    }
                }

                // Periodic idle check (idle-triggered jobs)
                _ = idle_check_tick.tick() => {
                    if let Err(e) = self.check_idle_jobs().await {
                        error!("Error checking idle jobs: {}", e);
                    }
                }

                // Periodic session maintenance
                _ = maintenance_tick.tick() => {
                    if let Err(e) = self.run_session_maintenance().await {
                        error!("Error running session maintenance: {}", e);
                    }
                }

                // Handle system events (event-triggered jobs)
                Some(event) = async {
                    match &mut event_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Err(e) = self.handle_system_event(event).await {
                        error!("Error handling system event: {}", e);
                    }
                }

                // Handle commands
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        DaemonCommand::Shutdown => {
                            info!("🛑 Daemon shutdown requested...");
                            break;
                        }
                        DaemonCommand::CheckCron => {
                            info!("🔍 Manual cron check triggered");
                            if let Err(e) = self.check_and_run_jobs().await {
                                error!("Error in manual cron check: {}", e);
                            }
                        }
                        DaemonCommand::GetStatus => {
                            let status = self.status.lock().await.clone();
                            info!("📊 Daemon status: {:?}", status);
                        }
                    }
                }
            }
        }

        {
            let mut status = self.status.lock().await;
            status.running = false;
        }

        info!("👋 Daemon shutdown complete");
        Ok(())
    }

    /// Check for due jobs and execute them
    async fn check_and_run_jobs(&self) -> Result<()> {
        let now = Utc::now();

        // Update status
        {
            let mut status = self.status.lock().await;
            status.jobs_checked += 1;
            status.last_check = Some(now);
        }

        // Get due jobs
        let due_jobs = self.scheduler.due_jobs(now)?;

        if !due_jobs.is_empty() {
            info!("⏰ Found {} job(s) due for execution", due_jobs.len());

            for job in due_jobs {
                if let Err(e) = self.execute_job(job).await {
                    error!("Failed to execute job: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Check for idle-triggered jobs and execute if conditions are met
    async fn check_idle_jobs(&self) -> Result<()> {
        use crate::cron::ScheduleKind;

        // Get all idle-triggered jobs
        let idle_jobs = self.scheduler.idle_jobs(false)?;

        if idle_jobs.is_empty() {
            return Ok(());
        }

        debug!("Checking {} idle-triggered jobs", idle_jobs.len());

        for job in idle_jobs {
            if let ScheduleKind::Idle { minutes, agent_id } = &job.schedule {
                let should_execute = if let Some(agent) = agent_id {
                    // Check if specific agent is idle
                    self.idle_detector.is_idle(agent, *minutes).await
                } else {
                    // Check if any agent is idle (global)
                    self.idle_detector.is_global_idle(*minutes).await
                };

                if should_execute {
                    info!(
                        "⏸️  Agent idle for {} minutes, executing job '{}'",
                        minutes, job.name
                    );
                    if let Err(e) = self.execute_job(job).await {
                        error!("Failed to execute idle job: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle system event and trigger matching event-triggered jobs
    async fn handle_system_event(&self, event: SystemEvent) -> Result<()> {
        use crate::cron::ScheduleKind;

        let event_type = event.event_type().to_string();
        debug!("Handling system event: {}", event_type);

        // Get event-triggered jobs
        let event_jobs = self.scheduler.event_jobs(false)?;

        for job in event_jobs {
            if let ScheduleKind::Event {
                event_type: job_event_type,
                filter,
                once,
            } = &job.schedule
            {
                // Check if event type matches
                if job_event_type != &event_type {
                    continue;
                }

                // Check if filter matches (if present)
                if let Some(filter) = filter {
                    if !Self::event_matches_filter(&event, filter) {
                        continue;
                    }
                }

                // Execute the job
                info!(
                    "📡 Event '{}' matches job '{}'",
                    event_type, job.name
                );
                if let Err(e) = self.execute_job(job.clone()).await {
                    error!("Failed to execute event-triggered job: {}", e);
                    continue;
                }

                // Disable if once flag is set
                if *once {
                    if let Err(e) = self.scheduler.set_job_enabled(&job.id, false) {
                        warn!("Failed to disable one-time job {}: {}", job.id, e);
                    } else {
                        info!("🔄 Disabled one-time event job: {}", job.name);
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if an event matches a filter
    fn event_matches_filter(
        event: &SystemEvent,
        filter: &serde_json::Value,
    ) -> bool {
        // Convert event to JSON for filtering
        let event_json = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(_) => return false,
        };

        // Check if filter is a subset of event JSON
        Self::json_subset(&event_json, filter)
    }

    /// Check if target JSON contains all fields from filter with matching values
    fn json_subset(target: &serde_json::Value, filter: &serde_json::Value) -> bool {
        match (target, filter) {
            (serde_json::Value::Object(target_obj), serde_json::Value::Object(filter_obj)) => {
                for (key, filter_val) in filter_obj {
                    match target_obj.get(key) {
                        Some(target_val) => {
                            if !Self::json_subset(target_val, filter_val) {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
                true
            }
            (a, b) => a == b,
        }
    }

    /// Execute a single cron job
    async fn execute_job(&self, job: CronJob) -> Result<()> {
        info!("🔄 Executing job '{}' ({})", job.name, job.id);

        let run_id = Uuid::new_v4().to_string();
        let started_at = Utc::now();

        // Record run start
        let run = CronRun {
            id: run_id.clone(),
            job_id: job.id.clone(),
            started_at,
            finished_at: None,
            status: "running".to_string(),
            output: None,
            error: None,
        };
        self.scheduler.record_run(&run)?;

        // Execute based on target
        let result = match job.target {
            ExecutionTarget::Main => self.execute_main_job(&job).await,
            ExecutionTarget::Isolated => {
                if self.config.enable_isolated_execution {
                    self.execute_isolated_job(&job).await
                } else {
                    warn!("Isolated execution disabled, skipping job {}", job.id);
                    Ok(("skipped".to_string(), None))
                }
            }
        };

        // Record result
        let (status, output, error) = match result {
            Ok((s, o)) => (s, o, None),
            Err(e) => ("failed".to_string(), None, Some(e.to_string())),
        };

        let finished_at = Utc::now();
        let run = CronRun {
            id: run_id,
            job_id: job.id.clone(),
            started_at,
            finished_at: Some(finished_at),
            status: status.clone(),
            output,
            error,
        };
        self.scheduler.record_run(&run)?;

        // Calculate next run time
        let next_run = self
            .scheduler
            .calculate_next_run(&job.schedule, finished_at)?;
        self.scheduler
            .update_job_after_run(&job.id, &status, next_run)?;

        // Handle delivery if needed
        if let DeliveryMode::Announce { .. } = job.delivery {
            self.handle_delivery(&job, &status).await?;
        }

        // Delete one-shot jobs after successful execution
        if job.delete_after_run && status == "success" {
            info!(
                "🗑️  Deleting one-shot job '{}' after successful run",
                job.name
            );
            self.scheduler.delete_job(&job.id)?;
        }

        // Update status
        {
            let mut status = self.status.lock().await;
            status.jobs_executed += 1;
        }

        info!("✅ Job '{}' completed with status: {}", job.name, status);
        Ok(())
    }

    /// Execute a main session job (enqueue system event)
    async fn execute_main_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        info!("📨 Main session job: '{}'", job.message);

        // For main session jobs, we would enqueue a system event
        // to be processed on the next agent interaction
        // For now, we log and mark as success

        // TODO: Implement system event queue for main session jobs
        // This would integrate with the agent's event loop

        let output = format!(
            "[cron:{}] System event enqueued:\n{}",
            job.name, job.message
        );

        info!("   Event enqueued for main session processing");

        Ok(("success".to_string(), Some(output)))
    }

    /// Execute an isolated job (spawn dedicated agent turn)
    async fn execute_isolated_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
        info!("🔧 Isolated job: '{}'", job.message);

        // Load agent config if specified
        let _agent_config = if let Some(ref agent_id) = job.agent_id {
            self.load_agent_config(agent_id).await.ok()
        } else {
            None
        };

        // For isolated execution, we would spawn a new agent instance
        // and run it with the job's message as the prompt

        // TODO: Implement isolated agent spawning
        // This requires:
        // 1. Loading agent config
        // 2. Creating temporary agent instance
        // 3. Running with job message
        // 4. Capturing output
        // 5. Cleanup

        let output = format!(
            "[cron:{} {}] Isolated execution:\n{}",
            job.id, job.name, job.message
        );

        info!("   Isolated execution completed");

        Ok(("success".to_string(), Some(output)))
    }

    /// Handle delivery of job results
    async fn handle_delivery(&self, job: &CronJob, status: &str) -> Result<()> {
        match &job.delivery {
            DeliveryMode::Announce {
                channel,
                to,
                best_effort,
            } => {
                info!("📢 Announcing job '{}' result: {}", job.name, status);

                // TODO: Implement actual delivery to channels
                // This would integrate with the messaging system

                if *best_effort {
                    // Don't fail the job if delivery fails
                    if let Err(e) = self
                        .send_announcement(job, status, channel.as_deref(), to.as_deref())
                        .await
                    {
                        warn!("Failed to send announcement (best_effort=true): {}", e);
                    }
                } else {
                    self.send_announcement(job, status, channel.as_deref(), to.as_deref())
                        .await?;
                }
            }
            DeliveryMode::None => {
                // No delivery needed
            }
        }

        Ok(())
    }

    /// Send announcement to configured channel
    async fn send_announcement(
        &self,
        job: &CronJob,
        status: &str,
        _channel: Option<&str>,
        _to: Option<&str>,
    ) -> Result<()> {
        // Placeholder for actual delivery implementation
        // This would integrate with Discord, Slack, etc.

        let message = format!(
            "🤖 Cron Job Completed\n\nName: {}\nStatus: {}\nMessage: {}",
            job.name, status, job.message
        );

        info!("   Announcement would be sent: {}", message);

        // TODO: Implement actual channel delivery
        // - Load channel credentials from config
        // - Send message via appropriate API
        // - Handle retries and errors

        Ok(())
    }

    /// Load agent configuration by ID
    async fn load_agent_config(&self, agent_id: &str) -> Result<AgentConfig> {
        let config_path = self
            .config
            .config_dir
            .join("agents")
            .join(format!("{agent_id}.toml"));

        if !config_path.exists() {
            return Err(anyhow::anyhow!(
                "Agent config not found: {}",
                config_path.display()
            ));
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: AgentConfig = toml::from_str(&content)?;

        Ok(config)
    }

    /// Run session maintenance on all agents
    async fn run_session_maintenance(&self) -> Result<()> {
        info!("🔧 Running session maintenance...");

        let agents_dir = self.config.config_dir.join("agents");
        if !agents_dir.exists() {
            return Ok(());
        }

        let mut total_pruned = 0;
        let mut total_capped = 0;
        let mut total_rotated = 0;
        let mut total_bytes = 0u64;

        let mut entries = tokio::fs::read_dir(&agents_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let metadata = entry.metadata().await?;
            if !metadata.is_dir() {
                continue;
            }

            let agent_name = entry.file_name().to_string_lossy().to_string();
            let sessions_dir = entry.path().join("sessions");

            if !sessions_dir.exists() {
                continue;
            }

            let mut index = SessionIndex::open(&sessions_dir);
            let _ = index.migrate_from_directory(&agent_name).await;

            let config = MaintenanceConfig {
                mode: MaintenanceMode::Auto,
                ..Default::default()
            };

            match index.maintenance(&config).await {
                Ok(report) => {
                    total_pruned += report.pruned;
                    total_capped += report.capped;
                    if report.rotated {
                        total_rotated += 1;
                    }
                    total_bytes += report.bytes_reclaimed;
                }
                Err(e) => {
                    warn!("Session maintenance failed for {}: {}", agent_name, e);
                }
            }
        }

        if total_pruned > 0 || total_capped > 0 || total_rotated > 0 {
            info!(
                "🔧 Session maintenance complete: pruned={}, capped={}, rotated={}, reclaimed={} bytes",
                total_pruned, total_capped, total_rotated, total_bytes
            );
        } else {
            debug!("🔧 Session maintenance complete: no action needed");
        }

        Ok(())
    }

    /// Get current daemon status
    pub async fn get_status(&self) -> DaemonStatus {
        self.status.lock().await.clone()
    }
}

/// Spawn a daemon in the background and return control handle
pub fn spawn_daemon(config: DaemonConfig) -> Result<DaemonHandle> {
    let (command_tx, command_rx) = mpsc::channel(100);

    let daemon = Daemon::new(config, command_rx)?;
    let status = daemon.status.clone();

    // Spawn daemon in background task
    let handle = tokio::spawn(async move {
        if let Err(e) = daemon.run().await {
            error!("Daemon error: {}", e);
        }
    });

    Ok(DaemonHandle {
        command_tx,
        status,
        _handle: handle,
    })
}

/// Handle for controlling a spawned daemon
pub struct DaemonHandle {
    command_tx: mpsc::Sender<DaemonCommand>,
    status: Arc<Mutex<DaemonStatus>>,
    _handle: tokio::task::JoinHandle<()>,
}

impl DaemonHandle {
    /// Send shutdown command
    pub async fn shutdown(&self) -> Result<()> {
        self.command_tx.send(DaemonCommand::Shutdown).await?;
        Ok(())
    }

    /// Trigger immediate cron check
    pub async fn check_cron(&self) -> Result<()> {
        self.command_tx.send(DaemonCommand::CheckCron).await?;
        Ok(())
    }

    /// Get daemon status
    pub async fn get_status(&self) -> DaemonStatus {
        self.status.lock().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_daemon_creation() {
        let tmp = TempDir::new().unwrap();
        let config = DaemonConfig {
            cron_db_path: tmp.path().join("cron.db"),
            poll_interval: Duration::from_secs(1),
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
            enable_isolated_execution: false,
            maintenance_interval: Duration::from_secs(60), // 1 minute for tests
        };

        let (_tx, rx) = mpsc::channel(10);
        let daemon = Daemon::new(config, rx).unwrap();

        let status = daemon.get_status().await;
        assert!(!status.running);
    }
}
