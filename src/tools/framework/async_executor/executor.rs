//! Unified executor for all async tool operations

use super::delivery::{QueueDelivery, ResultDelivery};
use super::queue::{AsyncResultQueueManager, SharedAsyncResultQueueManager};
use super::registry::{
    AsyncTaskEntry, AsyncTaskRegistry, SharedAsyncTaskRegistry, TaskMetadata,
};
use super::task_file::{TaskFileRecord, TaskFileWriter};
use super::types::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskStatus, AsyncToolConfig,
    DeliveryTarget, WaitResult,
};
use crate::tools::core::traits::ToolResult;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Internal outcome of executing an async task, distinguishing timeout from failure
enum TaskOutcome {
    Success(Value),
    Failure(anyhow::Error),
    Timeout,
}

/// Unified executor for all async tool operations
///
/// This provides a single entry point for executing async tasks with:
/// - Task registration and tracking
/// - Task file writing for agent polling
/// - Automatic status updates
/// - Result formatting and caching
#[derive(Clone)]
pub struct AsyncExecutor {
    /// Task registry for tracking all async operations
    registry: SharedAsyncTaskRegistry,
    /// Queue manager for queue-based delivery (deprecated, kept for compatibility)
    queue_manager: SharedAsyncResultQueueManager,
    /// Registered delivery mechanisms by target type
    deliveries: Arc<RwLock<HashMap<DeliveryTarget, Box<dyn ResultDelivery>>>>,
    /// Default delivery target
    default_delivery: DeliveryTarget,
    /// Task file writer for disk-based polling
    task_file_writer: Option<TaskFileWriter>,
}

impl AsyncExecutor {
    /// Create a new unified async executor
    #[must_use]
    pub fn new() -> Self {
        let task_file_writer = crate::common::paths::default_data_dir()
            .join("async_tasks")
            .into();
        Self {
            registry: Arc::new(RwLock::new(AsyncTaskRegistry::new())),
            queue_manager: Arc::new(RwLock::new(AsyncResultQueueManager::new())),
            deliveries: Arc::new(RwLock::new(HashMap::new())),
            default_delivery: DeliveryTarget::AsyncQueue,
            task_file_writer: Some(TaskFileWriter::new(task_file_writer)),
        }
    }

    /// Create with existing registries (for sharing with other components)
    #[must_use]
    pub fn with_registries(
        registry: SharedAsyncTaskRegistry,
        queue_manager: SharedAsyncResultQueueManager,
    ) -> Self {
        let task_file_writer = crate::common::paths::default_data_dir()
            .join("async_tasks")
            .into();
        Self {
            registry,
            queue_manager,
            deliveries: Arc::new(RwLock::new(HashMap::new())),
            default_delivery: DeliveryTarget::AsyncQueue,
            task_file_writer: Some(TaskFileWriter::new(task_file_writer)),
        }
    }

    /// Register a delivery mechanism for a target type
    pub async fn register_delivery(
        &self,
        target: DeliveryTarget,
        delivery: Box<dyn ResultDelivery>,
    ) {
        let mut deliveries = self.deliveries.write().await;
        deliveries.insert(target, delivery);
    }

    /// Set the default delivery target
    #[must_use]
    pub fn with_default_delivery(mut self, target: DeliveryTarget) -> Self {
        self.default_delivery = target;
        self
    }

    /// Set a custom task file writer
    pub fn with_task_file_writer(mut self, writer: TaskFileWriter) -> Self {
        self.task_file_writer = Some(writer);
        self
    }

    /// Get the task file writer
    #[must_use]
    pub fn task_file_writer(&self) -> Option<&TaskFileWriter> {
        self.task_file_writer.as_ref()
    }

    /// Get a reference to the task registry
    #[must_use]
    pub fn registry(&self) -> &SharedAsyncTaskRegistry {
        &self.registry
    }

    /// Get a reference to the queue manager
    #[must_use]
    pub fn queue_manager(&self) -> &SharedAsyncResultQueueManager {
        &self.queue_manager
    }

    /// Execute an async task with the unified executor (internal)
    async fn execute_inner(
        &self,
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        parent_session_key: String,
        config: AsyncToolConfig,
        metadata: TaskMetadata,
        execution_fn: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Value>> + Send>,
                > + Send,
        >,
    ) -> Result<AsyncTaskReceipt> {
        // Determine task file path
        let task_file = self
            .task_file_writer
            .as_ref()
            .map(|w| w.task_file_path(&task_id));

        // Create initial task file record
        if let Some(ref writer) = self.task_file_writer {
            let mut record = TaskFileRecord::new(task_id.clone(), tool_name.clone());
            record.params = Some(params.clone());
            record.timeout_requested = Some(config.timeout_secs);
            record.callback_mode = config
                .delivery_target
                .map(|dt| format!("{:?}", dt).to_lowercase());
            if let Err(e) = writer.write(&record).await {
                tracing::warn!("Failed to write initial task file for {}: {}", task_id, e);
            }
        }

        // Create task entry (with metadata if provided)
        let entry = if matches!(metadata, TaskMetadata::None) {
            AsyncTaskEntry::new(
                task_id.clone(),
                tool_name.clone(),
                params.clone(),
                parent_session_key.clone(),
                config.clone(),
            )
        } else {
            AsyncTaskEntry::with_metadata(
                task_id.clone(),
                tool_name.clone(),
                params.clone(),
                parent_session_key.clone(),
                config.clone(),
                metadata,
            )
        };

        // Register task
        {
            let mut registry = self.registry.write().await;
            registry.register(entry);
        }

        // Determine delivery target and mechanism
        let delivery_target = config.delivery_target.unwrap_or(self.default_delivery);
        let delivery = {
            let deliveries = self.deliveries.read().await;
            deliveries.get(&delivery_target).cloned()
        };

        // Fall back to queue delivery if no specific mechanism registered
        let delivery: Box<dyn ResultDelivery> = match delivery {
            Some(d) => d,
            None => Box::new(QueueDelivery::new(self.queue_manager.clone())),
        };

        // Clone what we need for the spawned task
        let registry_clone = self.registry.clone();
        let task_id_clone = task_id.clone();
        let task_file_writer_clone = self.task_file_writer.clone();
        let timeout_secs = config.timeout_secs;
        let callback_mode = config
            .delivery_target
            .map(|dt| format!("{:?}", dt).to_lowercase());
        let params_for_spawn = params.clone();

        // Spawn the background execution
        tokio::spawn(async move {
            // Update status to running
            {
                let mut registry = registry_clone.write().await;
                registry.update_status(&task_id_clone, AsyncTaskStatus::Running);
            }
            if let Some(ref writer) = task_file_writer_clone {
                let mut record = TaskFileRecord::new(task_id_clone.clone(), tool_name.clone());
                record.params = Some(params_for_spawn.clone());
                record.timeout_requested = Some(timeout_secs);
                record.callback_mode = callback_mode.clone();
                record.set_running();
                if let Err(e) = writer.write(&record).await {
                    tracing::warn!(
                        "Failed to write running task file for {}: {}",
                        task_id_clone,
                        e
                    );
                }
            }

            // Execute the work with timeout enforcement
            let timeout_duration = std::time::Duration::from_secs(timeout_secs);
            let outcome = match tokio::time::timeout(timeout_duration, execution_fn()).await {
                Ok(Ok(value)) => TaskOutcome::Success(value),
                Ok(Err(e)) => TaskOutcome::Failure(e),
                Err(_) => TaskOutcome::Timeout,
            };

            // Check if task was cancelled before updating
            let was_cancelled = {
                let registry = registry_clone.read().await;
                registry
                    .get(&task_id_clone)
                    .map(|e| matches!(e.status, AsyncTaskStatus::Cancelled))
                    .unwrap_or(false)
            };

            if was_cancelled {
                tracing::debug!(
                    "Task {} was cancelled, skipping result update",
                    task_id_clone
                );
                return;
            }

            // Map outcome to status and update registry
            let status = match &outcome {
                TaskOutcome::Success(_) => AsyncTaskStatus::Completed {
                    result: ToolResult::success(serde_json::json!({"completed": true})),
                },
                TaskOutcome::Failure(e) => AsyncTaskStatus::Failed {
                    error: e.to_string(),
                },
                TaskOutcome::Timeout => AsyncTaskStatus::TimedOut {
                    error: format!("Task timed out after {}s", timeout_secs),
                },
            };

            {
                let mut registry = registry_clone.write().await;
                registry.update_status(&task_id_clone, status.clone());

                // Store the result
                if let TaskOutcome::Success(ref value) = outcome {
                    if let Some(entry) = registry.get_mut(&task_id_clone) {
                        entry.set_result(value.clone());
                    }
                }
            }

            // Write final task file record
            if let Some(ref writer) = task_file_writer_clone {
                let mut record = TaskFileRecord::new(task_id_clone.clone(), tool_name.clone());
                record.params = Some(params_for_spawn.clone());
                record.timeout_requested = Some(timeout_secs);
                record.callback_mode = callback_mode.clone();
                match outcome {
                    TaskOutcome::Success(value) => {
                        record.set_completed(value);
                    }
                    TaskOutcome::Failure(e) => {
                        record.set_failed(e.to_string());
                    }
                    TaskOutcome::Timeout => {
                        record.set_timed_out(format!("Task timed out after {}s", timeout_secs));
                    }
                }
                if let Err(e) = writer.write(&record).await {
                    tracing::warn!(
                        "Failed to write final task file for {}: {}",
                        task_id_clone,
                        e
                    );
                }
            }

            // Deliver the result
            if let Some(entry) = registry_clone.read().await.get(&task_id_clone) {
                if let Err(e) = delivery.deliver(entry).await {
                    tracing::debug!("Delivery result for task {}: {}", task_id_clone, e);
                }
            }
        });

        // Return receipt immediately
        Ok(AsyncTaskReceipt {
            task_id: task_id.clone(),
            status: AsyncTaskStatus::Pending,
            estimated_duration_secs: None,
            task_file,
            params: Some(params.clone()),
        })
    }

    /// Execute an async task with the unified executor
    pub async fn execute<F, Fut>(
        &self,
        task_id: AsyncTaskId,
        tool_name: impl Into<String>,
        params: Value,
        parent_session_key: impl Into<String>,
        config: AsyncToolConfig,
        execution_fn: F,
    ) -> Result<AsyncTaskReceipt>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let tool_name = tool_name.into();
        let parent_session_key = parent_session_key.into();

        // Box the generic closure so it can be passed to the non-generic inner method
        let boxed_fn: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Value>> + Send>,
                > + Send,
        > = Box::new(move || Box::pin(execution_fn()));

        self.execute_inner(
            task_id,
            tool_name,
            params,
            parent_session_key,
            config,
            TaskMetadata::None,
            boxed_fn,
        )
        .await
    }

    /// Execute an async task with metadata attached to the registry entry.
    ///
    /// This is used by domain-specific executors (e.g., `SubagentExecutor`)
    /// to attach structured metadata to a task without the generic executor
    /// needing to know about domain types.
    pub async fn execute_with_metadata<F, Fut>(
        &self,
        task_id: AsyncTaskId,
        tool_name: impl Into<String>,
        params: Value,
        parent_session_key: impl Into<String>,
        config: AsyncToolConfig,
        metadata: TaskMetadata,
        execution_fn: F,
    ) -> Result<AsyncTaskReceipt>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let tool_name = tool_name.into();
        let parent_session_key = parent_session_key.into();

        let boxed_fn: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Value>> + Send>,
                > + Send,
        > = Box::new(move || Box::pin(execution_fn()));

        self.execute_inner(
            task_id,
            tool_name,
            params,
            parent_session_key,
            config,
            metadata,
            boxed_fn,
        )
        .await
    }

    /// Execute an async task with a boxed future
    pub async fn execute_boxed(
        &self,
        task_id: AsyncTaskId,
        tool_name: impl Into<String>,
        params: Value,
        parent_session_key: impl Into<String>,
        config: AsyncToolConfig,
        execution_fn: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Value>> + Send>,
                > + Send,
        >,
    ) -> Result<AsyncTaskReceipt> {
        let tool_name = tool_name.into();
        let parent_session_key = parent_session_key.into();

        self.execute_inner(
            task_id,
            tool_name,
            params,
            parent_session_key,
            config,
            TaskMetadata::None,
            execution_fn,
        )
        .await
    }

    /// Wait for a task to complete (sync mode)
    pub async fn wait_for_completion(
        &self,
        task_id: &AsyncTaskId,
        timeout: Duration,
    ) -> Result<WaitResult> {
        let registry = self.registry.read().await;
        registry.wait_for_completion(task_id, timeout).await
    }

    /// Get the current status of a task
    pub async fn check_status(&self, task_id: &AsyncTaskId) -> Option<AsyncTaskStatus> {
        let registry = self.registry.read().await;
        registry.check_status(task_id)
    }

    /// Cancel a running task
    pub async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool> {
        let mut registry = self.registry.write().await;
        if let Some(entry) = registry.get_mut(task_id) {
            if !entry.status.is_terminal() {
                entry.status = AsyncTaskStatus::Cancelled;
                entry.completed_at = Some(chrono::Utc::now());
                entry.notify_completion();
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Wait for all tasks to reach a terminal state
    pub async fn wait_for_all_tasks(&self, timeout: Duration) {
        let start = tokio::time::Instant::now();
        loop {
            let has_pending = {
                let registry = self.registry.read().await;
                registry.has_pending_tasks()
            };
            if !has_pending {
                break;
            }
            if start.elapsed() >= timeout {
                tracing::warn!("Timeout waiting for async tasks to complete");
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// List all tasks in the registry, optionally filtered by session_key
    pub async fn list_tasks(&self, session_key: Option<&str>) -> Vec<AsyncTaskEntry> {
        let registry = self.registry.read().await;
        registry.list_tasks(session_key)
    }

    /// Run janitor: clean old task files and purge stale registry entries
    pub async fn run_janitor(&self, file_ttl: Duration) -> Result<(usize, usize)> {
        let files_removed = if let Some(ref writer) = self.task_file_writer {
            writer.cleanup_old(file_ttl).await?
        } else {
            0
        };

        let registry_purged = {
            let mut registry = self.registry.write().await;
            registry.cleanup_completed()
        };

        Ok((files_removed, registry_purged))
    }
}

impl std::fmt::Debug for AsyncExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncExecutor")
            .field("registry", &"<AsyncTaskRegistry>")
            .field("queue_manager", &"<AsyncResultQueueManager>")
            .field(
                "deliveries",
                &"<HashMap<DeliveryTarget, Box<dyn ResultDelivery>>>",
            )
            .field("default_delivery", &self.default_delivery)
            .field("task_file_writer", &self.task_file_writer)
            .finish()
    }
}

impl Default for AsyncExecutor {
    fn default() -> Self {
        Self::new()
    }
}
