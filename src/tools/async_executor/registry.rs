//! Registry for tracking async tasks

use super::event_bus::AsyncTaskCompletionEvent;
use super::types::{AsyncTaskId, AsyncTaskStatus, AsyncToolConfig, WaitResult};
use crate::common::registry::SimpleRegistry;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

// ================================================================================
// Domain metadata extensions
// ================================================================================

/// Domain-specific metadata extensions for async task entries.
///
/// This enum keeps the generic `AsyncTaskEntry` clean while allowing
/// domain modules (subagents, shell commands, etc.) to attach their
/// own structured data. The registry ignores this field — it is
/// owned by the domain module that creates the task.
#[derive(Debug, Clone, Default)]
pub enum TaskMetadata {
    /// No additional metadata (generic async tool)
    #[default]
    None,
    /// Subagent-specific metadata
    Subagent(SubagentMetadata),
    // Future variants: ShellCommand, FileWatcher, etc.
}

/// Subagent-specific metadata attached to an `AsyncTaskEntry`.
///
/// This replaces the fields from the deleted `SubagentRun` struct
/// that were not already present in `AsyncTaskEntry`.
#[derive(Debug, Clone)]
pub struct SubagentMetadata {
    pub child_session_key: String,
    pub cleanup: crate::session::types::SpawnCleanupPolicy,
    pub depth: u32,
    pub announce_completion: bool,
    /// The subagent result (output, error, token_usage) —
    /// distinct from the generic `AsyncTaskEntry.result` which is
    /// the raw JSON returned by the execution closure.
    pub subagent_result: Option<SubagentResult>,
}

/// Result of a subagent run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentResult {
    /// Final status
    pub status: AsyncTaskStatus,
    /// Output content (if successful)
    pub output: Option<String>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Token usage (input, output, total)
    pub token_usage: Option<(usize, usize, usize)>,
    /// Completion timestamp
    pub completed_at: chrono::DateTime<chrono::Utc>,
}

// ================================================================================
// AsyncTaskEntry
// ================================================================================

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
    /// Domain-specific metadata extension
    pub metadata: TaskMetadata,
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
            metadata: self.metadata.clone(),
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
            metadata: TaskMetadata::None,
            completion_tx: None,
        }
    }

    /// Create a new async task entry with metadata
    #[must_use]
    pub fn with_metadata(
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        parent_session_key: String,
        config: AsyncToolConfig,
        metadata: TaskMetadata,
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
            metadata,
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

// ================================================================================
// AsyncTaskRegistry
// ================================================================================

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

    // ================================================================================
    // Subagent-specific query methods
    // ================================================================================

    /// Get all tasks with `TaskMetadata::Subagent` for a parent session.
    #[must_use]
    pub fn list_subagents_for_parent(&self, parent_session_key: &str) -> Vec<&AsyncTaskEntry> {
        self.tasks
            .values()
            .filter(|e| e.parent_session_key == parent_session_key)
            .filter(|e| matches!(e.metadata, TaskMetadata::Subagent(_)))
            .collect()
    }

    /// Count active (non-terminal) subagents for a parent session.
    #[must_use]
    pub fn count_active_subagents_for_parent(&self, parent_session_key: &str) -> usize {
        self.tasks
            .values()
            .filter(|e| e.parent_session_key == parent_session_key)
            .filter(|e| matches!(e.metadata, TaskMetadata::Subagent(_)))
            .filter(|e| !e.status.is_terminal())
            .count()
    }

    /// Count total subagents for a parent session.
    #[must_use]
    pub fn count_subagents_for_parent(&self, parent_session_key: &str) -> usize {
        self.tasks
            .values()
            .filter(|e| e.parent_session_key == parent_session_key)
            .filter(|e| matches!(e.metadata, TaskMetadata::Subagent(_)))
            .count()
    }

    /// Get the spawn depth of a session by looking up where it was a child.
    #[must_use]
    pub fn get_subagent_depth_for_session(&self, session_key: &str) -> u32 {
        self.tasks
            .values()
            .filter_map(|e| match &e.metadata {
                TaskMetadata::Subagent(m) if m.child_session_key == session_key => Some(m.depth),
                _ => None,
            })
            .next()
            .unwrap_or(0)
    }

    /// Get subagent-specific result data (if any).
    #[must_use]
    pub fn get_subagent_result(&self, task_id: &AsyncTaskId) -> Option<SubagentResult> {
        self.tasks.get(task_id).and_then(|e| match &e.metadata {
            TaskMetadata::Subagent(m) => m.subagent_result.clone(),
            _ => None,
        })
    }

    /// Clean up terminal subagent runs older than a given duration
    pub fn cleanup_old_subagents(&mut self, max_age: chrono::Duration) -> usize {
        let now = chrono::Utc::now();
        let to_remove: Vec<String> = self
            .tasks
            .values()
            .filter(|e| matches!(e.metadata, TaskMetadata::Subagent(_)))
            .filter(|e| {
                e.status.is_terminal()
                    && e.completed_at
                        .is_some_and(|t| now.signed_duration_since(t) > max_age)
            })
            .map(|e| e.task_id.clone())
            .collect();

        let count = to_remove.len();
        for task_id in to_remove {
            self.tasks.remove(&task_id);
        }

        if count > 0 {
            tracing::info!("Cleaned up {} old subagent runs from registry", count);
        }
        count
    }
}

/// A generic, serializable view of any async task entry.
///
/// This is NOT stored — it is constructed on demand from the unified
/// registry's `AsyncTaskEntry`. It works for ALL task types regardless
/// of `TaskMetadata` variant.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub task_id: String,
    pub tool_name: String,
    pub status: AsyncTaskStatus,
    pub parent_session_key: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<Value>,
    pub label: Option<String>,
    pub metadata_type: String,
}

impl TaskView {
    /// Project an `AsyncTaskEntry` into a universal `TaskView`.
    #[must_use]
    pub fn from_entry(entry: &AsyncTaskEntry) -> Self {
        let metadata_type = match &entry.metadata {
            TaskMetadata::None => "none",
            TaskMetadata::Subagent(_) => "subagent",
        };

        Self {
            task_id: entry.task_id.clone(),
            tool_name: entry.tool_name.clone(),
            status: entry.status.clone(),
            parent_session_key: entry.parent_session_key.clone(),
            created_at: entry.created_at,
            completed_at: entry.completed_at,
            result: entry.result.clone(),
            label: entry.config.label.clone(),
            metadata_type: metadata_type.to_string(),
        }
    }

    /// Get duration of the task
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        let end = self.completed_at.unwrap_or_else(Utc::now);
        Some(end.signed_duration_since(self.created_at))
    }

    /// Check if status is terminal
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

pub type SharedAsyncTaskRegistry = Arc<tokio::sync::RwLock<AsyncTaskRegistry>>;

// ================================================================================
// Global per-agent registry cache
// ================================================================================

static GLOBAL_ASYNC_TASK_REGISTRIES: std::sync::OnceLock<
    std::sync::Mutex<HashMap<String, SharedAsyncTaskRegistry>>,
> = std::sync::OnceLock::new();

fn global_registries() -> &'static std::sync::Mutex<HashMap<String, SharedAsyncTaskRegistry>> {
    GLOBAL_ASYNC_TASK_REGISTRIES.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Get or create a shared async task registry for a given agent name.
///
/// This ensures that all `Agent` instances for the same agent name share
/// the same registry, making status queries and result delivery work
/// across stateless requests.
pub fn get_or_create_registry_for_agent(agent_name: &str) -> SharedAsyncTaskRegistry {
    let mut map = global_registries().lock().unwrap();
    map.entry(agent_name.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(AsyncTaskRegistry::new())))
        .clone()
}

/// Look up a task by ID across all agent registries.
pub async fn find_task_across_all_registries(task_id: &str) -> Option<AsyncTaskEntry> {
    let task_id = task_id.to_string();
    let registries: Vec<SharedAsyncTaskRegistry> = {
        let map = global_registries().lock().unwrap();
        map.values().cloned().collect()
    };
    for registry in registries {
        let reg = registry.read().await;
        if let Some(entry) = reg.get(&task_id) {
            return Some(entry.clone());
        }
    }
    None
}

/// List all tasks across all agent registries.
pub async fn list_all_tasks_across_all_registries() -> Vec<AsyncTaskEntry> {
    let registries: Vec<SharedAsyncTaskRegistry> = {
        let map = global_registries().lock().unwrap();
        map.values().cloned().collect()
    };
    let mut all = Vec::new();
    for registry in registries {
        let reg = registry.read().await;
        for entry in reg.list_tasks(None) {
            all.push(entry);
        }
    }
    all
}

/// Look up a subagent run by ID across all agent registries.
///
/// This is a convenience wrapper for subagent-specific lookups.
pub async fn find_run_across_all_registries(run_id: &str) -> Option<AsyncTaskEntry> {
    find_task_across_all_registries(run_id).await
}

/// List all subagent runs across all agent registries.
///
/// This is a convenience wrapper for subagent-specific listings.
pub async fn list_all_runs_across_all_registries() -> Vec<AsyncTaskEntry> {
    let all = list_all_tasks_across_all_registries().await;
    all.into_iter()
        .filter(|e| matches!(e.metadata, TaskMetadata::Subagent(_)))
        .collect()
}
