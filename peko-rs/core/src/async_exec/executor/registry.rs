//! Registry for tracking async tasks

use super::types::{AsyncTaskId, AsyncTaskStatus, AsyncToolConfig, WaitResult};
use crate::extensions::framework::registry::SimpleRegistry;
use serde_json::Value;
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
    /// The child's plain session id (uuid) — the id `session list`
    /// shows and Agent's `action = "resume"` consumes. Spawn-registered
    /// runs store the overlay key in `child_session_key`; this field
    /// lets busy/depth lookups match on the durable session id.
    /// `None` only for runs registered before this field existed.
    pub child_session_id: Option<String>,
    pub cleanup: peko_session::types::SpawnCleanupPolicy,
    pub depth: u32,
    /// The subagent result (output, error, token_usage) —
    /// distinct from the generic `AsyncTaskEntry.result` which is
    /// the raw JSON returned by the execution closure.
    pub subagent_result: Option<SubagentResult>,
}

pub use crate::tools::builtin::messaging::SubagentResult;

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
    /// F38: abort-signal sender for spawned tasks that opted in via
    /// `AsyncExecutor::dispatch_tool_with_signal`. When `Some`, calling
    /// `AsyncExecutor::cancel(task_id)` will `send(true)` to flip the
    /// inner tool's `ToolContext::is_aborted()` check. The matching
    /// `watch::Receiver<bool>` flows through `execute_tool_via_hook`'s
    /// `abort_signal` parameter (F38). Tool bodies that don't check
    /// `ctx.is_aborted()` remain uncancellable even with this set —
    /// the watch channel only short-circuits cancellations through
    /// the built-in `ToolContext` plumbing.
    cancel_signal: Option<tokio::sync::watch::Sender<bool>>,
    /// Whether the terminal outcome has already been delivered (inbox
    /// push / cron steer). Guards against double delivery now that
    /// `enable_completion_delivery` can deliver a terminal outcome
    /// itself when the flip races a fast-finishing task.
    delivered: bool,
    /// Live progress buffer (§4.1). Shared with the executing closure —
    /// long-running tools append what they have produced so far, and the
    /// buffer is surfaced through `Async action output` on non-terminal tasks and
    /// through the completion event on cancel/timeout, where the real
    /// result never materializes. `Arc<Mutex<..>>` so the entry and the
    /// executing closure share the same buffer across the registry's
    /// clone-heavy read paths.
    progress: Option<Arc<std::sync::Mutex<String>>>,
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
            // F38: cancel_signal is a per-task, per-instance channel —
            // cloning would split senders and is not safe. Drop on clone.
            cancel_signal: None,
            // The progress buffer is `Arc`-shared by design: a clone of
            // the entry must keep observing the *live* buffer, not a
            // snapshot. `delivered` starts false on a clone — clones are
            // read-path snapshots, and the delivery claim is made on the
            // registry-owned entry.
            delivered: false,
            progress: self.progress.clone(),
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
            progress: config.progress.clone(),
            config,
            formatted_result: None,
            metadata: TaskMetadata::None,
            completion_tx: None,
            cancel_signal: None,
            delivered: false,
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
            progress: config.progress.clone(),
            config,
            formatted_result: None,
            metadata,
            completion_tx: None,
            cancel_signal: None,
            delivered: false,
        }
    }

    /// Set the result and update `formatted_result` cache
    pub fn set_result(&mut self, result: Value) {
        self.formatted_result = Some(self.format_result(&result));
        self.result = Some(result);
    }

    /// Snapshot the progress buffer, trimmed to `max_bytes` from the tail.
    /// Returns `None` when the task has no progress buffer or nothing has
    /// been appended yet.
    #[must_use]
    pub fn partial_output(&self, max_bytes: usize) -> Option<String> {
        let buf = self.progress.as_ref()?;
        let guard = buf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_empty() {
            return None;
        }
        Some(peko_tools_core::background::tail(&guard, max_bytes).to_string())
    }

    /// Whether the terminal outcome has already been delivered.
    #[must_use]
    pub fn is_delivered(&self) -> bool {
        self.delivered
    }

    /// Mark the terminal outcome as delivered. Called under the
    /// registry's write lock so the claim is atomic with the
    /// `deliver_completion` check — two racers (the spawned task and a
    /// late `enable_completion_delivery` flip) cannot both deliver.
    pub fn mark_delivered(&mut self) {
        self.delivered = true;
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

    /// F38: set the abort-signal sender. The matching receiver flows
    /// into the spawned task's `execute_tool_via_hook(..., abort_signal)` call.
    /// Calling `AsyncExecutor::cancel(task_id)` will fire `send(true)` on
    /// this sender to flip `ToolContext::is_aborted()` for tools that respect
    /// it.
    pub fn set_cancel_signal(&mut self, tx: tokio::sync::watch::Sender<bool>) {
        self.cancel_signal = Some(tx);
    }

    /// F38: signal the inner tool to abort (no-op if no cancel_signal
    /// was set, e.g. for tasks created via the original
    /// `AsyncExecutor::execute(...)` API). Idempotent — repeated calls
    /// are no-ops once `true` has been sent.
    pub fn signal_cancel(&self) {
        if let Some(ref tx) = self.cancel_signal {
            let _ = tx.send(true);
        }
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

/// Cap on the partial-output preview surfaced through the completion
/// event and `Async action output` — the agent gets a window into what the task
/// produced, not the whole thing (the full tail stays in the task file
/// for tools that stream there).
pub const PARTIAL_OUTPUT_PREVIEW_BYTES: usize = 4 * 1024;

/// Registry for tracking async tasks.
///
/// Wraps a [`SimpleRegistry`] for task storage.
#[derive(Debug, Default)]
pub struct AsyncTaskRegistry {
    tasks: SimpleRegistry<AsyncTaskId, AsyncTaskEntry>,
}

impl AsyncTaskRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: SimpleRegistry::new(),
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

    /// Check if a task exists and return its current status
    #[must_use]
    pub fn check_status(&self, task_id: &AsyncTaskId) -> Option<AsyncTaskStatus> {
        self.tasks.get(task_id).map(|e| e.status.clone())
    }

    /// Convert status to wait result
    pub(crate) fn status_to_wait_result(status: &AsyncTaskStatus) -> WaitResult {
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

    /// Whether a non-terminal subagent run is currently registered for
    /// the given child session (id or overlay key). The unified
    /// registry is the active-run source of truth for subagent runs —
    /// `InboxRegistry` run permits are only held for root sessions.
    #[must_use]
    pub fn has_active_subagent_run_for_child(&self, child: &str) -> bool {
        self.tasks.values().any(|e| {
            e.tool_name == "Agent"
                && !e.status.is_terminal()
                && match &e.metadata {
                    TaskMetadata::Subagent(m) => {
                        m.child_session_key == child || m.child_session_id.as_deref() == Some(child)
                    }
                    _ => false,
                }
        })
    }

    /// Cancel a task by ID, returning structured result.
    pub fn cancel(&mut self, task_id: &AsyncTaskId) -> CancelResult {
        match self.tasks.get_mut(task_id) {
            Some(entry) => {
                let previous = entry.status.as_str().to_string();
                if entry.status.is_terminal() {
                    CancelResult::AlreadyTerminal { previous }
                } else {
                    entry.status = AsyncTaskStatus::Cancelled;
                    entry.completed_at = Some(chrono::Utc::now());
                    entry.notify_completion();
                    CancelResult::Success { previous }
                }
            }
            None => CancelResult::NotFound,
        }
    }
}

/// Result of attempting to cancel a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelResult {
    /// Task was found and cancelled.
    Success { previous: String },
    /// Task was found but already in a terminal state.
    AlreadyTerminal { previous: String },
    /// Task was not found in the registry.
    NotFound,
}

pub type SharedAsyncTaskRegistry = Arc<tokio::sync::RwLock<AsyncTaskRegistry>>;

/// Poll `registry` for `task_id` with short-lived read guards until the
/// task reaches a terminal state or `timeout` elapses.
///
/// Unlike [`AsyncTaskRegistry::wait_for_completion`] — which the caller
/// traditionally invoked while holding a read guard for the entire
/// wait — this helper drops the guard between polls so the executor's
/// background writer is never starved of the write lock (the same
/// hazard `SubagentExecutor::wait_for_run` documents inline).
pub async fn wait_for_completion_polled(
    registry: &SharedAsyncTaskRegistry,
    task_id: &AsyncTaskId,
    timeout: Duration,
) -> anyhow::Result<WaitResult> {
    let start = tokio::time::Instant::now();
    loop {
        let status = {
            let reg = registry.read().await;
            reg.check_status(task_id)
        };
        match status {
            Some(s) if s.is_terminal() => {
                return Ok(AsyncTaskRegistry::status_to_wait_result(&s));
            }
            Some(_) => {}
            None => return Err(anyhow::anyhow!("Task {task_id} not found in registry")),
        }

        if start.elapsed() >= timeout {
            return Ok(WaitResult::Timeout);
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        tokio::time::sleep(Duration::from_millis(50).min(remaining)).await;
    }
}
