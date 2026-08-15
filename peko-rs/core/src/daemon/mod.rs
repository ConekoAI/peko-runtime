//! peko daemon - Long-running process for cron job execution
//!
//! The daemon provides:
//! - Cron job polling and execution (via `cron_engine`)
//! - Principal message handling
//! - Delivery/announcement of results
//! - Session maintenance (prune, cap, rotate)
//! - Graceful shutdown

pub(crate) mod background_runtime;
pub(crate) mod config_drift;
pub(crate) mod cron_engine;
pub(crate) mod cron_ops;
pub(crate) mod cron_runtime;
pub(crate) mod state;

use crate::common::paths::PathResolver;
use crate::daemon::cron_engine::CronEngine;
use anyhow::Result;
use chrono::Utc;
use peko_cron::events::SystemEvent;
use peko_cron::IdleDetector;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

/// How this daemon was launched.
///
/// Used by the Status response so clients can tell who owns the running
/// IPC socket. `Headless` is the default for `peko daemon start` (CLI)
/// and any ad-hoc invocation; `Sidecar` is set by `peko-desktop`'s
/// supervisor so it can detect when a foreign daemon is already
/// holding the socket (and adopt it instead of spawning a competing
/// child). See ADR-043 §adoption.
///
/// `pub` (not `pub(crate)`) because `ResponsePacket::Status::mode`
/// is part of the public IPC wire envelope (`ipc::packet` is `pub`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    #[default]
    Headless,
    Sidecar,
}

impl LaunchMode {
    /// String form used in the `mode` field of `ResponsePacket::Status`
    /// and in CLI diagnostic messages. Kept in lockstep with the serde
    /// representation above.
    pub fn as_str(&self) -> &'static str {
        match self {
            LaunchMode::Headless => "headless",
            LaunchMode::Sidecar => "sidecar",
        }
    }
}

impl std::fmt::Display for LaunchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Daemon configuration.
///
/// `pub` since Phase 11b so the `peko-daemon` binary artifact
/// (`src/bin/peko-daemon.rs`) can construct a `DaemonConfig` from CLI
/// flags and pass it to `Daemon::new`. Field visibility stays `pub` so
/// the binary can populate the struct by-name.
///
/// **Phase A.** The legacy global `cron_db_path` field is removed —
/// cron state now lives per-principal at
/// `{data_dir}/principals/{name}/local/cron/schedule.toml`, derived
/// from the typed path resolver the daemon carries. The cron engine
/// opens a `CronScheduler` per loaded principal on demand.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Polling interval for checking due jobs
    pub poll_interval: Duration,
    /// Config directory for loading agents
    pub config_dir: PathBuf,
    /// Data directory for storage
    pub data_dir: PathBuf,
    /// Session maintenance interval (0 to disable)
    pub maintenance_interval: Duration,
    /// Maximum number of consecutive PekoHub tunnel reconnect attempts
    /// before the tunnel client stops retrying and reports degraded state.
    /// Issue #8: defaults to 50 (~28 minutes with exponential backoff).
    pub max_reconnect_attempts: u32,
    /// How this daemon was launched (CLI vs. sidecar). Reflected in the
    /// `mode` field of `ResponsePacket::Status` so peers (notably the
    /// desktop's SidecarSupervisor) can tell who owns the IPC socket.
    pub launch_mode: LaunchMode,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let config_dir =
            dirs::home_dir().map_or_else(|| PathBuf::from(".peko"), |d| d.join(".peko"));
        let data_dir = dirs::data_dir().map_or_else(|| config_dir.clone(), |d| d.join("peko"));

        Self {
            // Phase A: cron state is per-principal; no top-level
            // `cron_db_path` is set on the config. The cron engine
            // reads each principal's schedule via the typed
            // `PathResolver`.
            poll_interval: Duration::from_secs(15),
            config_dir,
            data_dir,
            maintenance_interval: Duration::from_hours(1), // 1 hour default
            max_reconnect_attempts: crate::tunnel::DEFAULT_MAX_RECONNECT_ATTEMPTS,
            launch_mode: LaunchMode::default(),
        }
    }
}

/// Daemon status
#[derive(Debug, Clone)]
pub(crate) struct DaemonStatus {
    pub running: bool,
    pub jobs_checked: u64,
    pub jobs_executed: u64,
    pub last_check: Option<chrono::DateTime<Utc>>,
}

/// The peko daemon.
///
/// `pub` since Phase 11b so the `peko-daemon` binary artifact
/// (`src/bin/peko-daemon.rs`) can call `Daemon::new(config)?` and
/// `daemon.run().await`. Internal state (cron engine, status mutex,
/// event channels) stays private to `Daemon::run`.
pub struct Daemon {
    config: DaemonConfig,
    status: Arc<Mutex<DaemonStatus>>,
    event_rx: Option<mpsc::Receiver<SystemEvent>>,
    #[allow(dead_code)]
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

        // Phase A: the cron engine no longer opens a single global
        // scheduler — it derives per-principal schedulers from the
        // typed path resolver. The legacy `cron_db_path` field is
        // gone from `DaemonConfig`.
        let cron_path_resolver = crate::common::paths::PathResolver::with_dirs(
            config.config_dir.clone(),
            config.data_dir.clone(),
            dirs::cache_dir().map_or_else(|| config.data_dir.join("cache"), |d| d.join("peko")),
        );
        // ADR-046 trust + audit: extract the audit dir BEFORE moving
        // the path resolver into the cron engine — JSONL audit
        // events (cron.execute, cron.result, cron.write) flow
        // through this observability instance and need a sink to land
        // in.
        let cron_audit_dir = cron_path_resolver.audit_dir();

        let cron_engine = CronEngine::new(
            cron_path_resolver,
            std::sync::Arc::new(peko_cron::IdleDetector::new()),
            std::sync::Arc::new(peko_observability::Observability::with_audit_dir(
                "daemon",
                cron_audit_dir,
            )?),
            None,
            // Placeholder executor for the un-wired constructor — the
            // daemon replaces this in `Daemon::run` with a real one
            // bound to the AppState's `InboxRegistry`.
            std::sync::Arc::new(
                crate::extensions::framework::async_exec::executor::AsyncExecutor::new(
                    crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
                ),
            ),
            std::sync::Weak::new(),
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
    #[allow(dead_code)]
    pub fn new_with_events(config: DaemonConfig) -> Result<(Self, mpsc::Sender<SystemEvent>)> {
        let status = Arc::new(Mutex::new(DaemonStatus {
            running: false,
            jobs_checked: 0,
            jobs_executed: 0,
            last_check: None,
        }));
        let (event_tx, event_rx) = mpsc::channel(1024);

        let cron_path_resolver = crate::common::paths::PathResolver::with_dirs(
            config.config_dir.clone(),
            config.data_dir.clone(),
            dirs::cache_dir().map_or_else(|| config.data_dir.join("cache"), |d| d.join("peko")),
        );
        // ADR-046 trust + audit: extract the audit dir BEFORE moving
        // the path resolver into the cron engine — JSONL audit
        // events (cron.execute, cron.result, cron.write) flow
        // through this observability instance and need a sink to land
        // in.
        let cron_audit_dir = cron_path_resolver.audit_dir();

        let cron_engine = CronEngine::new(
            cron_path_resolver,
            std::sync::Arc::new(peko_cron::IdleDetector::new()),
            std::sync::Arc::new(peko_observability::Observability::with_audit_dir(
                "daemon",
                cron_audit_dir,
            )?),
            None,
            std::sync::Arc::new(
                crate::extensions::framework::async_exec::executor::AsyncExecutor::new(
                    crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
                ),
            ),
            std::sync::Weak::new(),
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
    #[allow(dead_code)]
    pub async fn publish_event(&self, event: SystemEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event).await;
        }
    }

    /// Run the daemon (blocks until shutdown)
    pub async fn run(mut self) -> Result<()> {
        // PEKO_VERSION line MUST be the first thing written to stderr. The
        // peko-desktop sidecar supervisor (ADR-043) parses this single line
        // for version-mismatch detection: it scrapes stderr as bytes stream
        // in, looks for the `PEKO_VERSION=` prefix, and compares against the
        // version baked into the desktop's bundle manifest. Anything emitted
        // before this line shifts the parser's read frame and makes it look
        // like the daemon is silently failing to start.
        //
        // Format: literal `PEKO_VERSION=<semver>\n`. No JSON, no extra
        // fields, no prefix logs. The supervisor's parser is a strict
        // line-prefix scan.
        //
        // Written via `eprintln!` (not `tracing`) so it reaches stderr even
        // when tracing is suppressed (the CLI typically spawns the daemon
        // with stderr=null, which is fine — the supervisor pipes stderr).
        eprintln!("PEKO_VERSION={}", crate::VERSION);

        info!("🚀 peko daemon starting...");
        info!("   Config dir: {}", self.config.config_dir.display());
        info!("   Data dir: {}", self.config.data_dir.display());
        // Phase A: cron state is per-principal; no top-level
        // "Cron DB" log line. Each principal's schedule file lives
        // under `<data_dir>/principals/{name}/local/cron/`.
        info!("   Poll interval: {:?}", self.config.poll_interval);
        info!(
            "   Maintenance interval: {:?}",
            self.config.maintenance_interval
        );

        {
            let mut status = self.status.lock().await;
            status.running = true;
        }

        // Shared idle detector: used by the cron engine for idle-triggered
        // jobs and by the IPC server to record user activity.
        let idle_detector = Arc::new(IdleDetector::new());

        // Create shared AppState for daemon services
        let mut app_state = crate::daemon::state::AppState::new(
            &self.config.data_dir,
            "127.0.0.1", // host placeholder (HTTP API removed)
            0,           // port placeholder (HTTP API removed)
            crate::daemon::state::DaemonConfigSnapshot {
                data_dir: self.config.data_dir.clone(),
                config_dir: self.config.config_dir.clone(),
                log_level: "info".to_string(),
                launch_mode: self.config.launch_mode,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create AppState: {e}"))?;

        app_state.set_idle_detector(idle_detector.clone());

        // ADR-046: principal-config drift detection. Hashes every
        // `principal.toml` and compares against the baseline written
        // on the previous boot. Drifted principals emit a
        // `principal.config_drift` Security audit event so operators
        // see them in `peko audit tail --since 5m`. The check is
        // startup-only in v1 — see the module doc for the `notify`-
        // based follow-up. Failures here are non-fatal: a corrupted
        // baseline logs a warning and the boot proceeds as a
        // first-boot.
        match config_drift::run_drift_check(&app_state.path_resolver, app_state.observability())
            .await
        {
            Ok(n) if n > 0 => {
                warn!("drift: {n} principal(s) drifted since last boot (see JSONL audit log)");
            }
            Ok(_) => {
                debug!("drift: no principal config drift detected");
            }
            Err(e) => {
                warn!("drift: check failed (continuing boot): {e}");
            }
        }

        // PR-3c / PR-5a: spawn a per-(principal, channel)
        // `ChannelSubscriber` for every loaded principal's channels.
        // The audit meter is the only thing the subscriber exercises
        // in production — agents read channels actively via the
        // `ChannelRead` tool (PR-4a), so there is no daemon-side
        // responder to dispatch through. The `NoopChannelResponder`
        // passed in is permanent (PR-5a deleted `EngineChannelResponder`).
        // Spawn-and-forget so a subscriber crash doesn't block daemon
        // boot; the `ChannelSubscriber::spawn` loop handles transient
        // errors internally (logs and continues; only `NotFound` /
        // `NotMember` end the loop, which is the correct signal for
        // "channel deleted out from under us").
        let channel_handles = spawn_channel_subscribers(&app_state).await;
        info!(
            "channel subscribers: spawned {} poll task(s)",
            channel_handles.len()
        );
        // Stash the handles so a future shutdown hook can abort
        // them; today daemon shutdown is a process kill so we don't
        // need to drive them.
        let _channel_handles = channel_handles;

        // Build the cron engine's `AsyncExecutor` with the daemon's
        // shared `InboxRegistry` so completion events and steer
        // messages land in the same inboxes the in-flight `AgenticLoop`
        // drains. The executor resolves tools via the daemon-global
        // `ExtensionCore` (`Arc::downgrade` so the cron engine does not
        // extend the core's lifetime).
        let cron_async_executor = Arc::new(
            crate::extensions::framework::async_exec::executor::AsyncExecutor::new(
                app_state.inbox_registry.clone(),
            ),
        );
        let cron_extension_core = crate::extensions::framework::core::global_core()
            .map(|arc| std::sync::Arc::downgrade(&arc))
            .unwrap_or_else(std::sync::Weak::new);

        // Replace the placeholder cron engine with one wired to the real
        // PrincipalManager, shared idle detector, and cron-owned executor.
        // Phase A: the cron engine takes the typed path resolver
        // directly; no global `CronScheduler` is constructed here —
        // the engine derives per-principal schedulers on demand.
        let cron_path_resolver = app_state.path_resolver.clone();
        self.cron_engine = CronEngine::new(
            cron_path_resolver,
            idle_detector,
            Arc::new(peko_observability::Observability::with_audit_dir(
                "daemon",
                app_state.path_resolver.audit_dir(),
            )?),
            Some(app_state.principal_manager().clone()),
            cron_async_executor,
            cron_extension_core,
        );
        // PR 2: hand the engine to AppState so the `CronRun` IPC
        // handler can dispatch manual triggers through the same
        // coalescing / spawn logic that scheduled fires use.
        app_state.set_cron_engine(Arc::new(self.cron_engine.clone()));

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

        // Auto-start extensions registered with the runtime starter registry (ADR-026).
        // This brings up MCP servers marked `auto_start: true` without requiring a
        // manual `peko ext start`, matching Claude Code's MCP lifecycle UX.
        let starter_ctx = app_state.starter_context();
        let started = app_state
            .runtime_starter_registry()
            .auto_start_all(&starter_ctx)
            .await;
        if !started.is_empty() {
            info!(
                "🚀 Auto-started {} runtime(s): {:?}",
                started.len(),
                started
            );
        } else {
            debug!("No runtimes requested auto-start");
        }

        // Start PekoHub tunnel if credentials exist (ADR-035)
        match app_state
            .start_tunnel(self.config.max_reconnect_attempts)
            .await
        {
            Ok(true) => info!(
                "🌐 PekoHub tunnel started in background (max_reconnect_attempts={})",
                self.config.max_reconnect_attempts
            ),
            Ok(false) => info!("📡 No PekoHub credentials found; tunnel not started"),
            Err(e) => warn!("Failed to start PekoHub tunnel: {}", e),
        }

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

        // Phase 10b: install the cron runtime port so the
        // `Cron{Create,Delete,List}Tool`s (which live in
        // `peko-cron` and cannot import daemon state directly)
        // can dispatch through the F37 capability-gated funnel.
        // F1b (2026-08-07 field test): the adapter dispatches to the
        // in-process `CronOps` — no IPC loopback over the daemon's own
        // socket (a latent receiver bug made every conversational cron
        // tool call hang 60s and then fail while silently adding the
        // job). Idempotent — repeated installs with the same adapter
        // are a no-op.
        {
            let cron_ops = std::sync::Arc::new(crate::daemon::cron_ops::CronOps::new(
                app_state.path_resolver.clone(),
                std::sync::Arc::clone(app_state.principal_manager()),
                std::sync::Arc::clone(&app_state.authority),
            ));
            let adapter = std::sync::Arc::new(crate::daemon::cron_runtime::DaemonCronAdapter::new(
                cron_ops,
            ));
            adapter.install_as_global();
            info!("🕓 Cron runtime port installed (DaemonCronAdapter, in-process)");
        }

        // Install the peer-messenger port so the `send_peer` tool's
        // user branch (and the cron engine's note delivery) can append
        // labeled notes to a peer's conversational session without
        // importing daemon state. Idempotent like the cron port.
        {
            crate::principal::messenger::set_global_messenger(std::sync::Arc::new(
                crate::principal::messenger::PrincipalPeerMessenger::new(std::sync::Arc::clone(
                    app_state.principal_manager(),
                )),
            ));
            info!("📨 Peer messenger port installed (PrincipalPeerMessenger)");
        }

        // Create polling intervals
        let mut poll_tick = interval(self.config.poll_interval);
        let mut maintenance_tick = interval(self.config.maintenance_interval);
        let mut idle_check_tick = interval(Duration::from_mins(1));
        let mut janitor_tick = interval(Duration::from_hours(1));

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
                    match executor.run_janitor(Duration::from_hours(24)).await {
                        Ok((files, registry)) => {
                            if files > 0 || registry > 0 {
                                info!("Async task janitor cleaned {} task files and {} registry entries", files, registry);
                            }
                        }
                        Err(e) => {
                            error!("Error running async task janitor: {}", e);
                        }
                    }

                    // Reconcile `CronRun` rows still marked `"running"`
                    // against the executor's task registry so SpawnTool
                    // fires reach their final status (Phase 4).
                    match self.cron_engine.reconcile_running_runs().await {
                        Ok(0) => {}
                        Ok(n) => info!("Cron janitor reconciled {n} previously-running runs"),
                        Err(e) => error!("Cron janitor reconciliation failed: {e}"),
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

        // Stop PekoHub tunnel
        app_state.stop_tunnel().await;

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

        let scheduler = peko_session::MaintenanceScheduler::new(sessions_root.to_path_buf());
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
            // Phase A: `cron_db_path` is gone; the cron engine
            // derives per-principal paths from `config.data_dir` via
            // the typed resolver.
            poll_interval: Duration::from_secs(1),
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
            maintenance_interval: Duration::from_mins(1),
            max_reconnect_attempts: crate::tunnel::DEFAULT_MAX_RECONNECT_ATTEMPTS,
            launch_mode: LaunchMode::Headless,
        };

        let daemon = Daemon::new(config).unwrap();
        let status = daemon.status.lock().await.clone();
        assert!(!status.running);
    }
}

// Opt-in layered E2E tests against the daemon (formerly
// `tests/tunnel_e2e.rs`). Gated by `--features test-utils` so the daemon
// internals can stay `pub(crate)` rather than being inflated to `pub` just
// to be reachable from a top-level `tests/*.rs` integration harness.
#[cfg(all(test, feature = "test-utils"))]
mod e2e_tests;

// ---------------------------------------------------------------------------
// PR-3c / PR-5a: channel subscriber lifespan
// ---------------------------------------------------------------------------

/// Spawn one [`ChannelSubscriber`] per (loaded principal × channel the
/// principal is a member of). Each subscriber runs the meter against
/// the audit ring buffer via the `AuditChannelMeter` returned by
/// `AppState::channel_meter`. The responder is the no-op impl
/// permanently — agents read channels actively via the `ChannelRead`
/// tool (PR-4a) rather than via a daemon-side responder. PR-5a deleted
/// `EngineChannelResponder`. The lifespan task itself is small, lives
/// once per daemon boot, and never blocks startup: a failure inside
/// one subscriber's tick logs and continues; only `NotFound` /
/// `NotMember` (channel vanished) ends the loop, which is the correct
/// signal.
///
/// `app_state` must already be constructed (drift check ran, principals
/// are loaded). The function returns the `JoinHandle`s so a future
/// shutdown hook can abort them; today's shutdown is a process kill,
/// so callers are free to drop them.
async fn spawn_channel_subscribers(
    app_state: &crate::daemon::state::AppState,
) -> Vec<tokio::task::JoinHandle<()>> {
    use peko_channel::responder::NoopChannelResponder;
    use peko_channel::{ChannelSubscriber, SubscriptionConfig};

    // Access `channel_port` / `channel_meter` through the
    // `ChannelHost` trait — they're trait methods, not inherent
    // methods. The handler module owns the trait definition
    // (consumer-defined pattern, per F6/F7).
    let host: &dyn crate::ipc::handlers::channel::ChannelHost = app_state;
    let port: Arc<dyn peko_channel::ChannelPort> = host.channel_port();
    let meter = host.channel_meter();
    let runtime_dir = app_state.path_resolver.runtime_dir();
    // `principal_manager()` returns `&Arc<PrincipalManager>` — always
    // present on a fully-constructed `AppState`. The `principal_manager`
    // field is private, so we go through the public accessor.
    let principal_manager = app_state.principal_manager().clone();

    let principals = principal_manager.list_all().await;
    let mut handles = Vec::new();
    let cfg = SubscriptionConfig::default();

    for principal in principals {
        let principal_id = principal.id.clone();
        let channels = match port.list_for_principal(&principal_id).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    principal = %principal_id,
                    ?e,
                    "channel subscribers: list_for_principal failed; skipping"
                );
                continue;
            }
        };

        for channel in channels {
            let channel_dir = runtime_dir.join("channels").join(channel.as_str());
            // Resume from the persisted per-member cursors so a daemon
            // restart doesn't re-observe the channel's entire event
            // history. A missing file loads as an empty map (first-ever
            // boot — the tick then starts from offset 0 by design); a
            // corrupt/unreadable file falls back to fresh cursors.
            let cursors = match peko_channel::ChannelCursors::load(&channel_dir).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        channel = %channel,
                        dir = %channel_dir.display(),
                        ?e,
                        "channel subscribers: cursor load failed; starting from fresh cursors"
                    );
                    peko_channel::ChannelCursors::new()
                }
            };
            let sub = ChannelSubscriber::new(
                channel.clone(),
                principal_id.clone(),
                channel_dir,
                port.clone(),
                Arc::new(NoopChannelResponder),
                meter.clone(),
                cursors,
                cfg.clone(),
            );
            handles.push(sub.spawn());
        }
    }
    handles
}
