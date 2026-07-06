//! Subagent Executor
//!
//! Async task executor for subagents. Handles:
//! - Spawning subagent sessions
//! - Executing agents in those sessions
//! - Tracking run status via the unified async task registry
//! - Announcing results back to parents
//! - Timeout and cancellation handling
//!
//! All state is stored in the unified `AsyncTaskRegistry` (see Issue 008).
//! This module no longer maintains a separate `SubagentRegistry`.
//!
//! The executor carries a `principal_id` (the spawning principal's DID)
//! rather than an `Arc<ExtensionCore>` — there is one daemon-global
//! [`crate::extensions::framework::ExtensionCore`] (`global_core()`) and
//! principals share it. Per-principal tool instances (sessions/memory/
//! catalog) are registered on that single global core keyed by the
//! principal, so per-subagent visibility is still scoped to the
//! principal's tool bag without each subagent needing its own core.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_announce::{build_subagent_system_prompt, build_subagent_task_message};
use crate::agents::subagent_error::SpawnError;
use crate::agents::subagent_types::{SubagentResult, SubagentRunView, SubagentStatus};
use crate::auth::Subject;
use crate::extensions::framework::async_exec::executor::{
    get_or_create_registry_for_agent, AsyncExecutor, AsyncResultDeliveryMode,
    AsyncResultQueueManager, AsyncTaskStatus, AsyncToolConfig, SharedAsyncResultQueueManager,
    SharedAsyncTaskRegistry, SubagentMetadata, TaskMetadata, WaitResult,
};
use crate::principal::PrincipalId;
use crate::session::context::SessionContext;
use crate::session::manager::SessionManager;
use crate::session::types::SpawnCleanupPolicy;

/// Channel for announcing completed subagent runs
pub type AnnouncementSender = mpsc::Sender<CompletedRun>;
pub type AnnouncementReceiver = mpsc::Receiver<CompletedRun>;

/// A completed subagent run ready for announcement
#[derive(Debug, Clone)]
pub struct CompletedRun {
    /// The run that completed (view projected from unified registry)
    pub run: SubagentRunView,
    /// The parent session key
    pub parent_session_key: String,
    /// The announcement message
    pub announcement: String,
}

/// Configuration for subagent execution
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum execution time in seconds (0 = unlimited)
    pub timeout_seconds: u64,
    /// Cleanup policy for the session
    pub cleanup: SpawnCleanupPolicy,
    /// Optional label for the run
    pub label: Option<String>,
    /// Whether to announce completion to parent
    pub announce_completion: bool,
    /// Maximum spawn depth (0 = unlimited)
    pub max_depth: u32,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 300, // 5 minutes default
            cleanup: SpawnCleanupPolicy::Keep,
            label: None,
            announce_completion: true,
            max_depth: 1, // Default: no nested spawns
        }
    }
}

/// Executor for subagent tasks
///
/// All task state lives in the unified `AsyncTaskRegistry`. This struct
/// orchestrates subagent-specific logic (session creation, depth tracking,
/// result formatting) but delegates all state storage to the framework.
#[derive(Clone)]
pub struct SubagentExecutor {
    /// Unified async executor for background task execution
    unified_executor: AsyncExecutor,
    /// Agent name for the executor
    agent_name: String,
    /// Maximum concurrent runs
    max_concurrent: usize,
    /// Channel for announcing completed runs
    announcement_tx: Option<AnnouncementSender>,
    /// Provider for LLM execution
    provider: Option<Arc<crate::providers::Provider>>,
    /// Agent configuration for creating subagents
    agent_config: Option<AgentConfig>,
    /// Session manager for accessing sessions
    session_manager: Arc<RwLock<SessionManager>>,
    /// Optional principal workspace. When set, spawned subagents are scoped to
    /// this workspace so their own `Agent` tool resolves nested subagents from
    /// `<workspace>/agents/<name>/AGENT.md`. Propagated down the spawn tree so
    /// delegation works at every depth, not just the first level.
    principal_workspace: Option<std::path::PathBuf>,
    /// The spawning principal's runtime id. Carried so per-principal tool
    /// registration on the global core can be looked up without
    /// re-reading the principal context, and so descendant subagents
    /// inherit the same principal scope down the spawn tree.
    principal_id: PrincipalId,
    /// The spawning principal's human-readable name. Carried so
    /// Principal-scoped tools (e.g. cron) inherit the correct target.
    principal_name: Option<String>,
}

impl SubagentExecutor {
    /// Create a new subagent executor
    ///
    /// Uses the global per-agent async task registry so that status queries
    /// and result delivery work across stateless requests.
    ///
    /// `principal_id` is the spawning principal's runtime id. There is no
    /// per-principal `ExtensionCore` — the executor and its subagents look
    /// tools up on the daemon-global
    /// [`crate::extensions::framework::core::global_core`].
    #[must_use]
    pub fn new(
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
        principal_id: PrincipalId,
    ) -> Self {
        let agent_name = agent_name.into();
        let async_registry = get_or_create_registry_for_agent(&agent_name);
        let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let unified_executor = AsyncExecutor::with_registries(async_registry, async_queue_manager);

        Self {
            unified_executor,
            agent_name,
            max_concurrent,
            announcement_tx: None,
            provider: None,
            agent_config: None,
            session_manager,
            principal_workspace: None,
            principal_id,
            principal_name: None,
        }
    }

    /// Get the spawning principal's runtime id.
    #[must_use]
    pub fn principal_id(&self) -> &PrincipalId {
        &self.principal_id
    }

    /// Get the spawning principal's human-readable name, if known.
    #[must_use]
    pub fn principal_name(&self) -> Option<&str> {
        self.principal_name.as_deref()
    }

    /// Set the spawning principal's human-readable name.
    #[must_use]
    pub fn with_principal_name(mut self, name: impl Into<String>) -> Self {
        self.principal_name = Some(name.into());
        self
    }

    /// Create an executor with an explicit registry (for testing and nested spawns)
    #[must_use]
    pub fn with_registry(
        async_registry: SharedAsyncTaskRegistry,
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
        principal_id: PrincipalId,
    ) -> Self {
        let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let unified_executor = AsyncExecutor::with_registries(async_registry, async_queue_manager);

        Self {
            unified_executor,
            agent_name: agent_name.into(),
            max_concurrent,
            announcement_tx: None,
            provider: None,
            agent_config: None,
            session_manager,
            principal_workspace: None,
            principal_id,
            principal_name: None,
        }
    }

    /// Create an executor with full async framework integration
    #[must_use]
    pub fn with_async_framework(
        async_registry: SharedAsyncTaskRegistry,
        async_queue_manager: SharedAsyncResultQueueManager,
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
        principal_id: PrincipalId,
    ) -> Self {
        let unified_executor = AsyncExecutor::with_registries(async_registry, async_queue_manager);

        Self {
            unified_executor,
            agent_name: agent_name.into(),
            max_concurrent,
            announcement_tx: None,
            provider: None,
            agent_config: None,
            session_manager,
            principal_workspace: None,
            principal_id,
            principal_name: None,
        }
    }

    /// Set the provider for LLM execution
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<crate::providers::Provider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Set the agent configuration
    #[must_use]
    pub fn with_agent_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = Some(config);
        self
    }

    /// Scope spawned subagents to a Principal workspace so nested delegation
    /// resolves subagents from `<workspace>/agents/<name>/AGENT.md`.
    #[must_use]
    pub fn with_principal_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        self.principal_workspace = Some(workspace);
        self
    }

    /// Set the announcement channel
    #[must_use]
    pub fn with_announcement_channel(mut self, tx: AnnouncementSender) -> Self {
        self.announcement_tx = Some(tx);
        self
    }

    /// Create announcement channel
    #[must_use]
    pub fn create_announcement_channel() -> (AnnouncementSender, AnnouncementReceiver) {
        mpsc::channel(100)
    }

    /// Get a reference to the async task registry (unified)
    #[must_use]
    pub fn registry(&self) -> &SharedAsyncTaskRegistry {
        self.unified_executor.registry()
    }

    /// Get a reference to the async queue manager
    #[must_use]
    pub fn async_queue_manager(&self) -> &SharedAsyncResultQueueManager {
        self.unified_executor.queue_manager()
    }

    /// Get a reference to the unified executor
    #[must_use]
    pub fn unified_executor(&self) -> &AsyncExecutor {
        &self.unified_executor
    }

    /// Spawn and execute a subagent
    ///
    /// Returns the `run_id` immediately. The execution happens in the background.
    ///
    /// `parent_cancel` is the soft-interrupt `CancellationToken` from
    /// the parent agent's `AgenticLoop` (PR #128). When set, a
    /// `child_token()` is derived so the sub-agent's own
    /// `AgenticLoop` observes a cancel at iteration boundaries —
    /// closing the gap where interrupting a parent left its
    /// sub-agents running. The child token also fires on
    /// `is_cancelled()` inside the closure below so the
    /// `AsyncTaskStatus::Cancelled` write path runs cleanly.
    pub async fn spawn_and_execute(
        &self,
        task: &str,
        _parent_ctx: Option<&SessionContext>,
        isolated: bool,
        parent_session_key: &str,
        config: ExecutionConfig,
        parent_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        // Check depth limits
        let parent_depth = self.get_parent_depth(parent_session_key).await;
        let child_depth = parent_depth + 1;

        if config.max_depth > 0 && child_depth > config.max_depth {
            return Err(anyhow::anyhow!(SpawnError::DepthLimitExceeded {
                current: child_depth,
                max: config.max_depth,
            }));
        }

        // Check concurrent run limits
        let active_count = self.count_active_runs().await;
        if active_count >= self.max_concurrent {
            return Err(anyhow::anyhow!(SpawnError::ConcurrentLimitExceeded {
                current: active_count,
                max: self.max_concurrent,
            }));
        }

        // Generate run ID
        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());

        // Create spawn session
        let peer = Subject::Principal(format!("spawn_{}", uuid::Uuid::new_v4().simple()).into());
        let spawn_resolved = {
            let mut manager = self.session_manager.write().await;
            manager
                .spawn_session(
                    &self.agent_name,
                    &peer,
                    task,
                    isolated,
                    parent_session_key,
                    Some(config.timeout_seconds),
                )
                .await
                .context("Failed to create spawn session")?
        };

        let child_session_key = spawn_resolved.context.full_session_key.clone();

        // Build the metadata extension that carries subagent-specific data
        let metadata = TaskMetadata::Subagent(SubagentMetadata {
            child_session_key: child_session_key.clone(),
            cleanup: config.cleanup,
            depth: child_depth,
            announce_completion: config.announce_completion,
            subagent_result: None,
        });

        // Execute using unified async executor — this is the ONLY registration point
        let async_config = AsyncToolConfig {
            delivery_mode: AsyncResultDeliveryMode::QueueWhenBusy,
            delivery_target: None,
            timeout_secs: Some(config.timeout_seconds),
            timeout_millis: None,
            cleanup_after_delivery: config.cleanup == SpawnCleanupPolicy::Delete,
            label: config.label.clone(),
            wake_on_completion: true,
            principal_root_session_key: None,
        };

        // Clone values for the execution closure
        let registry_for_task = self.registry().clone();
        let registry_for_completion = self.registry().clone();
        let child_session_key_clone = child_session_key.clone();
        let parent_session_key_clone = parent_session_key.to_string();
        let task_clone = task.to_string();
        let label_clone = config.label.clone();
        let run_id_clone = run_id.clone();
        let timeout = config.timeout_seconds;
        let agent_name = self.agent_name.clone();
        let provider_clone = self.provider.clone();
        let agent_config_clone = self.agent_config.clone();
        let principal_workspace_clone = self.principal_workspace.clone();
        let session_manager_clone = self.session_manager.clone();
        let session_manager_for_cleanup = self.session_manager.clone();
        let principal_id_clone = self.principal_id.clone();
        let cleanup_policy_clone = config.cleanup;
        // Derive a child token inside the closure so the sub-agent
        // observes the parent's cancel via `child_cancel` without
        // extending the parent's lifetime past the closure's
        // `'static` bound. Without `child_token()` the child would
        // share a token with the parent, which is fine for cancel
        // propagation but means a child cancel would also cancel the
        // parent — wrong direction. Derivation fixes both directions.
        let child_cancel_for_closure = parent_cancel.as_ref().map(|t| t.child_token());

        self.unified_executor
            .execute_with_metadata(
                run_id.clone(),
                "Agent",
                serde_json::json!({
                    "task": task,
                    "isolated": isolated,
                    "label": &config.label,
                    "child_session_key": &child_session_key,
                }),
                parent_session_key.to_string(),
                async_config,
                metadata,
                move || async move {
                    info!(
                        "Starting subagent execution: run_id={} session={}",
                        run_id_clone, child_session_key_clone
                    );

                    // Build system prompt and task message
                    let system_prompt = build_subagent_system_prompt(
                        &parent_session_key_clone,
                        &child_session_key_clone,
                        &task_clone,
                        label_clone.as_deref(),
                        child_depth,
                        config.max_depth,
                    );

                    let task_message =
                        build_subagent_task_message(&task_clone, child_depth, config.max_depth);

                    // Execute with timeout. The cancel token is
                    // observed via two paths: (1) the child's
                    // `AgenticLoop` checks `is_cancelled()` at
                    // iteration boundaries and exits cleanly via
                    // `Lifecycle::Interrupted`; (2) the closure
                    // here checks `is_cancelled()` after the
                    // `exec_fut` resolves so the registry is
                    // updated with `AsyncTaskStatus::Cancelled`
                    // rather than `Failed` when the parent was
                    // interrupted.
                    let exec_fut = execute_subagent_task(
                        &agent_name,
                        &child_session_key_clone,
                        &system_prompt,
                        &task_message,
                        provider_clone,
                        agent_config_clone,
                        session_manager_clone,
                        registry_for_task,
                        principal_id_clone,
                        principal_workspace_clone,
                        child_cancel_for_closure.clone(),
                    );
                    let result = if timeout > 0 {
                        match tokio::time::timeout(
                            tokio::time::Duration::from_secs(timeout),
                            exec_fut,
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(_) => {
                                warn!(
                                    "Subagent timed out: run_id={} timeout={}s",
                                    run_id_clone, timeout
                                );
                                Err(anyhow::anyhow!(SpawnError::Timeout { seconds: timeout }))
                            }
                        }
                    } else {
                        exec_fut.await
                    };

                    // Process result. If the parent was cancelled
                    // mid-flight, the child's loop returns
                    // `AgenticResult { interrupted: true }` —
                    // surface that as `Cancelled` instead of
                    // `Failed` so the parent's `peko async-list`
                    // shows the right state.
                    let cancelled = child_cancel_for_closure
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                    let (status, output, error): (AsyncTaskStatus, Option<String>, Option<String>) =
                        if cancelled {
                            info!("Subagent cancelled by parent: run_id={}", run_id_clone);
                            (AsyncTaskStatus::Cancelled, None, None)
                        } else {
                            match result {
                                Ok(output) => {
                                    info!(
                                        "Subagent completed successfully: run_id={}",
                                        run_id_clone
                                    );
                                    (
                                        AsyncTaskStatus::Completed {
                                            result: crate::tools::ToolResult::success(
                                                serde_json::json!({"output": &output}),
                                            ),
                                        },
                                        Some(output),
                                        None,
                                    )
                                }
                                Err(e) => {
                                    error!("Subagent failed: run_id={} error={}", run_id_clone, e);
                                    (
                                        AsyncTaskStatus::Failed {
                                            error: e.to_string(),
                                        },
                                        None,
                                        Some(e.to_string()),
                                    )
                                }
                            }
                        };

                    // Update the unified registry with the subagent result.
                    // This is the ONLY state update — no dual registry sync.
                    {
                        let mut registry = registry_for_completion.write().await;
                        if let Some(entry) = registry.get_mut(&run_id_clone) {
                            // Respect cancellation — don't overwrite if already cancelled
                            if matches!(entry.status, AsyncTaskStatus::Cancelled) {
                                info!(
                                    "Subagent run {} was cancelled, skipping completion update",
                                    run_id_clone
                                );
                                return Ok(serde_json::json!({
                                    "output": null,
                                    "error": "Cancelled",
                                    "token_usage": null,
                                }));
                            }

                            // Update subagent-specific result in metadata
                            if let TaskMetadata::Subagent(ref mut meta) = entry.metadata {
                                meta.subagent_result = Some(SubagentResult {
                                    status: status.clone(),
                                    output: output.clone(),
                                    error: error.clone(),
                                    token_usage: None, // TODO: Track token usage
                                    completed_at: Utc::now(),
                                });
                            }
                        }
                        // Update status (this also sets completed_at)
                        registry.update_status(&run_id_clone, status);
                    }

                    info!(
                        "Subagent result queued for delivery to {}: run_id={}",
                        parent_session_key_clone, run_id_clone
                    );

                    // Clean up session if cleanup policy is Delete
                    if cleanup_policy_clone == SpawnCleanupPolicy::Delete {
                        info!(
                            "Cleaning up subagent session: run_id={} session_key={}",
                            run_id_clone, child_session_key_clone
                        );
                        let mut manager = session_manager_for_cleanup.write().await;
                        match manager.cleanup_spawn(&child_session_key_clone).await {
                            Ok(true) => {
                                info!("Cleaned up spawn session: {}", child_session_key_clone);
                            }
                            Ok(false) => {
                                warn!(
                                    "Spawn overlay not found for cleanup: {}",
                                    child_session_key_clone
                                );
                            }
                            Err(e) => {
                                warn!("Failed to clean up spawn session: {}", e);
                            }
                        }
                    }

                    // Return async task result as opaque Value
                    Ok(serde_json::json!({
                        "output": output,
                        "error": error,
                        "token_usage": null,
                    }))
                },
            )
            .await?;

        info!(
            "Spawned subagent: run_id={} depth={} isolated={}",
            run_id, child_depth, isolated
        );

        Ok(run_id)
    }

    /// Execute a subagent and wait for completion (sync mode)
    ///
    /// This is similar to `spawn_and_execute` but blocks until the subagent
    /// completes or times out. Used for sequential decomposition patterns.
    ///
    /// Returns the completed run view on success, or an error if the run fails or times out.
    ///
    /// `parent_cancel` is forwarded to `spawn_and_execute` so the
    /// sub-agent's `AgenticLoop` observes the parent's cancel token at
    /// iteration boundaries. When the parent is interrupted via
    /// `PrincipalSendControl`, the sub-agent exits cleanly with
    /// `interrupted: true` and the wait unblocks promptly. `None` for
    /// legacy non-cancelable call sites.
    pub async fn execute_and_wait(
        &self,
        task: &str,
        parent_ctx: Option<&SessionContext>,
        isolated: bool,
        parent_session_key: &str,
        config: ExecutionConfig,
        timeout_secs: u64,
        parent_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<SubagentRunView> {
        // Start the subagent (async mode initially)
        let run_id = self
            .spawn_and_execute(
                task,
                parent_ctx,
                isolated,
                parent_session_key,
                config,
                parent_cancel,
            )
            .await?;

        // Wait for completion using the unified registry.
        // IMPORTANT: Do NOT hold the read lock while sleeping, as the background
        // task needs to acquire a write lock to update status. Holding the read
        // lock continuously would starve the writer and deadlock.
        let wait_result = {
            let start = tokio::time::Instant::now();
            let timeout = Duration::from_secs(timeout_secs);

            // Register a completion waiter so we block on a notification
            // instead of busy-polling every 50ms. A buffer of 1 ensures a
            // completion that lands between registration and `recv()` is not
            // lost.
            let (tx, mut rx) = tokio::sync::mpsc::channel::<AsyncTaskStatus>(1);
            {
                let mut registry = self.registry().write().await;
                registry.register_waiter(&run_id, tx).await?;
            }

            loop {
                // Check status with a brief lock acquisition
                let status = {
                    let registry = self.registry().read().await;
                    registry.check_status(&run_id)
                };

                match status {
                    Some(s) if s.is_terminal() => {
                        let result = match s {
                            AsyncTaskStatus::Completed { result } => {
                                WaitResult::Completed { result }
                            }
                            AsyncTaskStatus::Failed { error } => WaitResult::Failed { error },
                            AsyncTaskStatus::Cancelled => WaitResult::Cancelled,
                            _ => WaitResult::Timeout,
                        };
                        break Ok(result);
                    }
                    None => {
                        break Err(anyhow::anyhow!("Run {run_id} not found in async registry"));
                    }
                    _ => {
                        // Still running — fall through and wait for a
                        // completion notification or the remaining timeout.
                    }
                }

                let remaining = timeout.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    break Ok(WaitResult::Timeout);
                }

                // Block until the task signals completion or the timeout
                // window closes. A spurious or late wakeup simply re-checks
                // status on the next iteration.
                let _ = tokio::time::timeout(remaining, rx.recv()).await;
            }
        };

        // Get the final run state
        let run = self
            .get_run(&run_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Run {run_id} not found after completion"))?;

        match wait_result {
            Ok(WaitResult::Completed { .. }) => Ok(run),
            Ok(WaitResult::Failed { error }) => Err(anyhow::anyhow!("Subagent failed: {error}")),
            Ok(WaitResult::Cancelled) => Err(anyhow::anyhow!("Subagent was cancelled")),
            Ok(WaitResult::Timeout) => {
                // Cancel the run on timeout
                self.cancel(&run_id).await.ok();
                Err(anyhow::anyhow!(
                    "Subagent execution timed out after {timeout_secs}s"
                ))
            }
            Err(e) => Err(anyhow::anyhow!("Error waiting for subagent: {e}")),
        }
    }

    /// Get the current depth for a parent session
    async fn get_parent_depth(&self, parent_session_key: &str) -> u32 {
        let registry = self.registry().read().await;
        registry.get_subagent_depth_for_session(parent_session_key)
    }

    /// Count total active subagent runs
    async fn count_active_runs(&self) -> usize {
        let registry = self.registry().read().await;
        registry
            .list_tasks(None)
            .into_iter()
            .filter(|e| e.tool_name == "Agent" && !e.status.is_terminal())
            .count()
    }

    /// Get status of a run
    pub async fn get_run_status(&self, run_id: &str) -> Option<SubagentStatus> {
        let registry = self.registry().read().await;
        registry.check_status(&run_id.to_string())
    }

    /// Get a run by ID (projected view from unified registry)
    pub async fn get_run(&self, run_id: &str) -> Option<SubagentRunView> {
        let registry = self.registry().read().await;
        registry
            .get(&run_id.to_string())
            .and_then(SubagentRunView::from_entry)
    }

    /// Cancel a running subagent
    ///
    /// Single registry update — no dual sync needed.
    pub async fn cancel(&self, run_id: &str) -> Result<()> {
        self.unified_executor.cancel(&run_id.to_string()).await?;
        info!("Cancelled subagent task: run_id={}", run_id);
        Ok(())
    }

    /// Clean up completed tasks and old registry entries
    pub async fn cleanup(&self) -> usize {
        let mut registry = self.registry().write().await;
        registry.cleanup_old_subagents(chrono::Duration::hours(1))
    }

    /// Shutdown the executor, cancelling all running tasks
    pub async fn shutdown(&self) {
        info!("Shutting down subagent executor...");

        // Cancel all non-terminal subagent tasks in the unified registry
        let mut registry = self.registry().write().await;
        let active_runs: Vec<String> = registry
            .list_tasks(None)
            .into_iter()
            .filter(|e| e.tool_name == "Agent" && !e.status.is_terminal())
            .map(|e| e.task_id.clone())
            .collect();

        for run_id in active_runs {
            registry.update_status(&run_id, AsyncTaskStatus::Cancelled);
            info!(
                "Marked subagent as cancelled during shutdown: run_id={}",
                run_id
            );
        }

        info!("Subagent executor shutdown complete");
    }

    /// Get completed runs that need announcement
    pub async fn get_completed_for_announcement(&self) -> Vec<SubagentRunView> {
        let registry = self.registry().read().await;
        registry
            .list_tasks(None)
            .into_iter()
            .filter(|e| e.tool_name == "Agent" && e.status.is_terminal() && e.result.is_some())
            .filter_map(|e| {
                let view = SubagentRunView::from_entry(&e)?;
                if view.announce_completion {
                    Some(view)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get the announcement sender
    #[must_use]
    pub fn announcement_sender(&self) -> Option<AnnouncementSender> {
        self.announcement_tx.clone()
    }

    /// Send announcement for a completed run
    pub async fn send_announcement(&self, run: &SubagentRunView) -> anyhow::Result<()> {
        if let Some(ref tx) = self.announcement_tx {
            let announcement = crate::agents::subagent_announce::format_announcement(run);
            let completed = CompletedRun {
                run: run.clone(),
                parent_session_key: run.parent_session_key.clone(),
                announcement,
            };
            tx.send(completed)
                .await
                .map_err(|_| anyhow::anyhow!("Announcement channel closed"))?;
        }
        Ok(())
    }
}

/// Execute a subagent task
///
/// This is the core execution function that runs in a background task.
/// It:
/// 1. Loads the child session
/// 2. Creates a subagent Agent sharing the parent's session manager
/// 3. Runs the full `AgenticLoop` via `Agent::execute_with_session`
/// 4. Returns the assistant's final answer
///
/// The child resolves tools from the daemon-global
/// [`crate::extensions::framework::core::global_core`]. The parent's
/// `principal_id` is propagated so the child's own `SubagentExecutor`
/// and any descendant spawns carry the same identity.
async fn execute_subagent_task(
    agent_name: &str,
    session_key: &str,
    system_prompt: &str,
    task_message: &str,
    provider: Option<Arc<crate::providers::Provider>>,
    agent_config: Option<AgentConfig>,
    session_manager: Arc<RwLock<SessionManager>>,
    async_registry: SharedAsyncTaskRegistry,
    principal_id: PrincipalId,
    principal_workspace: Option<std::path::PathBuf>,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<String> {
    info!(
        "Executing subagent task: agent={} session={}",
        agent_name, session_key
    );

    // If no provider, we can't do real execution
    let provider = match provider {
        Some(p) => p,
        None => {
            return Ok(format!(
                "# Subagent Task\n\n**Task:** {task_message}\n\n**Status:** Completed (no provider configured)\n\nThe subagent executed without an LLM provider."
            ));
        }
    };

    // Get the base session key from the session key
    let base_key = crate::session::key::base_key_from_overlay(session_key)
        .unwrap_or_else(|| session_key.to_string());

    // Parse to get agent and peer, then find the child session
    let child_session: Option<Arc<RwLock<crate::session::Session>>> = {
        let parts: Vec<&str> = base_key.split(':').collect();
        if parts.len() >= 5 {
            if let Some(peer_idx) = parts.iter().position(|&p| p == "peer") {
                let agent = parts.get(1).unwrap_or(&agent_name);
                let peer_type = parts.get(peer_idx + 1).unwrap_or(&"agent");
                let peer_id = parts.get(peer_idx + 2).unwrap_or(&"spawn");
                let peer = match *peer_type {
                    "agent" => Subject::Principal(peer_id.to_string().into()),
                    _ => Subject::User(peer_id.to_string()),
                };

                let manager = session_manager.read().await;
                manager.get_existing_base(agent, &peer)
            } else {
                None
            }
        } else {
            None
        }
    };

    let child_session = match child_session {
        Some(s) => s,
        None => {
            return Err(anyhow::anyhow!(
                "Could not find child session for key: {}",
                session_key
            ));
        }
    };

    // Build agent config for the subagent
    let config = agent_config.unwrap_or_else(|| {
        let mut cfg = AgentConfig::default();
        cfg.name = agent_name.to_string();
        cfg
    });

    // Create a shared executor with the parent's registry so nested spawn depth
    // is tracked correctly across the whole tree. Propagate the principal
    // workspace and `principal_id` so grandchildren (and deeper) resolve their
    // subagents from the same workspace and inherit the same principal scope.
    let mut shared_executor_builder = SubagentExecutor::with_registry(
        async_registry,
        Arc::clone(&session_manager),
        agent_name,
        5,
        principal_id.clone(),
    )
    .with_provider(provider.clone())
    .with_agent_config(config.clone());
    if let Some(ref ws) = principal_workspace {
        shared_executor_builder = shared_executor_builder.with_principal_workspace(ws.clone());
    }
    let shared_executor = Arc::new(shared_executor_builder);

    // Create a subagent that shares the parent's session manager and executor registry.
    // Pass the parent's provider through so the child can run its own LLM calls —
    // `new_with_shared_executor` no longer re-resolves a provider (the v1
    // `[provider]` fallback was removed in PR #44) and would otherwise fail
    // `execute_with_session` with "No provider configured".
    let mut subagent = crate::agents::Agent::new_with_shared_executor(
        config,
        session_manager,
        shared_executor,
        Some(provider.clone()),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create subagent: {e}"))?;

    // Scope the child's own `Agent` tool to the principal workspace so it can
    // resolve and delegate to nested subagents (depth 2+).
    if let Some(ws) = principal_workspace {
        subagent = subagent.with_principal_workspace(ws);
    }

    // Update the subagent's session key provider so nested spawns know their parent
    subagent.session_key_provider().set_session_key(session_key);

    // Combine subagent context and task into a single user message.
    // We pass history: None so that run_with_resume prepends the FULL system
    // prompt (including tool definitions from ExtensionCore). Previously we
    // passed the subagent context as a system message in history, which caused
    // run_with_resume to skip the full system prompt — leaving the subagent
    // without knowledge of available tools.
    let combined_prompt = format!("{}\n\n{}", system_prompt, task_message);

    // Execute the agentic loop with the child session
    info!(
        "Starting AgenticLoop for subagent: agent={} session={}",
        agent_name, session_key
    );

    // Clone child_session for potential recovery after execution
    let child_session_for_recovery = child_session.clone();

    let result = subagent
        .execute_with_session(
            &combined_prompt,
            child_session,
            None, // history: None => full system prompt (with tools) is prepended
            cancel,
            |_event| {
                // Non-streaming: ignore events
            },
        )
        .await;

    match result {
        Ok(agentic_result) => {
            let mut final_answer = agentic_result.final_answer;

            // If the final answer is empty, try to recover from the session history.
            // This can happen when the LLM only makes tool calls without producing
            // text (accumulated_text is empty), or when the final assistant message
            // has empty text content.
            if final_answer.trim().is_empty() {
                if let Some(recovered) =
                    crate::agents::subagent_recovery::ResultRecovery::recover_from_session(
                        &child_session_for_recovery,
                    )
                    .await
                {
                    final_answer = recovered;
                }
            }

            info!(
                "Subagent task completed: agent={} session={} success={} iterations={} output_len={}",
                agent_name,
                session_key,
                agentic_result.success,
                agentic_result.iterations,
                final_answer.len()
            );
            Ok(final_answer)
        }
        Err(e) => {
            error!(
                "Subagent task failed: agent={} session={} error={}",
                agent_name, session_key, e
            );
            Err(e)
        }
    }
}

/// Background task manager for the executor
pub struct BackgroundTaskManager {
    /// Reference to the executor
    executor: Arc<SubagentExecutor>,
    /// Cleanup interval
    cleanup_interval: tokio::time::Duration,
}

impl BackgroundTaskManager {
    /// Create a new background task manager
    #[must_use]
    pub fn new(executor: Arc<SubagentExecutor>, cleanup_interval_secs: u64) -> Self {
        Self {
            executor,
            cleanup_interval: tokio::time::Duration::from_secs(cleanup_interval_secs),
        }
    }

    /// Start the background cleanup loop
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(self.cleanup_interval);

        loop {
            interval.tick().await;

            let cleaned = self.executor.cleanup().await;
            if cleaned > 0 {
                tracing::debug!("Cleaned up {} completed subagent tasks", cleaned);
            }
        }
    }

    /// Run cleanup once
    pub async fn cleanup_once(&self) -> usize {
        self.executor.cleanup().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::manager::SessionManager;

    #[tokio::test]
    async fn test_executor_creation() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = SubagentExecutor::new(
            manager,
            "test_agent",
            5,
            crate::principal::PrincipalId::generate(),
        );

        assert_eq!(executor.agent_name, "test_agent");
    }

    #[tokio::test]
    async fn test_execution_config_defaults() {
        let config = ExecutionConfig::default();
        assert_eq!(config.timeout_seconds, 300);
        assert!(matches!(config.cleanup, SpawnCleanupPolicy::Keep));
        assert!(config.label.is_none());
        assert!(config.announce_completion);
        assert_eq!(config.max_depth, 1);
    }

    #[tokio::test]
    async fn test_registry_operations() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = SubagentExecutor::new(
            manager,
            "test_agent",
            5,
            crate::principal::PrincipalId::generate(),
        );

        // Initially empty
        assert_eq!(executor.count_active_runs().await, 0);
    }

    #[tokio::test]
    async fn test_session_cleanup_delete_policy() {
        use crate::auth::Subject;
        use crate::common::PathResolver;

        // Create a session manager with path resolver
        let path_resolver = PathResolver::new();
        let manager = SessionManager::new()
            .with_path_resolver(path_resolver, "test_agent")
            .await
            .unwrap();
        let manager = Arc::new(RwLock::new(manager));

        // Create a parent session
        let parent_peer = Subject::User("parent".to_string());
        {
            let mut mgr = manager.write().await;
            let parent_handle = mgr
                .get_or_create_base("test_agent", &parent_peer)
                .await
                .unwrap();
            let parent_key = {
                let base = parent_handle.read().await;
                base.session_key.clone()
            };
            assert!(parent_key.contains("peer:user:parent"));
        }

        // Create a spawn overlay (simulating what spawn_and_execute does)
        let child_session_key = {
            let mut mgr = manager.write().await;
            let handle = mgr
                .create_spawn_overlay(
                    "test_agent",
                    &Subject::Principal("child".into()),
                    "test task",
                    false,
                    "agent:test_agent:peer:user:parent",
                )
                .await
                .unwrap();
            handle.full_session_key().await
        };
        assert!(child_session_key.contains("overlay:spawn:"));

        // Verify overlay exists
        {
            let mgr = manager.read().await;
            assert!(mgr.get_spawn_overlay(&child_session_key).is_some());
            assert_eq!(mgr.spawn_overlay_count(), 1);
        }

        // Simulate cleanup using cleanup_spawn
        {
            let mut mgr = manager.write().await;
            let cleaned = mgr.cleanup_spawn(&child_session_key).await;
            assert!(cleaned.is_ok(), "cleanup_spawn should succeed");
            assert!(cleaned.unwrap(), "cleanup_spawn should return true");
        }

        // Verify cleanup
        {
            let mgr = manager.read().await;
            assert_eq!(mgr.spawn_overlay_count(), 0);
        }
    }
}
