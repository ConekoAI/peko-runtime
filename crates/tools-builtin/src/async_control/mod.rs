//! `peko_tools_builtin::async_control` — Async control tool surface +
//! `AsyncRuntime` port.
//!
//! Phase 10c extracts the six async tools (`AsyncSpawn`, `AsyncOutput`,
//! `AsyncList`, `AsyncStatus`, `AsyncStop`, plus `async_common` helpers)
//! out of root. Per the Phase 10 plan rule ("Built-ins must not import
//! daemon state"), the tools here do NOT call `crate::extensions::framework::async_exec`
//! types directly. They speak to a runtime port trait ([`AsyncRuntime`])
//! that the agent side implements.
//!
//! ## DTOs
//!
//! [`SpawnRequest`], [`SpawnReceipt`], [`AsyncToolConfig`],
//! [`AsyncResultDeliveryMode`], [`DeliveryTarget`], [`WaitResult`],
//! [`TaskView`], [`CancelResult`], and [`SessionMessageType`] are
//! serialization-friendly types shared between the tool side and the
//! framework-host side. peko-tools-builtin is the canonical home; the
//! framework re-exports them for backward compatibility.
//!
//! ## Port
//!
//! [`AsyncRuntime`] is the five-method surface the async tools need:
//! spawn / lookup / list / cancel / wait_for_completion. The
//! framework-host side implements it (see
//! `src/extensions/framework/async_exec/executor/async_runtime_impl.rs`).

pub mod common;
pub mod list;
pub mod output;
pub mod spawn;
pub mod status;
pub mod stop;

pub use common::{
    apply_tail_lines, build_cancel_response, build_list_response, build_output_response,
    build_status_response, AsyncTaskHelper,
};
pub use list::AsyncListTool;
pub use output::AsyncOutputTool;
pub use spawn::AsyncSpawnTool;
pub use status::AsyncStatusTool;
pub use stop::AsyncStopTool;

// ─── DTOs (canonical home; root re-exports these) ─────────────────

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Request to spawn a new async task.
///
/// The runtime adapter overlays the spawning principal's identity
/// (`principal_id`, `capabilities`) at dispatch time, so those fields
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
    /// Whether completion should nudge the spawning session's next turn.
    ///
    /// `true` (default for natural agent spawns) prompts a
    /// `SteeringMessage` into the principal's root inbox. Cron
    /// schedules override to `false`.
    #[serde(default = "default_true")]
    pub wake_on_completion: bool,
    /// Maximum lifetime of the spawned task (seconds). `None` uses the
    /// executor's default (2h).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

fn default_true() -> bool {
    true
}

/// Receipt returned when an async task was spawned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnReceipt {
    pub task_id: String,
}

/// Result delivery modes
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

/// Delivery target types for async task results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryTarget {
    /// Deliver to session via announcement
    SessionAnnouncement,
    /// Deliver to async result queue
    #[default]
    AsyncQueue,
    /// Deliver via EventSubscriber broadcast
    EventBroadcast,
    /// Deliver via direct channel (for sync waiting)
    DirectChannel,
}

/// Configuration for async tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncToolConfig {
    pub delivery_mode: AsyncResultDeliveryMode,
    pub delivery_target: Option<DeliveryTarget>,
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_millis: Option<u64>,
    pub cleanup_after_delivery: bool,
    pub label: Option<String>,
    #[serde(default = "default_wake_on_completion")]
    pub wake_on_completion: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_root_session_key: Option<String>,
}

fn default_wake_on_completion() -> bool {
    true
}

impl Default for AsyncToolConfig {
    fn default() -> Self {
        Self {
            delivery_mode: AsyncResultDeliveryMode::QueueWhenBusy,
            delivery_target: None,
            timeout_secs: Some(7200),
            timeout_millis: None,
            cleanup_after_delivery: true,
            label: None,
            wake_on_completion: default_wake_on_completion(),
            principal_root_session_key: None,
        }
    }
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
        }
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

/// Message types for principal-to-principal communication
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SessionMessageType {
    /// Initial request to another agent
    #[default]
    Request,
    /// Response to a request
    Response,
    /// Fire-and-forget announcement
    Announcement,
    /// Subagent completion notification
    Completion,
    /// Error/timeout notification
    Error,
}

// ─── Port trait ────────────────────────────────────────────────────

/// Runtime port the built-in async control tools speak to.
///
/// The framework-host implements this via `AsyncExecutorRuntime` (which
/// wraps the per-agent `AsyncExecutor` + `Weak<ExtensionCore>` +
/// `principal_id` + `capabilities` snapshot). The trait is per-agent:
/// each `Agent` constructs one runtime and shares it across its
/// `AsyncSpawn`/`AsyncOutput`/`AsyncStatus`/`AsyncList`/`AsyncStop`
/// instances.
///
/// `lookup` / `list` / `cancel` operate on the runtime's own task set
/// (per-agent). The cross-cutting "see all agents' tasks" feature that
/// the legacy helper functions offered is collapsed here — the per-agent
/// scope is what every current caller actually uses.
#[async_trait]
pub trait AsyncRuntime: Send + Sync {
    /// Spawn a new async task by invoking `request.tool_name` with
    /// `request.params`. Returns the new task ID on success.
    async fn spawn(&self, request: SpawnRequest) -> Result<SpawnReceipt>;

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
