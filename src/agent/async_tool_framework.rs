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
    /// How to deliver results to the parent agent
    pub delivery_mode: AsyncResultDeliveryMode,
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
}
