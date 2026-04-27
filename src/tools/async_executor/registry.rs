//! Registry for tracking async tasks

use super::delivery::FormatterRegistry;
use super::event_bus::AsyncTaskCompletionEvent;
use super::types::{AsyncTaskId, AsyncTaskStatus, AsyncToolConfig, WaitResult};
use crate::common::registry::SimpleRegistry;
use crate::tools::traits::ToolResult;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// An async task entry stored in the registry
#[derive(Debug)]
pub struct AsyncTaskEntry {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: Value,
    pub status: AsyncTaskStatus,
    /// Opaque result from the async operation (available when status is terminal)
    pub result: Option<Value>,
    pub parent_session_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub config: AsyncToolConfig,
    /// The formatted result message ready for delivery (cached from result)
    pub formatted_result: Option<String>,
    /// Completion notification channel for sync waiting
    completion_tx: Option<mpsc::Sender<AsyncTaskStatus>>,
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
            completion_tx: None,
        }
    }
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

    /// Set the result and update `formatted_result` cache
    pub fn set_result(&mut self, result: Value) {
        self.formatted_result = Some(self.format_result(&result));
        self.result = Some(result);
    }

    /// Format a result value using the formatter registry
    pub fn format_result(&self, result: &Value) -> String {
        // Use a thread-local or static formatter registry.
        // For now, default to a simple JSON formatter.
        format!(
            "## {} Result\n\n```json\n{}\n```",
            self.tool_name,
            serde_json::to_string_pretty(result).unwrap_or_default()
        )
    }

    /// Set the completion notification channel
    pub fn set_completion_channel(&mut self, tx: mpsc::Sender<AsyncTaskStatus>) {
        self.completion_tx = Some(tx);
    }

    /// Clone the status for notification
    pub fn notify_completion(&self) {
        if let Some(ref tx) = self.completion_tx {
            // Use try_send to avoid blocking - if channel is full, skip notification
            let _ = tx.try_send(self.status.clone());
        }
    }
}

/// Registry for tracking async tasks.
///
/// Wraps a [`SimpleRegistry`] for task storage while keeping the
/// `pending_announcements` queue as a separate field.
#[derive(Debug, Default)]
pub struct AsyncTaskRegistry {
    tasks: SimpleRegistry<AsyncTaskId, AsyncTaskEntry>,
    /// Queue of completed tasks waiting to be announced to parent sessions
    pending_announcements: std::collections::HashMap<String, Vec<AsyncTaskId>>, // session_key -> task_ids
}

impl AsyncTaskRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: SimpleRegistry::new(),
            pending_announcements: std::collections::HashMap::new(),
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
    pub async fn wait_for_completion(
        &self,
        task_id: &AsyncTaskId,
        timeout: Duration,
    ) -> anyhow::Result<WaitResult> {
        // Fast path: check if already completed
        if let Some(entry) = self.tasks.get(task_id) {
            if entry.status.is_terminal() {
                return Ok(self.status_to_wait_result(&entry.status));
            }
        }

        // Polling-based wait
        let start = tokio::time::Instant::now();
        loop {
            // Check if completed
            if let Some(entry) = self.tasks.get(task_id) {
                if entry.status.is_terminal() {
                    return Ok(self.status_to_wait_result(&entry.status));
                }
            } else {
                return Err(anyhow::anyhow!("Task {task_id} not found in registry"));
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

    /// Check if any tasks are still non-terminal
    #[must_use]
    pub fn has_pending_tasks(&self) -> bool {
        self.tasks.values().any(|e| !e.status.is_terminal())
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
    pub async fn register_waiter(
        &mut self,
        task_id: &AsyncTaskId,
        tx: mpsc::Sender<AsyncTaskStatus>,
    ) -> anyhow::Result<()> {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.set_completion_channel(tx);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Task {task_id} not found in registry"))
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

    /// List all tasks, optionally filtered by session_key
    #[must_use]
    pub fn list_tasks(&self, session_key: Option<&str>) -> Vec<AsyncTaskEntry> {
        self.tasks
            .values()
            .filter(|entry| session_key.map_or(true, |sk| entry.parent_session_key == sk))
            .map(|entry| entry.clone())
            .collect()
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

pub type SharedAsyncTaskRegistry = Arc<tokio::sync::RwLock<AsyncTaskRegistry>>;
