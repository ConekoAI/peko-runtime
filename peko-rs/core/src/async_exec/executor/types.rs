//! Host task execution bookkeeping. Shared task statuses live in tools-core.

use std::sync::Arc;

use peko_tools_core::ToolResult;
use serde::{Deserialize, Serialize};

// Shared background task status contracts
pub use peko_tools_core::async_status::{AsyncTaskId, AsyncTaskResult, AsyncTaskStatus};

/// Receipt returned to agent when spawning an async task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncTaskReceipt {
    pub task_id: AsyncTaskId,
    pub status: AsyncTaskStatus,
    pub estimated_duration_secs: Option<u64>,
    /// Path to the task file on disk for polling
    pub task_file: Option<std::path::PathBuf>,
    /// Parameters the agent used to invoke the tool (audit transparency)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Configuration for async tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncToolConfig {
    /// Maximum time to wait for task completion. `None` means no timeout
    /// (the task runs to completion or until cancelled). Callers that
    /// omit a timeout get the default `Some(7200)` from
    /// [`Default::default`] — the timeout only disappears when a caller
    /// explicitly maps `0`/unlimited semantics to `None`
    /// (`SubagentExecutor`'s `timeout_seconds == 0`).
    pub timeout_secs: Option<u64>,
    /// Optional millisecond-precision timeout. When set, takes precedence
    /// over `timeout_secs` so callers can request sub-second timeouts
    /// (e.g. `Bash { run_in_background, timeout: 100 }`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_millis: Option<u64>,
    /// Whether the janitor may purge the registry entry shortly after
    /// the task reaches a terminal state. Subagent runs set `false` so
    /// the run record survives for `wait_for_run` / session guards.
    pub cleanup_after_delivery: bool,
    /// Label for grouping/identifying tasks
    pub label: Option<String>,
    /// Whether completion should wake the spawning session when it is
    /// idle (no run in flight when the task terminates).
    ///
    /// - `true` (default): after the completion is pushed to the
    ///   session inbox, the executor fires the process-global wake hook
    ///   (`wake::notify_completion_wake`) so the daemon can start a
    ///   successor turn. A session with a run in flight is not woken —
    ///   the loop drains the inbox at the next iteration boundary.
    /// - When [`Self::principal_root_session_key`] is also set (the
    ///   cron engine's spawn path), the terminal outcome is delivered
    ///   as a `SteeringMessage` into that root inbox instead of a
    ///   `CompletionEvent`, and the wake hook fires for that key.
    ///
    /// `false` skips the wake hook (completion still lands in the
    /// inbox for the next run to drain).
    #[serde(default = "default_wake_on_completion")]
    pub wake_on_completion: bool,
    /// When the spawn is attributed to a principal's root (e.g. via the
    /// cron engine), this is the inbox key to push a steer message into.
    /// `None` means deliver the existing `CompletionEvent` to
    /// `parent_session_key` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_root_session_key: Option<String>,
    /// Owning principal (string form of the principal id), stamped at
    /// spawn time by dispatch paths that know the caller. `None` means
    /// system/unattributed — such tasks remain visible to every
    /// principal. `AsyncExecutorRuntime` filters `list`/`lookup`/`cancel`
    /// by this field so one principal's agent cannot observe or cancel
    /// another principal's tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    /// Whether reaching a terminal state pushes a `CompletionEvent`
    /// into the parent session's inbox at all.
    ///
    /// Default `true` — that push is the delivery mechanism for
    /// `AsyncSpawn` / background `Bash` / subagent runs. The
    /// `AsyncExecutionRouter` sets it `false` at spawn (a call that
    /// completes inside the router's timeout already returned its
    /// result synchronously; an inbox event would be pure noise) and
    /// flips it to `true` via
    /// [`super::executor::AsyncExecutor::enable_completion_delivery`]
    /// only when the call detaches past the router timeout and the
    /// agent receives a `queued` receipt instead.
    #[serde(default = "default_deliver_completion")]
    pub deliver_completion: bool,
    /// Live progress buffer shared with the executing closure (§4.1).
    ///
    /// `Some` only for spawn paths that can stream progress — today that
    /// is background `Bash`, whose child-process output is appended as it
    /// is produced. Surfaced through `AsyncOutput` while the task runs
    /// and folded into the completion event when the task is cancelled or
    /// times out, where no real result ever materializes.
    ///
    /// `serde(skip)`: an `Arc<Mutex<..>>` is process-local shared state,
    /// not wire data, and the config is serialized into task files.
    #[serde(skip, default)]
    pub progress: Option<Arc<std::sync::Mutex<String>>>,
}

fn default_wake_on_completion() -> bool {
    true
}

fn default_deliver_completion() -> bool {
    true
}

impl Default for AsyncToolConfig {
    fn default() -> Self {
        Self {
            // Default async-task lifetime is 2 hours. Callers can override
            // per call via `timeout_secs` or `timeout_millis`. Cron schedules
            // and natural agent spawns both inherit this default; both
            // surfaces override it explicitly when needed.
            timeout_secs: Some(7200),
            timeout_millis: None,
            cleanup_after_delivery: true,
            label: None,
            wake_on_completion: default_wake_on_completion(),
            principal_root_session_key: None,
            principal_id: None,
            deliver_completion: default_deliver_completion(),
            progress: None,
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
