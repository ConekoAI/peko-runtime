//! Async Tool Framework - Generalized async tool execution with result queuing
//!
//! This module provides infrastructure for tools that execute asynchronously:
//! - Tool returns a receipt immediately (non-blocking)
//! - Background task executes the actual work
//! - Result is queued and delivered when the agent is ready
//!
//! Design inspired by OpenClaw's subagent announcement queue, but generalized
//! for any async tool operation.

use crate::tools::traits::ToolResult;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

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

/// Result delivery modes (inspired by OpenClaw)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AsyncResultDeliveryMode {
    /// Queue result and deliver when agent is idle (default)
    QueueWhenBusy,
    /// Interrupt current agent execution with result
    Interrupt,
    /// Batch multiple results together
    Collect,
    /// Try to inject into running session (advanced)
    Steer,
}

impl Default for AsyncResultDeliveryMode {
    fn default() -> Self {
        AsyncResultDeliveryMode::QueueWhenBusy
    }
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

/// An async task entry stored in the registry
#[derive(Debug, Clone)]
pub struct AsyncTaskEntry {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: Value,
    pub status: AsyncTaskStatus,
    pub parent_session_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub config: AsyncToolConfig,
    /// The formatted result message ready for delivery
    pub formatted_result: Option<String>,
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
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            pending_announcements: HashMap::new(),
        }
    }

    pub fn register(&mut self, entry: AsyncTaskEntry) {
        self.tasks.insert(entry.task_id.clone(), entry);
    }

    pub fn get(&self, task_id: &AsyncTaskId) -> Option<&AsyncTaskEntry> {
        self.tasks.get(task_id)
    }

    pub fn get_mut(&mut self, task_id: &AsyncTaskId) -> Option<&mut AsyncTaskEntry> {
        self.tasks.get_mut(task_id)
    }

    pub fn update_status(&mut self, task_id: &AsyncTaskId, status: AsyncTaskStatus) {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.status = status;
            if entry.status.is_terminal() {
                entry.completed_at = Some(chrono::Utc::now());
            }
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
    pub fn pending_count(&self, session_key: &str) -> usize {
        self.pending_announcements
            .get(session_key)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    pub fn cleanup_completed(&mut self) -> usize {
        let to_remove: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, entry)| {
                entry.status.is_terminal()
                    && entry.config.cleanup_after_delivery
                    && entry.completed_at.map_or(false, |t| {
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

/// Queue for managing async result delivery (inspired by OpenClaw)
#[derive(Debug)]
pub struct AsyncResultQueue {
    session_key: String,
    mode: AsyncResultDeliveryMode,
    items: Vec<AsyncTaskCompletionEvent>,
    /// Whether the parent session is currently executing
    is_parent_busy: bool,
}

impl AsyncResultQueue {
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

    pub fn len(&self) -> usize {
        self.items.len()
    }

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
            .map(|q| q.process())
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
        let entry = AsyncTaskEntry {
            task_id: "task_123".to_string(),
            tool_name: "test_tool".to_string(),
            params: serde_json::json!({}),
            status: AsyncTaskStatus::Pending,
            parent_session_key: "session:abc".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            config: AsyncToolConfig::default(),
            formatted_result: None,
        };

        registry.register(entry.clone());
        assert!(registry.get(&"task_123".to_string()).is_some());

        registry.update_status(&"task_123".to_string(), AsyncTaskStatus::Running);
        assert_eq!(
            registry.get(&"task_123".to_string()).unwrap().status,
            AsyncTaskStatus::Running
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
}
