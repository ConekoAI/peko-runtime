//! Core types for the async executor framework
//!
//! Phase 7 split this module in two:
//!
//! - `AsyncTaskStatus` and `AsyncTaskId` are **framework contracts**
//!   (they tag the `HookOutput::TaskStatus` variant) and now live in
//!   the `peko-extension-api` workspace crate. The shim re-exports
//!   them from there so existing
//!   `peko::extensions::framework::async_exec::executor::AsyncTaskStatus`
//!   paths keep resolving unchanged.
//! - `AsyncTaskResult`, `AsyncTaskReceipt`, `AsyncResultDeliveryMode`,
//!   `AsyncToolConfig`, `WaitResult`, `DeliveryTarget`, and
//!   `SessionMessageType` are **executor-internal** types that depend
//!   on `peko-tools-core::ToolResult` and other host-only deps. They
//!   stay in the framework host; Phase 8 may move the entire executor
//!   to `peko-extension-host`.

use crate::tools::core::ToolResult;
use serde::{Deserialize, Serialize};

// Re-export the framework-contract types that moved to peko-extension-api
// in Phase 7. The contract types live next to the `HookOutput::TaskStatus`
// variant; the executor imports them from there.
pub use peko_extension_api::async_status::{AsyncTaskId, AsyncTaskResult, AsyncTaskStatus};

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

/// Configuration for async tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncToolConfig {
    /// How to deliver results to the parent agent (queue mode)
    pub delivery_mode: AsyncResultDeliveryMode,
    /// Which delivery mechanism to use (optional, defaults to executor default)
    pub delivery_target: Option<DeliveryTarget>,
    /// Maximum time to wait for task completion. `None` means no timeout
    /// (the task runs to completion or until cancelled).
    pub timeout_secs: Option<u64>,
    /// Optional millisecond-precision timeout. When set, takes precedence
    /// over `timeout_secs` so callers can request sub-second timeouts
    /// (e.g. `Bash { run_in_background, timeout: 100 }`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_millis: Option<u64>,
    /// Whether to delete task record after delivery
    pub cleanup_after_delivery: bool,
    /// Label for grouping/identifying tasks
    pub label: Option<String>,
    /// Whether completion should wake the spawning session.
    ///
    /// When `true` and [`Self::principal_root_session_key`] is set, the
    /// executor delivers a `SteeringMessage` into the principal's root
    /// inbox instead of a `CompletionEvent`. The agent picks the message
    /// up at its next iteration start.
    ///
    /// Defaults to `true` for natural agent spawns. The cron engine
    /// overrides to `false` because scheduled runs do not need to nudge
    /// the agent's next turn — the user (or the janitor) decides what
    /// to do next.
    #[serde(default = "default_wake_on_completion")]
    pub wake_on_completion: bool,
    /// When the spawn is attributed to a principal's root (e.g. via the
    /// cron engine), this is the inbox key to push a steer message into.
    /// `None` means deliver the existing `CompletionEvent` to
    /// `parent_session_key` instead.
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

/// Delivery target types for async task results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
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
