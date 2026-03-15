//! Async Tool Framework - Generalized async tool execution with result queuing
//!
//! This module provides infrastructure for tools that execute asynchronously:
//! - Tool returns a receipt immediately (non-blocking)
//! - Background task executes the actual work
//! - Result is queued and delivered when the agent is ready
//!
//! Design inspired by `OpenClaw`'s subagent announcement queue, but generalized
//! for any async tool operation.

use crate::tools::traits::ToolResult;
use anyhow::Result;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

/// Unique identifier for an async task
pub type AsyncTaskId = String;

/// Status of an async task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AsyncTaskStatus {
    Pending,
    Running,
    Completed { result: ToolResult },
    Failed { error: String },
    Cancelled,
}

impl AsyncTaskStatus {
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AsyncTaskStatus::Completed { .. }
                | AsyncTaskStatus::Failed { .. }
                | AsyncTaskStatus::Cancelled
        )
    }
}

/// Receipt returned to agent when spawning an async task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncTaskReceipt {
    pub task_id: AsyncTaskId,
    pub status: AsyncTaskStatus,
    pub estimated_duration_secs: Option<u64>,
    pub check_status_tool: String, // Tool name to check status
}

/// Unified result type for all async operations
/// 
/// This enum normalizes results from different async tools (process, agent_spawn, agent_invoke)
/// into a single format that can be handled uniformly by the delivery infrastructure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AsyncTaskResult {
    /// Shell command result (from process tool)
    Process {
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    /// Subagent execution result
    Subagent {
        output: Option<String>,
        error: Option<String>,
        token_usage: Option<(u32, u32, u32)>,
    },
    /// Agent-to-agent invocation result
    Invocation {
        content: String,
        from: String,
        success: bool,
        error: Option<String>,
    },
    /// Generic tool result for extensibility
    Generic {
        data: serde_json::Value,
    },
}

impl AsyncTaskResult {
    /// Format the result for display/announcement to users
    #[must_use]
    pub fn format_for_announcement(&self, tool_name: &str) -> String {
        match self {
            Self::Process { stdout, stderr, exit_code } => {
                format!(
                    "## Process Result\n\n**Command:** {}\n**Exit Code:** {}\n\n**Stdout:**\n```\n{}\n```\n\n**Stderr:**\n```\n{}\n```",
                    tool_name, exit_code, stdout, stderr
                )
            }
            Self::Subagent { output, error, .. } => {
                let label_part = if tool_name == "agent_spawn" {
                    "Subagent".to_string()
                } else {
                    format!("Subagent [{}]", tool_name)
                };
                
                let status_emoji = if error.is_some() { "❌" } else { "✅" };
                let content = error.as_deref()
                    .or(output.as_deref())
                    .unwrap_or("(no content)");
                
                format!(
                    "## {} Result {}\n\n{}",
                    label_part, status_emoji, content
                )
            }
            Self::Invocation { content, from, success, error } => {
                let status = if *success { "✅ Success" } else { "❌ Failed" };
                let display_content = error.as_deref().unwrap_or(content);
                
                format!(
                    "## Message from {}\n\n**Status:** {}\n\n{}",
                    from, status, display_content
                )
            }
            Self::Generic { data } => {
                format!(
                    "## {} Result\n\n```json\n{}\n```",
                    tool_name,
                    serde_json::to_string_pretty(data).unwrap_or_default()
                )
            }
        }
    }
    
    /// Get a short summary of the result for logging/metrics
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Process { exit_code, .. } => format!("process: exit_code={}", exit_code),
            Self::Subagent { output, error, .. } => {
                if error.is_some() {
                    "subagent: failed".to_string()
                } else {
                    format!("subagent: {} chars output", output.as_ref().map(|s| s.len()).unwrap_or(0))
                }
            }
            Self::Invocation { success, .. } => format!("invocation: success={}", success),
            Self::Generic { .. } => "generic: completed".to_string(),
        }
    }
}

/// Result delivery modes (inspired by `OpenClaw`)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum AsyncResultDeliveryMode {
    /// Queue result and deliver when agent is idle (default)
    #[default]
    QueueWhenBusy,
    /// Interrupt current agent execution with result
    Interrupt,
    /// Batch multiple results together
    Collect,
    /// Try to inject into running session (advanced)
    Steer,
}

/// Configuration for async tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncToolConfig {
    /// How to deliver results to the parent agent (queue mode)
    pub delivery_mode: AsyncResultDeliveryMode,
    /// Which delivery mechanism to use (optional, defaults to executor default)
    pub delivery_target: Option<DeliveryTarget>,
    /// Maximum time to wait for task completion
    pub timeout_secs: u64,
    /// Whether to delete task record after delivery
    pub cleanup_after_delivery: bool,
    /// Label for grouping/identifying tasks
    pub label: Option<String>,
}

impl Default for AsyncToolConfig {
    fn default() -> Self {
        Self {
            delivery_mode: AsyncResultDeliveryMode::QueueWhenBusy,
            delivery_target: None,
            timeout_secs: 300,
            cleanup_after_delivery: true,
            label: None,
        }
    }
}

/// Result of waiting for an async task to complete
#[derive(Debug, Clone)]
pub enum WaitResult {
    Completed { result: ToolResult },
    Failed { error: String },
    Cancelled,
    Timeout,
}

/// An async task entry stored in the registry
#[derive(Debug)]
pub struct AsyncTaskEntry {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: Value,
    pub status: AsyncTaskStatus,
    /// Unified result from the async operation (available when status is terminal)
    pub result: Option<AsyncTaskResult>,
    pub parent_session_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub config: AsyncToolConfig,
    /// The formatted result message ready for delivery (cached from result)
    pub formatted_result: Option<String>,
    /// Completion notification channel for sync waiting
    completion_tx: Option<mpsc::Sender<AsyncTaskStatus>>,
}

impl AsyncTaskEntry {
    /// Create a new async task entry
    #[must_use]
    pub fn new(
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        parent_session_key: String,
        config: AsyncToolConfig,
    ) -> Self {
        Self {
            task_id,
            tool_name,
            params,
            status: AsyncTaskStatus::Pending,
            result: None,
            parent_session_key,
            created_at: chrono::Utc::now(),
            completed_at: None,
            config,
            formatted_result: None,
            completion_tx: None,
        }
    }

    /// Set the unified result and update formatted_result cache
    pub fn set_result(&mut self, result: AsyncTaskResult) {
        self.formatted_result = Some(result.format_for_announcement(&self.tool_name));
        self.result = Some(result);
    }

    /// Set the completion notification channel
    pub fn set_completion_channel(&mut self, tx: mpsc::Sender<AsyncTaskStatus>) {
        self.completion_tx = Some(tx);
    }

    /// Clone the status for notification
    fn notify_completion(&self) {
        if let Some(ref tx) = self.completion_tx {
            // Use try_send to avoid blocking - if channel is full, skip notification
            let _ = tx.try_send(self.status.clone());
        }
    }
}

impl Clone for AsyncTaskEntry {
    fn clone(&self) -> Self {
        Self {
            task_id: self.task_id.clone(),
            tool_name: self.tool_name.clone(),
            params: self.params.clone(),
            status: self.status.clone(),
            result: self.result.clone(),
            parent_session_key: self.parent_session_key.clone(),
            created_at: self.created_at,
            completed_at: self.completed_at,
            config: self.config.clone(),
            formatted_result: self.formatted_result.clone(),
            completion_tx: None, // Channels don't clone, will need to be re-set
        }
    }
}

/// Event sent to agent when an async task completes
#[derive(Debug, Clone)]
pub struct AsyncTaskCompletionEvent {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub result_message: String,
    pub parent_session_key: String,
    pub label: Option<String>,
}

/// Registry for tracking async tasks
#[derive(Debug, Default)]
pub struct AsyncTaskRegistry {
    tasks: HashMap<AsyncTaskId, AsyncTaskEntry>,
    /// Queue of completed tasks waiting to be announced to parent sessions
    pending_announcements: HashMap<String, Vec<AsyncTaskId>>, // session_key -> task_ids
}

impl AsyncTaskRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            pending_announcements: HashMap::new(),
        }
    }

    pub fn register(&mut self, entry: AsyncTaskEntry) {
        self.tasks.insert(entry.task_id.clone(), entry);
    }

    #[must_use]
    pub fn get(&self, task_id: &AsyncTaskId) -> Option<&AsyncTaskEntry> {
        self.tasks.get(task_id)
    }

    pub fn get_mut(&mut self, task_id: &AsyncTaskId) -> Option<&mut AsyncTaskEntry> {
        self.tasks.get_mut(task_id)
    }

    pub fn update_status(&mut self, task_id: &AsyncTaskId, status: AsyncTaskStatus) {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.status = status.clone();
            if entry.status.is_terminal() {
                entry.completed_at = Some(chrono::Utc::now());
                // Notify any waiters
                entry.notify_completion();
            }
        }
    }

    /// Wait for a task to complete with a timeout
    ///
    /// This is used for sync-mode execution of async tools.
    /// Returns immediately if the task is already in a terminal state.
    pub async fn wait_for_completion(
        &self,
        task_id: &AsyncTaskId,
        timeout: Duration,
    ) -> Result<WaitResult> {
        // Fast path: check if already completed
        if let Some(entry) = self.tasks.get(task_id) {
            if entry.status.is_terminal() {
                return Ok(self.status_to_wait_result(&entry.status));
            }
        }

        // Create a channel to receive completion notification
        let (tx, _rx) = mpsc::channel(1);

        // Register the completion channel
        if let Some(entry) = self.tasks.get(task_id) {
            // Clone the entry, set the channel, and re-insert
            let mut entry_with_channel = entry.clone();
            entry_with_channel.set_completion_channel(tx);

            // We need to modify through a mutable reference
            // Since we can't easily do this, we'll poll instead
            drop(entry);
        }

        // Polling-based wait with notification support
        let start = tokio::time::Instant::now();
        loop {
            // Check if completed
            if let Some(entry) = self.tasks.get(task_id) {
                if entry.status.is_terminal() {
                    return Ok(self.status_to_wait_result(&entry.status));
                }
            } else {
                return Err(anyhow::anyhow!("Task {} not found in registry", task_id));
            }

            // Check timeout
            if start.elapsed() >= timeout {
                return Ok(WaitResult::Timeout);
            }

            // Wait for either notification or poll interval
            let remaining = timeout.saturating_sub(start.elapsed());
            let poll_interval = Duration::from_millis(50).min(remaining);

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Check if a task exists and return its current status
    #[must_use]
    pub fn check_status(&self, task_id: &AsyncTaskId) -> Option<AsyncTaskStatus> {
        self.tasks.get(task_id).map(|e| e.status.clone())
    }

    /// Convert status to wait result
    fn status_to_wait_result(&self, status: &AsyncTaskStatus) -> WaitResult {
        match status {
            AsyncTaskStatus::Completed { result } => WaitResult::Completed {
                result: result.clone(),
            },
            AsyncTaskStatus::Failed { error } => WaitResult::Failed {
                error: error.clone(),
            },
            AsyncTaskStatus::Cancelled => WaitResult::Cancelled,
            _ => WaitResult::Timeout, // Should not happen for terminal states
        }
    }

    /// Register a completion waiter for a task
    ///
    /// This is an alternative to `wait_for_completion` that uses a channel
    /// for more efficient notification.
    pub async fn register_waiter(
        &mut self,
        task_id: &AsyncTaskId,
        tx: mpsc::Sender<AsyncTaskStatus>,
    ) -> Result<()> {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.set_completion_channel(tx);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Task {} not found in registry", task_id))
        }
    }

    /// Queue a completed task for announcement to parent
    pub fn queue_announcement(&mut self, task_id: AsyncTaskId, session_key: &str) {
        self.pending_announcements
            .entry(session_key.to_string())
            .or_default()
            .push(task_id);
    }

    /// Get pending announcements for a session
    pub fn get_pending_for_session(&mut self, session_key: &str) -> Vec<AsyncTaskCompletionEvent> {
        let task_ids = self
            .pending_announcements
            .remove(session_key)
            .unwrap_or_default();

        task_ids
            .into_iter()
            .filter_map(|task_id| {
                let entry = self.tasks.get(&task_id)?;
                Some(AsyncTaskCompletionEvent {
                    task_id: task_id.clone(),
                    tool_name: entry.tool_name.clone(),
                    result_message: entry.formatted_result.clone()?,
                    parent_session_key: entry.parent_session_key.clone(),
                    label: entry.config.label.clone(),
                })
            })
            .collect()
    }

    /// Get count of pending announcements for a session
    #[must_use]
    pub fn pending_count(&self, session_key: &str) -> usize {
        self.pending_announcements
            .get(session_key)
            .map_or(0, std::vec::Vec::len)
    }

    pub fn cleanup_completed(&mut self) -> usize {
        let to_remove: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, entry)| {
                entry.status.is_terminal()
                    && entry.config.cleanup_after_delivery
                    && entry.completed_at.is_some_and(|t| {
                        chrono::Utc::now().signed_duration_since(t).num_seconds() > 300
                    })
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in &to_remove {
            self.tasks.remove(id);
        }

        to_remove.len()
    }
}

pub type SharedAsyncTaskRegistry = Arc<RwLock<AsyncTaskRegistry>>;

/// Event bus for delivering async task completions to agents
#[derive(Debug, Clone)]
pub struct AsyncTaskEventBus {
    /// Sender for events - agents subscribe to receive events
    sender: mpsc::UnboundedSender<AsyncTaskCompletionEvent>,
}

impl AsyncTaskEventBus {
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<AsyncTaskCompletionEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }

    pub fn publish(&self, event: AsyncTaskCompletionEvent) -> Result<()> {
        self.sender
            .send(event)
            .map_err(|_| anyhow::anyhow!("Failed to send async task event - no listeners"))
    }
}

/// Delivery target types for async task results
/// 
/// Defines where and how the result should be delivered
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryTarget {
    /// Deliver to session via announcement (adds message to parent session)
    SessionAnnouncement,
    /// Deliver to async result queue
    AsyncQueue,
    /// Deliver via EventSubscriber broadcast
    EventBroadcast,
    /// Deliver via direct channel (for sync waiting)
    DirectChannel,
}

impl Default for DeliveryTarget {
    fn default() -> Self {
        DeliveryTarget::AsyncQueue
    }
}

/// Trait for result delivery mechanisms
/// 
/// Implementations define how async task results are delivered to their recipients.
/// This enables pluggable delivery strategies for different use cases.
#[async_trait::async_trait]
pub trait ResultDelivery: Send + Sync {
    /// Deliver result for a completed task
    /// 
    /// # Arguments
    /// * `task` - The completed async task entry with result
    /// 
    /// # Returns
    /// Ok(()) on successful delivery, Err otherwise
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()>;
    
    /// Clone this delivery mechanism into a Box
    /// 
    /// Required because ResultDelivery is a trait object and needs
    /// to be cloneable for use in spawned tasks.
    fn clone_box(&self) -> Box<dyn ResultDelivery>;
}

impl Clone for Box<dyn ResultDelivery> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Queue-based delivery mechanism
/// 
/// Delivers results via the AsyncResultQueueManager for later retrieval.
/// Used by process tool and agent_spawn in queue mode.
#[derive(Debug, Clone)]
pub struct QueueDelivery {
    queue_manager: SharedAsyncResultQueueManager,
}

impl QueueDelivery {
    /// Create a new queue delivery mechanism
    #[must_use]
    pub fn new(queue_manager: SharedAsyncResultQueueManager) -> Self {
        Self { queue_manager }
    }
}

#[async_trait::async_trait]
impl ResultDelivery for QueueDelivery {
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()> {
        let result_message = task.formatted_result.clone()
            .or_else(|| task.result.as_ref().map(|r| r.format_for_announcement(&task.tool_name)))
            .unwrap_or_else(|| format!("Task {} completed with no result", task.task_id));
        
        let event = AsyncTaskCompletionEvent {
            task_id: task.task_id.clone(),
            tool_name: task.tool_name.clone(),
            result_message,
            parent_session_key: task.parent_session_key.clone(),
            label: task.config.label.clone(),
        };
        
        let mut manager = self.queue_manager.write().await;
        manager.enqueue(event);
        
        tracing::debug!(
            "Queued result for task {} in session {}",
            task.task_id,
            task.parent_session_key
        );
        
        Ok(())
    }
    
    fn clone_box(&self) -> Box<dyn ResultDelivery> {
        Box::new(self.clone())
    }
}

/// Direct channel delivery mechanism
/// 
/// Delivers results via an mpsc channel for immediate consumption.
/// Used for sync-wait scenarios where a task waits for completion.
#[derive(Debug)]
pub struct ChannelDelivery {
    sender: mpsc::Sender<AsyncTaskCompletionEvent>,
}

impl ChannelDelivery {
    /// Create a new channel delivery mechanism
    #[must_use]
    pub fn new(sender: mpsc::Sender<AsyncTaskCompletionEvent>) -> Self {
        Self { sender }
    }
}

#[async_trait::async_trait]
impl ResultDelivery for ChannelDelivery {
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()> {
        let result_message = task.formatted_result.clone()
            .or_else(|| task.result.as_ref().map(|r| r.format_for_announcement(&task.tool_name)))
            .unwrap_or_else(|| format!("Task {} completed", task.task_id));
        
        let event = AsyncTaskCompletionEvent {
            task_id: task.task_id.clone(),
            tool_name: task.tool_name.clone(),
            result_message,
            parent_session_key: task.parent_session_key.clone(),
            label: task.config.label.clone(),
        };
        
        self.sender
            .send(event)
            .await
            .map_err(|_| anyhow::anyhow!("Failed to send result via channel - receiver dropped"))?;
        
        Ok(())
    }
    
    fn clone_box(&self) -> Box<dyn ResultDelivery> {
        // Channel cannot be cloned in the traditional sense
        // This is a limitation - ChannelDelivery is typically used for sync waiting
        // where the sender stays in the spawned task
        panic!("ChannelDelivery cannot be cloned - use QueueDelivery for multi-consumer scenarios");
    }
}

/// Callback-based delivery mechanism
/// 
/// Delivers results via a user-provided async callback function.
/// Used for session announcements and other custom delivery mechanisms.
pub struct CallbackDelivery {
    callback: Arc<dyn Fn(&AsyncTaskEntry) -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync>,
}

impl std::fmt::Debug for CallbackDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackDelivery")
            .field("callback", &"<async fn>")
            .finish()
    }
}

impl CallbackDelivery {
    /// Create a new callback delivery mechanism
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(&AsyncTaskEntry) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let callback = Arc::new(move |entry: &AsyncTaskEntry| {
            let fut = callback(entry);
            Box::pin(fut) as futures::future::BoxFuture<'static, Result<()>>
        });
        
        Self { callback }
    }
}

// Manual Clone implementation for CallbackDelivery
impl Clone for CallbackDelivery {
    fn clone(&self) -> Self {
        Self {
            callback: Arc::clone(&self.callback),
        }
    }
}

#[async_trait::async_trait]
impl ResultDelivery for CallbackDelivery {
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()> {
        (self.callback)(task).await
    }
    
    fn clone_box(&self) -> Box<dyn ResultDelivery> {
        Box::new(self.clone())
    }
}

/// Trait for tools that support async execution
#[async_trait::async_trait]
pub trait AsyncTool: Send + Sync {
    /// Unique name of the tool
    fn name(&self) -> &str;

    /// Execute the async operation
    /// Returns a receipt immediately, spawns background work
    async fn spawn_async(
        &self,
        params: Value,
        parent_session_key: &str,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt>;

    /// Check status of a running async task
    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus>;

    /// Cancel a running async task
    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool>;

    /// Format the result for delivery to parent agent
    fn format_result(&self, entry: &AsyncTaskEntry) -> String;
}

/// Unified executor for all async tool operations
/// 
/// This provides a single entry point for executing async tasks with:
/// - Task registration and tracking
/// - Pluggable delivery mechanisms
/// - Automatic status updates
/// - Result formatting and caching
#[derive(Clone)]
pub struct UnifiedAsyncExecutor {
    /// Task registry for tracking all async operations
    registry: SharedAsyncTaskRegistry,
    /// Queue manager for queue-based delivery
    queue_manager: SharedAsyncResultQueueManager,
    /// Registered delivery mechanisms by target type
    deliveries: Arc<RwLock<HashMap<DeliveryTarget, Box<dyn ResultDelivery>>>>,
    /// Default delivery target
    default_delivery: DeliveryTarget,
}

impl UnifiedAsyncExecutor {
    /// Create a new unified async executor
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(AsyncTaskRegistry::new())),
            queue_manager: Arc::new(RwLock::new(AsyncResultQueueManager::new())),
            deliveries: Arc::new(RwLock::new(HashMap::new())),
            default_delivery: DeliveryTarget::AsyncQueue,
        }
    }

    /// Create with existing registries (for sharing with other components)
    #[must_use]
    pub fn with_registries(
        registry: SharedAsyncTaskRegistry,
        queue_manager: SharedAsyncResultQueueManager,
    ) -> Self {
        Self {
            registry,
            queue_manager,
            deliveries: Arc::new(RwLock::new(HashMap::new())),
            default_delivery: DeliveryTarget::AsyncQueue,
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
    pub fn with_default_delivery(mut self, target: DeliveryTarget) -> Self {
        self.default_delivery = target;
        self
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

    /// Execute an async task with the unified executor
    /// 
    /// # Arguments
    /// * `task_id` - Unique identifier for this task
    /// * `tool_name` - Name of the tool executing the task
    /// * `params` - Parameters for the task
    /// * `parent_session_key` - Session key for result routing
    /// * `config` - Async tool configuration
    /// * `execution_fn` - The actual async work to execute
    /// 
    /// # Returns
    /// Receipt with task_id and initial status
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
        Fut: std::future::Future<Output = Result<AsyncTaskResult>> + Send + 'static,
    {
        let tool_name = tool_name.into();
        let parent_session_key = parent_session_key.into();

        // Create task entry
        let entry = AsyncTaskEntry::new(
            task_id.clone(),
            tool_name.clone(),
            params,
            parent_session_key.clone(),
            config.clone(),
        );

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

        // Spawn the background execution
        tokio::spawn(async move {
            // Update status to running
            {
                let mut registry = registry_clone.write().await;
                registry.update_status(&task_id_clone, AsyncTaskStatus::Running);
            }

            // Execute the work
            let result = execution_fn().await;

            // Update registry with result
            let status = match &result {
                Ok(_) => AsyncTaskStatus::Completed {
                    result: ToolResult::success(serde_json::json!({"completed": true})),
                },
                Err(e) => AsyncTaskStatus::Failed {
                    error: e.to_string(),
                },
            };

            {
                let mut registry = registry_clone.write().await;
                registry.update_status(&task_id_clone, status);
                
                // Store the unified result
                if let Ok(async_result) = result {
                    if let Some(entry) = registry.get_mut(&task_id_clone) {
                        entry.set_result(async_result);
                    }
                }
            }

            // Deliver the result
            if let Some(entry) = registry_clone.read().await.get(&task_id_clone) {
                if let Err(e) = delivery.deliver(entry).await {
                    tracing::error!(
                        "Failed to deliver result for task {}: {}",
                        task_id_clone,
                        e
                    );
                }
            }
        });

        // Return receipt immediately
        Ok(AsyncTaskReceipt {
            task_id: task_id.clone(),
            status: AsyncTaskStatus::Pending,
            estimated_duration_secs: None,
            check_status_tool: "async_task_status".to_string(),
        })
    }

    /// Wait for a task to complete (sync mode)
    /// 
    /// This is a convenience method for tools that need to wait for completion.
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
        // Note: Actual cancellation would need the task handle
        // For now, just mark as cancelled in registry
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
}

impl std::fmt::Debug for UnifiedAsyncExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnifiedAsyncExecutor")
            .field("registry", &"<AsyncTaskRegistry>")
            .field("queue_manager", &"<AsyncResultQueueManager>")
            .field("deliveries", &"<HashMap<DeliveryTarget, Box<dyn ResultDelivery>>>")
            .field("default_delivery", &self.default_delivery)
            .finish()
    }
}

impl Default for UnifiedAsyncExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Queue for managing async result delivery (inspired by `OpenClaw`)
#[derive(Debug)]
pub struct AsyncResultQueue {
    session_key: String,
    mode: AsyncResultDeliveryMode,
    items: Vec<AsyncTaskCompletionEvent>,
    /// Whether the parent session is currently executing
    is_parent_busy: bool,
}

impl AsyncResultQueue {
    #[must_use]
    pub fn new(session_key: String, mode: AsyncResultDeliveryMode) -> Self {
        Self {
            session_key,
            mode,
            items: Vec::new(),
            is_parent_busy: false,
        }
    }

    pub fn enqueue(&mut self, event: AsyncTaskCompletionEvent) {
        self.items.push(event);
    }

    /// Process queue based on delivery mode
    pub fn process(&mut self) -> Vec<AsyncTaskCompletionEvent> {
        if self.items.is_empty() {
            return Vec::new();
        }

        match self.mode {
            AsyncResultDeliveryMode::QueueWhenBusy => {
                if self.is_parent_busy {
                    // Keep items queued
                    Vec::new()
                } else {
                    // Deliver all items
                    std::mem::take(&mut self.items)
                }
            }
            AsyncResultDeliveryMode::Collect => {
                // Collect mode: batch all items into a single composite event
                if self.items.len() >= 2 {
                    let batched = self.create_batch_event();
                    self.items.clear();
                    if let Some(event) = batched {
                        return vec![event];
                    }
                }
                std::mem::take(&mut self.items)
            }
            AsyncResultDeliveryMode::Interrupt | AsyncResultDeliveryMode::Steer => {
                // Deliver immediately
                std::mem::take(&mut self.items)
            }
        }
    }

    fn create_batch_event(&self) -> Option<AsyncTaskCompletionEvent> {
        if self.items.is_empty() {
            return None;
        }

        let first = self.items.first()?;
        let combined_results: Vec<String> = self
            .items
            .iter()
            .map(|e| format!("## {} ({}):\n{}", e.tool_name, e.task_id, e.result_message))
            .collect();

        Some(AsyncTaskCompletionEvent {
            task_id: "batched".to_string(),
            tool_name: "async_batch".to_string(),
            result_message: format!(
                "[Multiple async tasks completed]\n\n{}",
                combined_results.join("\n\n")
            ),
            parent_session_key: first.parent_session_key.clone(),
            label: Some("batched".to_string()),
        })
    }

    pub fn set_parent_busy(&mut self, busy: bool) {
        self.is_parent_busy = busy;
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Manager for async result queues per session
#[derive(Debug, Default)]
pub struct AsyncResultQueueManager {
    queues: HashMap<String, AsyncResultQueue>,
}

impl AsyncResultQueueManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: HashMap::new(),
        }
    }

    pub fn get_or_create(
        &mut self,
        session_key: &str,
        mode: AsyncResultDeliveryMode,
    ) -> &mut AsyncResultQueue {
        self.queues
            .entry(session_key.to_string())
            .or_insert_with(|| AsyncResultQueue::new(session_key.to_string(), mode))
    }

    pub fn set_parent_busy(&mut self, session_key: &str, busy: bool) {
        if let Some(queue) = self.queues.get_mut(session_key) {
            queue.set_parent_busy(busy);
        }
    }

    pub fn enqueue(&mut self, event: AsyncTaskCompletionEvent) {
        let session_key = event.parent_session_key.clone();
        let queue = self.queues.entry(session_key.clone()).or_insert_with(|| {
            AsyncResultQueue::new(session_key, AsyncResultDeliveryMode::default())
        });
        queue.enqueue(event);
    }

    pub fn process_queue(&mut self, session_key: &str) -> Vec<AsyncTaskCompletionEvent> {
        self.queues
            .get_mut(session_key)
            .map(AsyncResultQueue::process)
            .unwrap_or_default()
    }

    pub fn cleanup_empty_queues(&mut self) {
        self.queues.retain(|_, q| !q.is_empty());
    }
}

pub type SharedAsyncResultQueueManager = Arc<RwLock<AsyncResultQueueManager>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_async_task_status_terminal() {
        assert!(!AsyncTaskStatus::Pending.is_terminal());
        assert!(!AsyncTaskStatus::Running.is_terminal());
        assert!(AsyncTaskStatus::Completed {
            result: ToolResult::success(serde_json::json!({"ok": true}))
        }
        .is_terminal());
        assert!(AsyncTaskStatus::Failed {
            error: "test".to_string()
        }
        .is_terminal());
        assert!(AsyncTaskStatus::Cancelled.is_terminal());
    }

    #[tokio::test]
    async fn test_async_task_registry() {
        let mut registry = AsyncTaskRegistry::new();
        let entry = AsyncTaskEntry::new(
            "task_123".to_string(),
            "test_tool".to_string(),
            serde_json::json!({}),
            "session:abc".to_string(),
            AsyncToolConfig::default(),
        );

        registry.register(entry);
        assert!(registry.get(&"task_123".to_string()).is_some());

        registry.update_status(&"task_123".to_string(), AsyncTaskStatus::Running);
        assert_eq!(
            registry.get(&"task_123".to_string()).unwrap().status,
            AsyncTaskStatus::Running
        );
    }

    #[tokio::test]
    async fn test_wait_for_completion() {
        let mut registry = AsyncTaskRegistry::new();
        let task_id = "task_wait".to_string();

        let entry = AsyncTaskEntry::new(
            task_id.clone(),
            "test_tool".to_string(),
            serde_json::json!({}),
            "session:abc".to_string(),
            AsyncToolConfig::default(),
        );

        registry.register(entry);

        // Test timeout when task doesn't complete
        let timeout_result = tokio::time::timeout(
            Duration::from_millis(100),
            registry.wait_for_completion(&task_id, Duration::from_millis(50)),
        )
        .await;

        // Should timeout or return WaitResult::Timeout
        match timeout_result {
            Ok(Ok(WaitResult::Timeout)) => {}
            Err(_) => {} // tokio timeout also acceptable
            other => panic!("Expected timeout, got: {:?}", other),
        }

        // Now complete the task
        registry.update_status(
            &task_id,
            AsyncTaskStatus::Completed {
                result: ToolResult::success(serde_json::json!({"done": true})),
            },
        );

        // Now wait should return immediately with completed result
        let result = registry
            .wait_for_completion(&task_id, Duration::from_secs(1))
            .await
            .unwrap();
        match result {
            WaitResult::Completed { result } => {
                assert!(result.success);
            }
            _ => panic!("Expected completed result, got: {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_wait_for_failed_task() {
        let mut registry = AsyncTaskRegistry::new();
        let task_id = "task_fail".to_string();

        let entry = AsyncTaskEntry::new(
            task_id.clone(),
            "test_tool".to_string(),
            serde_json::json!({}),
            "session:abc".to_string(),
            AsyncToolConfig::default(),
        );

        registry.register(entry);

        // Mark as failed
        registry.update_status(
            &task_id,
            AsyncTaskStatus::Failed {
                error: "Something went wrong".to_string(),
            },
        );

        let result = registry
            .wait_for_completion(&task_id, Duration::from_secs(1))
            .await
            .unwrap();
        match result {
            WaitResult::Failed { error } => {
                assert_eq!(error, "Something went wrong");
            }
            _ => panic!("Expected failed result, got: {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_check_status() {
        let mut registry = AsyncTaskRegistry::new();
        let task_id = "task_check".to_string();

        assert!(registry.check_status(&task_id).is_none());

        let entry = AsyncTaskEntry::new(
            task_id.clone(),
            "test_tool".to_string(),
            serde_json::json!({}),
            "session:abc".to_string(),
            AsyncToolConfig::default(),
        );

        registry.register(entry);
        assert_eq!(
            registry.check_status(&task_id),
            Some(AsyncTaskStatus::Pending)
        );

        registry.update_status(&task_id, AsyncTaskStatus::Running);
        assert_eq!(
            registry.check_status(&task_id),
            Some(AsyncTaskStatus::Running)
        );
    }

    #[test]
    fn test_async_result_queue() {
        let mut queue = AsyncResultQueue::new(
            "session:abc".to_string(),
            AsyncResultDeliveryMode::QueueWhenBusy,
        );

        let event = AsyncTaskCompletionEvent {
            task_id: "task_1".to_string(),
            tool_name: "subagent_spawn".to_string(),
            result_message: "Task completed".to_string(),
            parent_session_key: "session:abc".to_string(),
            label: None,
        };

        // When parent is busy, items stay queued
        queue.set_parent_busy(true);
        queue.enqueue(event.clone());
        assert_eq!(queue.process().len(), 0);
        assert_eq!(queue.len(), 1);

        // When parent is idle, items are delivered
        queue.set_parent_busy(false);
        let delivered = queue.process();
        assert_eq!(delivered.len(), 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_collect_mode_batching() {
        let mut queue =
            AsyncResultQueue::new("session:abc".to_string(), AsyncResultDeliveryMode::Collect);

        queue.enqueue(AsyncTaskCompletionEvent {
            task_id: "task_1".to_string(),
            tool_name: "subagent_spawn".to_string(),
            result_message: "Result 1".to_string(),
            parent_session_key: "session:abc".to_string(),
            label: None,
        });

        queue.enqueue(AsyncTaskCompletionEvent {
            task_id: "task_2".to_string(),
            tool_name: "subagent_spawn".to_string(),
            result_message: "Result 2".to_string(),
            parent_session_key: "session:abc".to_string(),
            label: None,
        });

        let delivered = queue.process();
        assert_eq!(delivered.len(), 1); // Batched into one
        assert!(delivered[0].result_message.contains("Multiple async tasks"));
    }

    // Tests for AsyncTaskResult (Phase 1 of unified infrastructure)

    #[test]
    fn test_async_task_result_process() {
        let result = AsyncTaskResult::Process {
            stdout: "Hello World".to_string(),
            stderr: "".to_string(),
            exit_code: 0,
        };

        let formatted = result.format_for_announcement("echo");
        assert!(formatted.contains("Process Result"));
        assert!(formatted.contains("Hello World"));
        assert!(formatted.contains("Exit Code:** 0"));
        
        assert_eq!(result.summary(), "process: exit_code=0");
    }

    #[test]
    fn test_async_task_result_subagent() {
        let result = AsyncTaskResult::Subagent {
            output: Some("Analysis complete".to_string()),
            error: None,
            token_usage: Some((100, 200, 300)),
        };

        let formatted = result.format_for_announcement("analyzer");
        assert!(formatted.contains("Subagent [analyzer]"));
        assert!(formatted.contains("✅"));
        assert!(formatted.contains("Analysis complete"));
        
        assert_eq!(result.summary(), "subagent: 17 chars output");
    }

    #[test]
    fn test_async_task_result_subagent_error() {
        let result = AsyncTaskResult::Subagent {
            output: None,
            error: Some("Task failed".to_string()),
            token_usage: None,
        };

        let formatted = result.format_for_announcement("agent_spawn");
        assert!(formatted.contains("Subagent"));
        assert!(formatted.contains("❌"));
        assert!(formatted.contains("Task failed"));
        
        assert_eq!(result.summary(), "subagent: failed");
    }

    #[test]
    fn test_async_task_result_invocation() {
        let result = AsyncTaskResult::Invocation {
            content: "Here is the research result".to_string(),
            from: "researcher_agent".to_string(),
            success: true,
            error: None,
        };

        let formatted = result.format_for_announcement("agent_invoke");
        assert!(formatted.contains("Message from researcher_agent"));
        assert!(formatted.contains("✅ Success"));
        assert!(formatted.contains("Here is the research result"));
        
        assert_eq!(result.summary(), "invocation: success=true");
    }

    #[test]
    fn test_async_task_result_invocation_failed() {
        let result = AsyncTaskResult::Invocation {
            content: "".to_string(),
            from: "helper_agent".to_string(),
            success: false,
            error: Some("Agent not available".to_string()),
        };

        let formatted = result.format_for_announcement("agent_invoke");
        assert!(formatted.contains("❌ Failed"));
        assert!(formatted.contains("Agent not available"));
        
        assert_eq!(result.summary(), "invocation: success=false");
    }

    #[test]
    fn test_async_task_result_generic() {
        let data = serde_json::json!({"key": "value", "count": 42});
        let result = AsyncTaskResult::Generic { data };

        let formatted = result.format_for_announcement("custom_tool");
        assert!(formatted.contains("custom_tool Result"));
        assert!(formatted.contains("key"));
        assert!(formatted.contains("value"));
        
        assert_eq!(result.summary(), "generic: completed");
    }

    #[test]
    fn test_async_task_entry_set_result() {
        let mut entry = AsyncTaskEntry::new(
            "task_123".to_string(),
            "test_process".to_string(),
            serde_json::json!({"command": "echo"}),
            "session:abc".to_string(),
            AsyncToolConfig::default(),
        );

        assert!(entry.result.is_none());
        assert!(entry.formatted_result.is_none());

        let result = AsyncTaskResult::Process {
            stdout: "output".to_string(),
            stderr: "".to_string(),
            exit_code: 0,
        };

        entry.set_result(result);

        assert!(entry.result.is_some());
        assert!(entry.formatted_result.is_some());
        assert!(entry.formatted_result.as_ref().unwrap().contains("Process Result"));
    }

    // Tests for Delivery Mechanisms (Phase 2)

    #[tokio::test]
    async fn test_queue_delivery() {
        let queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let delivery = QueueDelivery::new(queue_manager.clone());

        let mut entry = AsyncTaskEntry::new(
            "task_123".to_string(),
            "test_tool".to_string(),
            serde_json::json!({}),
            "session:abc".to_string(),
            AsyncToolConfig::default(),
        );

        entry.set_result(AsyncTaskResult::Process {
            stdout: "Hello".to_string(),
            stderr: "".to_string(),
            exit_code: 0,
        });

        // Deliver the result
        delivery.deliver(&entry).await.unwrap();

        // Verify it was queued
        let mut manager = queue_manager.write().await;
        let events = manager.process_queue("session:abc");
        
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].task_id, "task_123");
        assert!(events[0].result_message.contains("Hello"));
    }

    #[tokio::test]
    async fn test_channel_delivery() {
        let (tx, mut rx) = mpsc::channel(10);
        let delivery = ChannelDelivery::new(tx);

        let mut entry = AsyncTaskEntry::new(
            "task_456".to_string(),
            "test_tool".to_string(),
            serde_json::json!({}),
            "session:xyz".to_string(),
            AsyncToolConfig::default(),
        );

        entry.set_result(AsyncTaskResult::Generic {
            data: serde_json::json!({"status": "done"}),
        });

        // Deliver the result
        delivery.deliver(&entry).await.unwrap();

        // Verify it was received
        let event = rx.recv().await.unwrap();
        assert_eq!(event.task_id, "task_456");
        assert_eq!(event.parent_session_key, "session:xyz");
    }

    #[tokio::test]
    async fn test_callback_delivery() {
        let delivered = Arc::new(RwLock::new(false));
        let delivered_clone = delivered.clone();

        let delivery = CallbackDelivery::new(move |_entry: &AsyncTaskEntry| {
            let flag = delivered_clone.clone();
            async move {
                *flag.write().await = true;
                Ok(())
            }
        });

        let entry = AsyncTaskEntry::new(
            "task_789".to_string(),
            "test_tool".to_string(),
            serde_json::json!({}),
            "session:def".to_string(),
            AsyncToolConfig::default(),
        );

        // Deliver the result
        delivery.deliver(&entry).await.unwrap();

        // Verify callback was invoked
        assert!(*delivered.read().await);
    }

    #[tokio::test]
    async fn test_delivery_target_serialization() {
        // Test serialization/deserialization
        let targets = vec![
            DeliveryTarget::SessionAnnouncement,
            DeliveryTarget::AsyncQueue,
            DeliveryTarget::EventBroadcast,
            DeliveryTarget::DirectChannel,
        ];

        for target in targets {
            let json = serde_json::to_string(&target).unwrap();
            let deserialized: DeliveryTarget = serde_json::from_str(&json).unwrap();
            assert_eq!(target, deserialized);
        }
    }

    #[test]
    fn test_delivery_target_default() {
        let target: DeliveryTarget = Default::default();
        assert_eq!(target, DeliveryTarget::AsyncQueue);
    }

    // Tests for UnifiedAsyncExecutor (Phase 3)

    #[tokio::test]
    async fn test_unified_executor_creation() {
        let executor = UnifiedAsyncExecutor::new();
        
        // Should be able to access registry and queue manager
        let _registry = executor.registry();
        let _queue_manager = executor.queue_manager();
        
        // Debug should work
        let debug_str = format!("{:?}", executor);
        assert!(debug_str.contains("UnifiedAsyncExecutor"));
    }

    #[tokio::test]
    async fn test_unified_executor_default_delivery() {
        let executor = UnifiedAsyncExecutor::new()
            .with_default_delivery(DeliveryTarget::SessionAnnouncement);
        
        let debug_str = format!("{:?}", executor);
        assert!(debug_str.contains("SessionAnnouncement"));
    }

    #[tokio::test]
    async fn test_unified_executor_execute() {
        let executor = UnifiedAsyncExecutor::new();
        let task_id = "test_task_001".to_string();

        // Execute a simple async task
        let receipt = executor
            .execute(
                task_id.clone(),
                "test_tool",
                serde_json::json!({"param": "value"}),
                "session:test",
                AsyncToolConfig::default(),
                || async {
                    // Simulate some work
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok(AsyncTaskResult::Generic {
                        data: serde_json::json!({"result": "success"}),
                    })
                },
            )
            .await
            .unwrap();

        assert_eq!(receipt.task_id, task_id);
        assert!(matches!(receipt.status, AsyncTaskStatus::Pending));

        // Wait a bit for the task to complete
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check that the task completed
        let status = executor.check_status(&task_id).await;
        assert!(status.is_some());
        assert!(status.unwrap().is_terminal());
    }

    #[tokio::test]
    async fn test_unified_executor_check_status() {
        let executor = UnifiedAsyncExecutor::new();
        let task_id = "status_check_task".to_string();

        // Check non-existent task
        assert!(executor.check_status(&task_id).await.is_none());

        // Execute a task
        executor
            .execute(
                task_id.clone(),
                "test_tool",
                serde_json::json!({}),
                "session:test",
                AsyncToolConfig::default(),
                || async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(AsyncTaskResult::Generic {
                        data: serde_json::json!({}),
                    })
                },
            )
            .await
            .unwrap();

        // Should be pending or running immediately after spawn
        let status = executor.check_status(&task_id).await;
        assert!(status.is_some());
    }

    #[tokio::test]
    async fn test_unified_executor_cancel() {
        let executor = UnifiedAsyncExecutor::new();
        let task_id = "cancel_task".to_string();

        // Execute a long-running task
        executor
            .execute(
                task_id.clone(),
                "test_tool",
                serde_json::json!({}),
                "session:test",
                AsyncToolConfig::default(),
                || async {
                    // Long running task
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok(AsyncTaskResult::Generic {
                        data: serde_json::json!({}),
                    })
                },
            )
            .await
            .unwrap();

        // Cancel it
        let cancelled = executor.cancel(&task_id).await.unwrap();
        assert!(cancelled);

        // Check it's now cancelled
        let status = executor.check_status(&task_id).await;
        assert!(matches!(status, Some(AsyncTaskStatus::Cancelled)));

        // Cancel again should return false (already terminal)
        let cancelled_again = executor.cancel(&task_id).await.unwrap();
        assert!(!cancelled_again);
    }

    #[tokio::test]
    async fn test_unified_executor_queue_delivery() {
        let executor = UnifiedAsyncExecutor::new();
        let task_id = "queue_delivery_task".to_string();

        // Execute a task
        executor
            .execute(
                task_id.clone(),
                "process",
                serde_json::json!({"command": "echo hello"}),
                "session:abc",
                AsyncToolConfig {
                    delivery_target: Some(DeliveryTarget::AsyncQueue),
                    ..Default::default()
                },
                || async {
                    Ok(AsyncTaskResult::Process {
                        stdout: "hello".to_string(),
                        stderr: "".to_string(),
                        exit_code: 0,
                    })
                },
            )
            .await
            .unwrap();

        // Wait for completion
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check queue for result
        let mut manager = executor.queue_manager().write().await;
        let events = manager.process_queue("session:abc");
        
        assert_eq!(events.len(), 1);
        assert!(events[0].result_message.contains("hello"));
    }

    #[tokio::test]
    async fn test_unified_executor_custom_callback_delivery() {
        let executor = UnifiedAsyncExecutor::new();
        let delivered = Arc::new(RwLock::new(false));
        let delivered_clone = delivered.clone();

        // Register custom callback delivery
        let callback_delivery = CallbackDelivery::new(move |_entry: &AsyncTaskEntry| {
            let flag = delivered_clone.clone();
            async move {
                *flag.write().await = true;
                Ok(())
            }
        });

        executor
            .register_delivery(
                DeliveryTarget::SessionAnnouncement,
                Box::new(callback_delivery),
            )
            .await;

        let task_id = "callback_task".to_string();

        // Execute with custom delivery
        executor
            .execute(
                task_id.clone(),
                "test_tool",
                serde_json::json!({}),
                "session:test",
                AsyncToolConfig {
                    delivery_target: Some(DeliveryTarget::SessionAnnouncement),
                    ..Default::default()
                },
                || async {
                    Ok(AsyncTaskResult::Generic {
                        data: serde_json::json!({"done": true}),
                    })
                },
            )
            .await
            .unwrap();

        // Wait for completion
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify callback was invoked
        assert!(*delivered.read().await);
    }

    #[tokio::test]
    async fn test_unified_executor_default() {
        let executor: UnifiedAsyncExecutor = Default::default();
        
        // Should work the same as new()
        let task_id = "default_exec_task".to_string();
        let receipt = executor
            .execute(
                task_id.clone(),
                "test_tool",
                serde_json::json!({}),
                "session:test",
                AsyncToolConfig::default(),
                || async {
                    Ok(AsyncTaskResult::Generic {
                        data: serde_json::json!({}),
                    })
                },
            )
            .await
            .unwrap();

        assert_eq!(receipt.task_id, task_id);
    }
}
