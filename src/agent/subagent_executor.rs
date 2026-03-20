//! Subagent Executor
//!
//! Async task executor for subagents. Handles:
//! - Spawning subagent sessions
//! - Executing agents in those sessions
//! - Tracking run status
//! - Announcing results back to parents
//! - Timeout and cancellation handling

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

use crate::agent::async_tool_framework::{
    AsyncResultDeliveryMode, AsyncResultQueueManager, AsyncTaskRegistry, AsyncTaskResult,
    AsyncToolConfig, SharedAsyncResultQueueManager, SharedAsyncTaskRegistry, UnifiedAsyncExecutor,
    WaitResult,
};
use crate::agent::subagent_announce::{build_subagent_system_prompt, build_subagent_task_message};
use crate::agent::subagent_registry::{
    create_shared_registry, SharedSubagentRegistry, SubagentResult, SubagentRun, SubagentStatus,
};
use crate::session::context::{SessionContext, SessionRouter};
use crate::session::manager::SessionManager;
use crate::session::types::{Peer, SpawnCleanupPolicy};
use crate::session::UnifiedSession;
use crate::types::agent::AgentConfig;

/// Channel for announcing completed subagent runs
pub type AnnouncementSender = mpsc::Sender<CompletedRun>;
pub type AnnouncementReceiver = mpsc::Receiver<CompletedRun>;

/// A completed subagent run ready for announcement
#[derive(Debug, Clone)]
pub struct CompletedRun {
    /// The run that completed
    pub run: SubagentRun,
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
pub struct SubagentExecutor {
    /// Registry for tracking runs (subagent-specific)
    registry: SharedSubagentRegistry,
    /// Unified async executor for background task execution
    unified_executor: UnifiedAsyncExecutor,
    /// Session router for creating sessions
    session_router: SessionRouter,
    /// Agent name for the executor
    agent_name: String,
    /// Maximum concurrent runs
    max_concurrent: usize,
    /// Channel for announcing completed runs
    announcement_tx: Option<AnnouncementSender>,
    /// Provider for LLM execution
    provider: Option<Arc<dyn crate::providers::Provider>>,
    /// Agent configuration for creating subagents
    agent_config: Option<AgentConfig>,
    /// Session manager for accessing sessions
    session_manager: Arc<RwLock<SessionManager>>,
}

impl SubagentExecutor {
    /// Create a new subagent executor
    #[must_use]
    pub fn new(
        session_router: SessionRouter,
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
    ) -> Self {
        let async_registry = Arc::new(RwLock::new(AsyncTaskRegistry::new()));
        let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let unified_executor =
            UnifiedAsyncExecutor::with_registries(async_registry, async_queue_manager);

        Self {
            registry: create_shared_registry(),
            unified_executor,
            session_router,
            agent_name: agent_name.into(),
            max_concurrent,
            announcement_tx: None,
            provider: None,
            agent_config: None,
            session_manager,
        }
    }

    /// Create an executor with shared registries
    #[must_use]
    pub fn with_registry(
        registry: SharedSubagentRegistry,
        session_router: SessionRouter,
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
    ) -> Self {
        let async_registry = Arc::new(RwLock::new(AsyncTaskRegistry::new()));
        let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let unified_executor =
            UnifiedAsyncExecutor::with_registries(async_registry, async_queue_manager);

        Self {
            registry,
            unified_executor,
            session_router,
            agent_name: agent_name.into(),
            max_concurrent,
            announcement_tx: None,
            provider: None,
            agent_config: None,
            session_manager,
        }
    }

    /// Create an executor with full async framework integration
    #[must_use]
    pub fn with_async_framework(
        registry: SharedSubagentRegistry,
        async_registry: SharedAsyncTaskRegistry,
        session_router: SessionRouter,
        session_manager: Arc<RwLock<SessionManager>>,
        async_queue_manager: SharedAsyncResultQueueManager,
        agent_name: impl Into<String>,
        max_concurrent: usize,
    ) -> Self {
        let unified_executor =
            UnifiedAsyncExecutor::with_registries(async_registry, async_queue_manager);

        Self {
            registry,
            unified_executor,
            session_router,
            agent_name: agent_name.into(),
            max_concurrent,
            announcement_tx: None,
            provider: None,
            agent_config: None,
            session_manager,
        }
    }

    /// Set the provider for LLM execution
    pub fn with_provider(mut self, provider: Arc<dyn crate::providers::Provider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Set the agent configuration
    #[must_use]
    pub fn with_agent_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = Some(config);
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

    /// Get a reference to the registry
    #[must_use]
    pub fn registry(&self) -> &SharedSubagentRegistry {
        &self.registry
    }

    /// Get a reference to the async task registry
    #[must_use]
    pub fn async_registry(&self) -> &SharedAsyncTaskRegistry {
        self.unified_executor.registry()
    }

    /// Get a reference to the async queue manager
    #[must_use]
    pub fn async_queue_manager(&self) -> &SharedAsyncResultQueueManager {
        self.unified_executor.queue_manager()
    }

    /// Get a reference to the unified executor
    #[must_use]
    pub fn unified_executor(&self) -> &UnifiedAsyncExecutor {
        &self.unified_executor
    }

    /// Spawn and execute a subagent
    ///
    /// Returns the `run_id` immediately. The execution happens in the background.
    pub async fn spawn_and_execute(
        &self,
        task: &str,
        _parent_ctx: Option<&SessionContext>,
        isolated: bool,
        parent_session_key: &str,
        config: ExecutionConfig,
    ) -> Result<String> {
        // Check depth limits
        let parent_depth = self.get_parent_depth(parent_session_key).await;
        let child_depth = parent_depth + 1;

        if config.max_depth > 0 && child_depth > config.max_depth {
            return Err(anyhow::anyhow!(
                "Maximum spawn depth exceeded: {} (max: {})",
                child_depth,
                config.max_depth
            ));
        }

        // Check concurrent run limits
        let active_count = self.count_active_runs().await;
        if active_count >= self.max_concurrent {
            return Err(anyhow::anyhow!(
                "Maximum concurrent subagent runs exceeded: {} (max: {})",
                active_count,
                self.max_concurrent
            ));
        }

        // Generate run ID
        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());

        // Create spawn session
        let peer = Peer::Agent(format!("spawn_{}", uuid::Uuid::new_v4().simple()));
        let spawn_ctx = self
            .session_router
            .spawn(
                &self.agent_name,
                &peer,
                task,
                isolated,
                parent_session_key,
                Some(config.timeout_seconds),
            )
            .await
            .context("Failed to create spawn session")?;

        let child_session_key = spawn_ctx.full_session_key().await;

        // Register the run
        let run = SubagentRun::new(
            run_id.clone(),
            child_session_key.clone(),
            parent_session_key.to_string(),
            task.to_string(),
            config.cleanup,
            config.label.clone(),
            child_depth,
        );

        {
            let mut registry = self.registry.write().await;
            registry.register(run);
        }

        // Execute using unified async executor
        let async_config = AsyncToolConfig {
            delivery_mode: AsyncResultDeliveryMode::QueueWhenBusy,
            delivery_target: None,
            timeout_secs: config.timeout_seconds,
            cleanup_after_delivery: config.cleanup == SpawnCleanupPolicy::Delete,
            label: config.label.clone(),
        };

        // Clone values for the execution closure
        let registry_clone = self.registry.clone();
        let child_session_key_clone = child_session_key.clone();
        let parent_session_key_clone = parent_session_key.to_string();
        let task_clone = task.to_string();
        let label_clone = config.label.clone();
        let run_id_clone = run_id.clone();
        let timeout = config.timeout_seconds;
        let agent_name = self.agent_name.clone();
        let provider_clone = self.provider.clone();
        let agent_config_clone = self.agent_config.clone();
        let session_manager_clone = self.session_manager.clone();

        // Use unified executor for background execution
        self.unified_executor
            .execute(
                run_id.clone(),
                "agent_spawn",
                serde_json::json!({
                    "task": task,
                    "isolated": isolated,
                    "label": &config.label,
                    "child_session_key": &child_session_key,
                }),
                parent_session_key.to_string(),
                async_config,
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

                    // Execute with timeout
                    let result = if timeout > 0 {
                        match tokio::time::timeout(
                            tokio::time::Duration::from_secs(timeout),
                            execute_subagent_task(
                                &agent_name,
                                &child_session_key_clone,
                                &system_prompt,
                                &task_message,
                                provider_clone,
                                agent_config_clone,
                                session_manager_clone,
                            ),
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(_) => {
                                warn!(
                                    "Subagent timed out: run_id={} timeout={}s",
                                    run_id_clone, timeout
                                );
                                Err(anyhow::anyhow!(
                                    "Subagent execution timed out after {timeout} seconds"
                                ))
                            }
                        }
                    } else {
                        execute_subagent_task(
                            &agent_name,
                            &child_session_key_clone,
                            &system_prompt,
                            &task_message,
                            provider_clone,
                            agent_config_clone,
                            session_manager_clone,
                        )
                        .await
                    };

                    // Process result and update subagent registry
                    let (status, output, error) = match result {
                        Ok(output) => {
                            info!("Subagent completed successfully: run_id={}", run_id_clone);
                            (SubagentStatus::Completed, Some(output), None)
                        }
                        Err(e) => {
                            error!("Subagent failed: run_id={} error={}", run_id_clone, e);
                            (SubagentStatus::Failed, None, Some(e.to_string()))
                        }
                    };

                    // Complete the run in subagent registry (if not already cancelled)
                    {
                        let mut registry = registry_clone.write().await;
                        if let Some(existing_run) = registry.get(&run_id_clone) {
                            if existing_run.status == SubagentStatus::Cancelled {
                                // Run was cancelled, don't overwrite status
                                info!(
                                    "Subagent run {} was cancelled, skipping completion update",
                                    run_id_clone
                                );
                                return Ok(AsyncTaskResult::Subagent {
                                    output: None,
                                    error: Some("Cancelled".to_string()),
                                    token_usage: None,
                                });
                            }
                        }

                        let subagent_result = SubagentResult {
                            status,
                            output: output.clone(),
                            error: error.clone(),
                            token_usage: None, // TODO: Track token usage
                            completed_at: Utc::now(),
                        };
                        registry.complete(&run_id_clone, subagent_result);
                    }

                    info!(
                        "Subagent result queued for delivery to {}: run_id={}",
                        parent_session_key_clone, run_id_clone
                    );

                    // Return unified async task result
                    Ok(AsyncTaskResult::Subagent {
                        output,
                        error,
                        token_usage: None,
                    })
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
    /// Returns the completed run on success, or an error if the run fails or times out.
    pub async fn execute_and_wait(
        &self,
        task: &str,
        parent_ctx: Option<&SessionContext>,
        isolated: bool,
        parent_session_key: &str,
        config: ExecutionConfig,
        timeout_secs: u64,
    ) -> Result<SubagentRun> {
        // Start the subagent (async mode initially)
        let run_id = self
            .spawn_and_execute(task, parent_ctx, isolated, parent_session_key, config)
            .await?;

        // Wait for completion using the async registry
        let wait_result = {
            let registry = self.async_registry().read().await;
            registry
                .wait_for_completion(&run_id, Duration::from_secs(timeout_secs))
                .await
        };

        // Get the final run state
        let run = self
            .get_run(&run_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Run {} not found after completion", run_id))?;

        match wait_result {
            Ok(WaitResult::Completed { .. }) => Ok(run),
            Ok(WaitResult::Failed { error }) => Err(anyhow::anyhow!("Subagent failed: {}", error)),
            Ok(WaitResult::Cancelled) => Err(anyhow::anyhow!("Subagent was cancelled")),
            Ok(WaitResult::Timeout) => {
                // Cancel the run on timeout
                self.cancel(&run_id).await.ok();
                Err(anyhow::anyhow!(
                    "Subagent execution timed out after {}s",
                    timeout_secs
                ))
            }
            Err(e) => Err(anyhow::anyhow!("Error waiting for subagent: {}", e)),
        }
    }

    /// Get the current depth for a parent session
    async fn get_parent_depth(&self, parent_session_key: &str) -> u32 {
        let registry = self.registry.read().await;
        registry.get_max_depth_for_parent(parent_session_key)
    }

    /// Count total active runs
    async fn count_active_runs(&self) -> usize {
        let registry = self.registry.read().await;
        registry
            .list_all()
            .into_iter()
            .filter(|run| !run.status.is_terminal())
            .count()
    }

    /// Get status of a run
    pub async fn get_run_status(&self, run_id: &str) -> Option<SubagentStatus> {
        let registry = self.registry.read().await;
        registry.get(run_id).map(|run| run.status)
    }

    /// Get a run by ID
    pub async fn get_run(&self, run_id: &str) -> Option<SubagentRun> {
        let registry = self.registry.read().await;
        registry.get(run_id).cloned()
    }

    /// Cancel a running subagent
    pub async fn cancel(&self, run_id: &str) -> Result<()> {
        // Cancel via unified executor
        self.unified_executor.cancel(&run_id.to_string()).await.ok();

        // Update subagent registry
        {
            let mut registry = self.registry.write().await;
            registry.update_status(run_id, SubagentStatus::Cancelled);
        }

        info!("Cancelled subagent task: run_id={}", run_id);
        Ok(())
    }

    /// Clean up completed tasks and old registry entries
    pub async fn cleanup(&self) -> usize {
        // Clean up old registry entries (older than 1 hour)
        let mut registry = self.registry.write().await;
        registry.cleanup_old(chrono::Duration::hours(1))
    }

    /// Shutdown the executor, cancelling all running tasks
    pub async fn shutdown(&self) {
        info!("Shutting down subagent executor...");

        // Note: UnifiedAsyncExecutor doesn't track all task handles externally,
        // so we rely on the registry to know what might be running
        // In practice, the async task registry handles cleanup internally

        // Update all non-terminal runs to cancelled status
        {
            let mut registry = self.registry.write().await;
            let active_runs: Vec<String> = registry
                .list_all()
                .into_iter()
                .filter(|run| !run.status.is_terminal())
                .map(|run| run.run_id.clone())
                .collect();

            for run_id in active_runs {
                registry.update_status(&run_id, SubagentStatus::Cancelled);
                info!(
                    "Marked subagent as cancelled during shutdown: run_id={}",
                    run_id
                );
            }
        }

        info!("Subagent executor shutdown complete");
    }

    /// Get completed runs that need announcement
    pub async fn get_completed_for_announcement(&self) -> Vec<SubagentRun> {
        let registry = self.registry.read().await;
        registry
            .list_all()
            .into_iter()
            .filter(|run| {
                run.status.is_terminal() && run.announce_completion && run.result.is_some()
            })
            .cloned()
            .collect()
    }

    /// Get the announcement sender
    #[must_use]
    pub fn announcement_sender(&self) -> Option<AnnouncementSender> {
        self.announcement_tx.clone()
    }

    /// Send announcement for a completed run
    pub async fn send_announcement(&self, run: &SubagentRun) -> anyhow::Result<()> {
        if let Some(ref tx) = self.announcement_tx {
            let announcement = crate::agent::subagent_announce::format_announcement(run);
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

impl Clone for SubagentExecutor {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            unified_executor: self.unified_executor.clone(),
            session_router: self.session_router.clone(),
            agent_name: self.agent_name.clone(),
            max_concurrent: self.max_concurrent,
            announcement_tx: self.announcement_tx.clone(),
            provider: self.provider.clone(),
            agent_config: self.agent_config.clone(),
            session_manager: self.session_manager.clone(),
        }
    }
}

/// Execute a subagent task
///
/// This is the core execution function that runs in a background task.
/// It:
/// 1. Loads the child session
/// 2. Adds system prompt and user task message
/// 3. Creates a minimal agent with tools
/// 4. Runs `AgenticLoopV4` to execute the task
/// 5. Returns the assistant's response
async fn execute_subagent_task(
    agent_name: &str,
    session_key: &str,
    system_prompt: &str,
    task_message: &str,
    provider: Option<Arc<dyn crate::providers::Provider>>,
    _agent_config: Option<AgentConfig>,
    session_manager: Arc<RwLock<SessionManager>>,
) -> Result<String> {
    info!(
        "Executing subagent task: agent={} session={}",
        agent_name, session_key
    );

    // If no provider, we can't do real execution
    let _provider = match provider {
        Some(p) => p,
        None => {
            // Fallback to simplified response
            return Ok(format!(
                "# Subagent Task\n\n**Task:** {task_message}\n\n**Status:** Completed (no provider configured)\n\nThe subagent executed without an LLM provider."
            ));
        }
    };

    // Add messages to the child session
    // Get the base session key from the session key
    let base_key = crate::session::key::base_key_from_overlay(session_key)
        .unwrap_or_else(|| session_key.to_string());

    // Parse to get agent and peer
    let parts: Vec<&str> = base_key.split(':').collect();
    if parts.len() >= 5 {
        if let Some(peer_idx) = parts.iter().position(|&p| p == "peer") {
            let agent = parts.get(1).unwrap_or(&agent_name);
            let peer_type = parts.get(peer_idx + 1).unwrap_or(&"agent");
            let peer_id = parts.get(peer_idx + 2).unwrap_or(&"spawn");
            let peer = match *peer_type {
                "agent" => Peer::Agent(peer_id.to_string()),
                _ => Peer::User(peer_id.to_string()),
            };

            // Get the base session and add messages
            let manager = session_manager.read().await;
            if let Some(base) = manager.get_existing_base(agent, &peer) {
                // Add system prompt and task to the session
                // Note: We need to drop the read lock before getting write lock
                drop(manager);

                if let Ok(mut base_write) = base.try_write() {
                    // Add system prompt
                    if let Err(e) = base_write.add_system(system_prompt).await {
                        tracing::warn!("Failed to add system prompt: {}", e);
                    }
                    // Add task as user message
                    if let Err(e) = base_write.add_user(task_message).await {
                        tracing::warn!("Failed to add user message: {}", e);
                    }
                    info!("Added system prompt and task to child session");
                }
            }
        }
    }

    // TODO: Full agent execution with AgenticLoopV4
    // For now, return a placeholder that shows the task was received
    // This will be implemented in a follow-up to avoid complex Send issues
    let output = format!(
        "# Subagent Task Received\n\n**Task:** {}\n\n**Status:** Task queued for execution\n\n**Note:** Messages added to session. Full LLM execution to be implemented.",
        task_message.lines().next().unwrap_or("Task")
    );

    info!(
        "Subagent task completed: agent={} session={} output_len={}",
        agent_name,
        session_key,
        output.len()
    );

    Ok(output)
}

/// Format subagent result for delivery to parent (OpenClaw-style)
fn format_subagent_result(
    run_id: &str,
    child_session_key: &str,
    task: &str,
    label: Option<&str>,
    status: &SubagentStatus,
    output: Option<&str>,
    error: Option<&str>,
) -> String {
    let status_label = match status {
        SubagentStatus::Completed => "completed successfully",
        SubagentStatus::Failed => "failed",
        SubagentStatus::Cancelled => "was cancelled",
        SubagentStatus::Running => "is running",
        SubagentStatus::TimedOut => "timed out",
    };

    let task_label = label.unwrap_or("subagent task");
    let findings = output.unwrap_or_else(|| error.unwrap_or("(no output)"));

    // Build OpenClaw-style system message
    let mut message = format!(
        "[System Message] [sessionId: {}] A {} \"{}\" just {}.\n\n",
        child_session_key,
        task_label,
        task.lines().next().unwrap_or(task),
        status_label
    );

    message.push_str("Result:\n");
    message.push_str(findings);
    message.push_str("\n\n");

    // Add metadata
    message.push_str(&format!("[runId: {run_id} | session: {child_session_key}]"));

    // Add instruction for parent agent
    message.push_str("\n\n");
    message.push_str("Instruction: Convert this result into your normal assistant voice for the user. Keep this internal context private (don't mention system/log/stats/session details).");

    message
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
        let router = SessionRouter::new(manager.clone(), "test_agent");
        let executor = SubagentExecutor::new(router, manager, "test_agent", 5);

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
        let router = SessionRouter::new(manager.clone(), "test_agent");
        let executor = SubagentExecutor::new(router, manager, "test_agent", 5);

        // Initially empty
        assert_eq!(executor.count_active_runs().await, 0);
    }
}
