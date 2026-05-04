//! BackgroundRuntimeManager — orchestrates all long-running background runtimes

use super::adapter::BackgroundRuntimeAdapter;
use super::supervisor::{
    is_runtime_alive, spawn_runtime_external, spawn_runtime_process, spawn_runtime_task,
    ManagedRuntime, RuntimeState,
};
use crate::common::process::{RestartPolicy, RuntimeSpawnConfig};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Summary of a managed runtime for listing/status
#[derive(Debug, Clone)]
pub struct RuntimeSummary {
    pub id: String,
    pub state: RuntimeState,
    pub restart_count: u32,
    pub last_error: Option<String>,
}

/// Manages all background runtimes in the daemon
#[derive(Clone)]
pub struct BackgroundRuntimeManager {
    runtimes: Arc<RwLock<HashMap<String, ManagedRuntime>>>,
    health_tasks: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
}

impl BackgroundRuntimeManager {
    /// Create a new background runtime manager
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtimes: Arc::new(RwLock::new(HashMap::new())),
            health_tasks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start a new managed runtime
    pub async fn start(
        &self,
        id: String,
        spawn_config: RuntimeSpawnConfig,
        adapter: Arc<dyn BackgroundRuntimeAdapter>,
        restart_policy: RestartPolicy,
    ) -> anyhow::Result<()> {
        let mut runtimes = self.runtimes.write().await;

        if runtimes.contains_key(&id) {
            anyhow::bail!("Runtime '{}' already exists", id);
        }

        info!("Starting background runtime: {}", id);

        // Create the runtime based on spawn config
        let mut runtime = match &spawn_config {
            RuntimeSpawnConfig::Process(process_config) => {
                spawn_runtime_process(&id, process_config, adapter, restart_policy, spawn_config.clone()).await?
            }
            RuntimeSpawnConfig::Task { .. } => {
                // For tasks, the caller must spawn the task and pass the handle.
                // This variant is used when the task is created externally.
                anyhow::bail!(
                    "Use start_task() for in-process task runtimes"
                );
            }
            RuntimeSpawnConfig::External { endpoint, .. } => {
                spawn_runtime_external(&id, endpoint.clone(), adapter, restart_policy, spawn_config.clone())
            }
        };

        // Initialize via adapter — we must do this before inserting into the map
        // because initialize takes &mut ManagedRuntime
        let adapter = runtime.adapter.clone();
        if let Err(e) = adapter.initialize(&mut runtime).await {
            runtime.state = RuntimeState::Crashed;
            runtime.last_error = Some(e.to_string());
            runtimes.insert(id.clone(), runtime);
            anyhow::bail!("Failed to initialize runtime '{}': {}", id, e);
        }

        runtime.state = RuntimeState::Running;
        runtimes.insert(id.clone(), runtime);

        // Start health check loop
        self.start_health_check(&id).await;

        info!("Background runtime '{}' started successfully", id);
        Ok(())
    }

    /// Start an in-process task runtime
    pub async fn start_task(
        &self,
        id: String,
        task: tokio::task::JoinHandle<()>,
        abort_tx: tokio::sync::oneshot::Sender<()>,
        adapter: Arc<dyn BackgroundRuntimeAdapter>,
        restart_policy: RestartPolicy,
    ) -> anyhow::Result<()> {
        let mut runtimes = self.runtimes.write().await;

        if runtimes.contains_key(&id) {
            anyhow::bail!("Runtime '{}' already exists", id);
        }

        info!("Starting background task runtime: {}", id);

        let spawn_config = RuntimeSpawnConfig::Task { name: id.clone() };
        let mut runtime = spawn_runtime_task(&id, task, abort_tx, adapter.clone(), restart_policy, spawn_config);

        // Initialize via adapter
        if let Err(e) = adapter.initialize(&mut runtime).await {
            runtime.state = RuntimeState::Crashed;
            runtime.last_error = Some(e.to_string());
            runtimes.insert(id.clone(), runtime);
            anyhow::bail!("Failed to initialize task runtime '{}': {}", id, e);
        }

        runtime.state = RuntimeState::Running;
        runtimes.insert(id.clone(), runtime);

        // Start health check loop
        self.start_health_check(&id).await;

        info!("Background task runtime '{}' started successfully", id);
        Ok(())
    }

    /// Stop a managed runtime
    pub async fn stop(&self, id: &str) -> anyhow::Result<()> {
        let mut runtimes = self.runtimes.write().await;

        let runtime = runtimes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Runtime '{}' not found", id))?;

        info!("Stopping background runtime: {}", id);

        // Stop health check task
        {
            let mut health_tasks = self.health_tasks.write().await;
            if let Some(handle) = health_tasks.remove(id) {
                handle.abort();
            }
        }

        // Stop the runtime
        if let Err(e) = super::supervisor::stop_runtime(runtime).await {
            warn!("Error stopping runtime '{}': {}", id, e);
        }

        runtimes.remove(id);
        info!("Background runtime '{}' stopped", id);
        Ok(())
    }

    /// Restart a runtime
    pub async fn restart(&self, id: &str) -> anyhow::Result<()> {
        info!("Restarting background runtime: {}", id);

        // Collect config needed for restart
        let (spawn_config, adapter, restart_policy) = {
            let runtimes = self.runtimes.read().await;
            let runtime = runtimes
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("Runtime '{}' not found", id))?;
            (
                runtime.spawn_config.clone(),
                runtime.adapter.clone_box(),
                runtime.restart_policy.clone(),
            )
        };

        // Stop the existing runtime
        self.stop(id).await?;

        // Start a new runtime with the same configuration
        self.start(id.to_string(), spawn_config, adapter, restart_policy)
            .await?;

        info!("Background runtime '{}' restarted successfully", id);
        Ok(())
    }

    /// Get runtime state
    pub async fn get_state(&self, id: &str) -> Option<RuntimeState> {
        let runtimes = self.runtimes.read().await;
        runtimes.get(id).map(|r| r.state)
    }

    /// List all managed runtimes
    pub async fn list(&self) -> Vec<RuntimeSummary> {
        let runtimes = self.runtimes.read().await;
        runtimes
            .values()
            .map(|r| RuntimeSummary {
                id: r.id.clone(),
                state: r.state,
                restart_count: r.restart_count,
                last_error: r.last_error.clone(),
            })
            .collect()
    }

    /// Graceful shutdown of all runtimes
    pub async fn shutdown_all(&self) -> anyhow::Result<()> {
        info!("Shutting down all background runtimes");

        let ids: Vec<String> = {
            let runtimes = self.runtimes.read().await;
            runtimes.keys().cloned().collect()
        };

        for id in ids {
            if let Err(e) = self.stop(&id).await {
                warn!("Error stopping runtime '{}': {}", id, e);
            }
        }

        info!("All background runtimes shut down");
        Ok(())
    }

    /// Start health check loop for a runtime
    async fn start_health_check(&self, id: &str) {
        let id_owned = id.to_string();
        let runtimes = self.runtimes.clone();
        let health_tasks = self.health_tasks.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));

            loop {
                interval.tick().await;

                // ── Phase 1: Check health (hold lock briefly) ─────────────────
                let crash_action = {
                    let mut runtimes_guard = runtimes.write().await;
                    let Some(runtime) = runtimes_guard.get_mut(&id_owned) else {
                        break;
                    };

                    let healthy = runtime.adapter.health_check(runtime).await;

                    if healthy {
                        if runtime.state == RuntimeState::Unhealthy {
                            info!("Runtime '{}' is now healthy", id_owned);
                            runtime.state = RuntimeState::Healthy;
                        } else if runtime.state == RuntimeState::Running {
                            runtime.state = RuntimeState::Healthy;
                        }
                        None
                    } else {
                        if runtime.state == RuntimeState::Healthy
                            || runtime.state == RuntimeState::Running
                        {
                            warn!("Runtime '{}' is now unhealthy", id_owned);
                            runtime.state = RuntimeState::Unhealthy;
                        }

                        // Check if the runtime has crashed
                        if !is_runtime_alive(runtime) {
                            warn!("Runtime '{}' has crashed", id_owned);
                            runtime.state = RuntimeState::Crashed;

                            let adapter = runtime.adapter.clone();
                            let action = adapter.on_crash(runtime).await;
                            Some(action)
                        } else {
                            None
                        }
                    }
                };

                // ── Phase 2: Act on crash (lock released, can do I/O) ─────────
                if let Some(action) = crash_action {
                    match action {
                        super::adapter::CrashAction::Restart => {
                            // Re-acquire lock to check restart count and collect config
                            let (should_restart, spawn_config, adapter, restart_policy) = {
                                let runtimes_guard = runtimes.read().await;
                                let Some(runtime) = runtimes_guard.get(&id_owned) else {
                                    break;
                                };

                                if runtime.restart_count < runtime.restart_policy.max_restarts {
                                    (
                                        true,
                                        runtime.spawn_config.clone(),
                                        runtime.adapter.clone_box(),
                                        runtime.restart_policy.clone(),
                                    )
                                } else {
                                    error!(
                                        "Runtime '{}' exceeded max restarts ({}), giving up",
                                        id_owned, runtime.restart_policy.max_restarts
                                    );
                                    (false, runtime.spawn_config.clone(), runtime.adapter.clone_box(), runtime.restart_policy.clone())
                                }
                            };

                            if should_restart {
                                // Apply backoff before restart
                                let backoff = restart_policy.backoff_for(
                                    // Read current count (we haven't incremented yet in the guard above)
                                    {
                                        let runtimes_guard = runtimes.read().await;
                                        let rt = runtimes_guard.get(&id_owned).unwrap();
                                        rt.restart_count
                                    }
                                );
                                if backoff > std::time::Duration::from_millis(0) {
                                    info!(
                                        "Waiting {:?} before restarting runtime '{}'",
                                        backoff, id_owned
                                    );
                                    tokio::time::sleep(backoff).await;
                                }

                                // Re-spawn the runtime
                                match Self::respawn_runtime(
                                    &id_owned,
                                    spawn_config,
                                    adapter,
                                    restart_policy,
                                    &runtimes,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        info!("Runtime '{}' restarted successfully", id_owned);
                                        // Continue the health check loop
                                        continue;
                                    }
                                    Err(e) => {
                                        error!("Failed to restart runtime '{}': {}", id_owned, e);
                                        let mut runtimes_guard = runtimes.write().await;
                                        if let Some(rt) = runtimes_guard.get_mut(&id_owned) {
                                            rt.state = RuntimeState::Stopped;
                                            rt.last_error = Some(e.to_string());
                                        }
                                        break;
                                    }
                                }
                            } else {
                                let mut runtimes_guard = runtimes.write().await;
                                if let Some(rt) = runtimes_guard.get_mut(&id_owned) {
                                    rt.state = RuntimeState::Stopped;
                                }
                                break;
                            }
                        }
                        super::adapter::CrashAction::Stop => {
                            let mut runtimes_guard = runtimes.write().await;
                            if let Some(rt) = runtimes_guard.get_mut(&id_owned) {
                                rt.state = RuntimeState::Stopped;
                            }
                            break;
                        }
                        super::adapter::CrashAction::Escalate => {
                            error!(
                                "Runtime '{}' crashed — operator intervention required",
                                id_owned
                            );
                            let mut runtimes_guard = runtimes.write().await;
                            if let Some(rt) = runtimes_guard.get_mut(&id_owned) {
                                rt.state = RuntimeState::Stopped;
                            }
                            break;
                        }
                    }
                }
            }

            // Remove from health tasks when loop exits
            let mut health_tasks = health_tasks.write().await;
            health_tasks.remove(&id_owned);
        });

        let mut health_tasks = self.health_tasks.write().await;
        health_tasks.insert(id.to_string(), handle);
    }

    /// Re-spawn a crashed runtime with the same configuration.
    ///
    /// This is called from the health check task when `CrashAction::Restart` is
    /// returned by the adapter. It replaces the old `ManagedRuntime` entry in
    /// the map with a freshly spawned one.
    async fn respawn_runtime(
        id: &str,
        spawn_config: RuntimeSpawnConfig,
        adapter: Arc<dyn BackgroundRuntimeAdapter>,
        restart_policy: RestartPolicy,
        runtimes: &Arc<RwLock<HashMap<String, ManagedRuntime>>>,
    ) -> anyhow::Result<()> {
        info!("Re-spawning runtime '{}'", id);

        // Spawn new process/task/external connection
        let mut runtime = match &spawn_config {
            RuntimeSpawnConfig::Process(process_config) => {
                spawn_runtime_process(id, process_config, adapter, restart_policy, spawn_config.clone())
                    .await?
            }
            RuntimeSpawnConfig::Task { .. } => {
                anyhow::bail!("Task runtime restart not supported from health check");
            }
            RuntimeSpawnConfig::External { endpoint, .. } => {
                spawn_runtime_external(id, endpoint.clone(), adapter, restart_policy, spawn_config.clone())
            }
        };

        // Initialize via adapter
        let adapter = runtime.adapter.clone();
        if let Err(e) = adapter.initialize(&mut runtime).await {
            runtime.state = RuntimeState::Crashed;
            runtime.last_error = Some(e.to_string());
            let mut runtimes_guard = runtimes.write().await;
            // Increment restart count since we attempted a restart
            if let Some(old_rt) = runtimes_guard.get(id) {
                runtime.restart_count = old_rt.restart_count + 1;
            }
            runtimes_guard.insert(id.to_string(), runtime);
            anyhow::bail!("Failed to initialize restarted runtime '{}': {}", id, e);
        }

        runtime.state = RuntimeState::Running;

        // Preserve restart count from previous incarnation
        let mut runtimes_guard = runtimes.write().await;
        if let Some(old_rt) = runtimes_guard.get(id) {
            runtime.restart_count = old_rt.restart_count + 1;
        }
        runtimes_guard.insert(id.to_string(), runtime);

        info!("Runtime '{}' re-spawned and initialized", id);
        Ok(())
    }
}

impl Default for BackgroundRuntimeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for BackgroundRuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundRuntimeManager")
            .field("runtimes", &"<HashMap<String, ManagedRuntime>>")
            .field("health_tasks", &"<HashMap<String, JoinHandle>>")
            .finish()
    }
}
