//! Async domain actions and runtime contracts.

pub mod common;
mod list;
mod output;
mod spawn;
mod status;
mod stop;

pub use common::{
    apply_tail_lines, build_cancel_response, build_list_response, build_output_response,
    build_status_response, AsyncTaskHelper,
};
pub(crate) use list::AsyncListAction;
pub(crate) use output::AsyncOutputAction;
pub(crate) use spawn::AsyncSpawnAction;
pub(crate) use status::AsyncStatusAction;
pub(crate) use stop::AsyncStopAction;

// ─── DTOs (canonical home; root re-exports these) ─────────────────

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Request to spawn a new async task.
///
/// The runtime adapter overlays the spawning principal's identity
/// (`principal_id`) at dispatch time, so those fields
/// do not appear here — built-in tools do not own them; the per-agent
/// runtime does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// Name of the tool to invoke asynchronously.
    pub tool_name: String,
    /// Parameters forwarded verbatim to the spawned tool.
    pub params: serde_json::Value,
    /// Optional human-readable label for the spawned task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whether completion should wake the spawning session when it is
    /// idle (no run in flight): the daemon drives a follow-up turn that
    /// drains the queued completion. `false` delivers silently — the
    /// completion waits in the inbox for the next run. Cron's spawn-tool
    /// path routes delivery through `principal_root_session_key` steering
    /// instead.
    #[serde(default = "default_true")]
    pub wake_on_completion: bool,
    /// Maximum lifetime of the spawned task (seconds). `None` uses the
    /// executor's default (2h).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Millisecond-precision lifetime; takes precedence over
    /// `timeout_secs`. Internal: set by tools that spawn themselves (Bash
    /// `run_in_background` + `timeout`), not exposed in the Async schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_millis: Option<u64>,
    /// Per-call parent session identity (ADR-061 follow-up): the
    /// session the spawn is attributed to — stamped as the task
    /// record's `parent_session_key` and used as the completion-event
    /// delivery inbox key. Filled by `AsyncSpawnAction` from
    /// `ToolContext.session_id` (the run id on the agent-loop path;
    /// the token-resolved node id — or session-key string — on the
    /// `ExecuteTool` path). `None` delegates stamping to the runtime's
    /// legacy session-key cell (in-process dispatches without session
    /// ctx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Receipt returned when an async task was spawned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnReceipt {
    pub task_id: String,
}

/// Result of waiting for an async task to complete
#[derive(Debug, Clone)]
pub enum WaitResult {
    Completed {
        result: peko_tools_core::exec::ToolResult,
    },
    Failed {
        error: String,
    },
    Cancelled,
    Timeout,
}

/// A universal, serializable view of any async task entry.
///
/// This is constructed on demand from the framework-host's
/// `AsyncTaskEntry`. Works for all task types regardless of internal
/// `TaskMetadata` variant. Lives in peko-tools-builtin because the
/// async control tools need to project to it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub task_id: String,
    pub tool_name: String,
    pub status: String,
    pub parent_session_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<serde_json::Value>,
    pub label: Option<String>,
    pub metadata_type: String,
    /// Tail of the task's live progress buffer, for tasks that are still
    /// running (§4.1). `None` for terminal tasks.
    pub partial_output: Option<String>,
}

impl TaskView {
    /// Project an `AsyncTaskEntry`-like record. The
    /// `from_async_task_entry` constructor in the host crate adapts the
    /// concrete `AsyncTaskEntry` to this shape. Built-in tools only see
    /// the [`TaskView`] projection; the raw `AsyncTaskEntry` is
    /// framework-internal.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        task_id: String,
        tool_name: String,
        status: String,
        parent_session_key: String,
        created_at: chrono::DateTime<chrono::Utc>,
        completed_at: Option<chrono::DateTime<chrono::Utc>>,
        result: Option<serde_json::Value>,
        label: Option<String>,
        metadata_type: String,
    ) -> Self {
        Self {
            task_id,
            tool_name,
            status,
            parent_session_key,
            created_at,
            completed_at,
            result,
            label,
            metadata_type,
            partial_output: None,
        }
    }

    /// Attach the task's live progress tail (§4.1). Builder-style so the
    /// existing `new` call sites stay untouched.
    #[must_use]
    pub fn with_partial_output(mut self, partial_output: Option<String>) -> Self {
        self.partial_output = partial_output;
        self
    }

    /// Get duration of the task
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        let end = self.completed_at.unwrap_or_else(chrono::Utc::now);
        Some(end.signed_duration_since(self.created_at))
    }

    /// Check if status is terminal
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "cancelled" | "timed_out"
        )
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

// ─── Port trait ────────────────────────────────────────────────────

/// Runtime port the built-in async control tools speak to.
///
/// The framework-host implements this via `AsyncExecutorRuntime` (which
/// wraps the per-agent `AsyncExecutor` + `Weak<ToolingRuntime>` +
/// principal identity). The trait is per-agent:
/// production shares one executor per principal across turns. Caller session
/// and workspace context are supplied through `spawn_with_context`.
/// Lookup/list/cancel apply principal ownership, including registry fallbacks
/// for subagent runs and detached foreground calls.
#[async_trait]
pub trait AsyncRuntime: Send + Sync {
    /// Spawn a new async task by invoking `request.tool_name` with
    /// `request.params`. Returns the new task ID on success.
    async fn spawn(&self, request: SpawnRequest) -> Result<SpawnReceipt>;

    /// Preserve the invocation's caller context when scheduling a tool.
    /// Adapters without context-dependent dispatch may use the default.
    async fn spawn_with_context(
        &self,
        request: SpawnRequest,
        _ctx: &peko_tools_core::ToolContext,
    ) -> Result<SpawnReceipt> {
        self.spawn(request).await
    }

    /// Look up a task by ID within this runtime's scope.
    async fn lookup(&self, task_id: &str) -> Option<TaskView>;

    /// List tasks in this runtime's scope with optional filters.
    async fn list(&self, status_filter: Option<&str>, tool_filter: Option<&str>) -> Vec<TaskView>;

    /// Cancel a task by ID within this runtime's scope.
    async fn cancel(&self, task_id: &str) -> CancelResult;

    /// Block until the task reaches a terminal state, or the timeout
    /// elapses. Returns `Ok(WaitResult::Timeout)` on timeout.
    async fn wait_for_completion(&self, task_id: &str, timeout: Duration) -> Result<WaitResult>;
}

/// Type alias for the shared runtime handle threaded through every
/// `Async*Tool` constructor. Tools hold `Arc<dyn AsyncRuntime>` so
/// per-agent swapping (e.g. in tests) is straightforward.
pub type SharedAsyncRuntime = Arc<dyn AsyncRuntime>;

/// [`peko_tools_core::BackgroundSpawner`] backed by a principal's Async
/// runtime, so a tool that starts itself in the background (Bash
/// `run_in_background`) takes exactly the `Async action=spawn` path: same
/// executor, same registry, same owner.
pub(crate) struct RuntimeSpawner(pub SharedAsyncRuntime);

#[async_trait]
impl peko_tools_core::BackgroundSpawner for RuntimeSpawner {
    async fn spawn(
        &self,
        request: peko_tools_core::BackgroundSpawn,
        ctx: &peko_tools_core::ToolContext,
    ) -> anyhow::Result<String> {
        let receipt = self
            .0
            .spawn_with_context(
                SpawnRequest {
                    tool_name: request.tool,
                    params: request.params,
                    label: None,
                    wake_on_completion: true,
                    timeout_secs: None,
                    timeout_millis: request.timeout_millis,
                    parent_session_id: ctx.session_id.clone().filter(|s| !s.is_empty()),
                },
                ctx,
            )
            .await?;
        Ok(receipt.task_id)
    }
}

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod tool_tests;

mod tool;
pub use tool::AsyncTool;
