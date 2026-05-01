//! Core types for the async executor framework

use crate::tools::core::traits::ToolResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Unique identifier for an async task
pub type AsyncTaskId = String;

/// Status of an async task
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsyncTaskStatus {
    Pending,
    Running,
    Completed { result: ToolResult },
    Failed { error: String },
    Cancelled,
    TimedOut { error: String },
}

impl Serialize for AsyncTaskStatus {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Pending => serializer.serialize_str("pending"),
            Self::Running => serializer.serialize_str("running"),
            Self::Completed { result } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("status", "completed")?;
                map.serialize_entry("result", result)?;
                map.end()
            }
            Self::Failed { error } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("status", "failed")?;
                map.serialize_entry("error", error)?;
                map.end()
            }
            Self::Cancelled => serializer.serialize_str("cancelled"),
            Self::TimedOut { error } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("status", "timed_out")?;
                map.serialize_entry("error", error)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for AsyncTaskStatus {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => match s.as_str() {
                "pending" => Ok(Self::Pending),
                "running" => Ok(Self::Running),
                "cancelled" => Ok(Self::Cancelled),
                _ => Err(serde::de::Error::custom(format!("unknown status: {s}"))),
            },
            serde_json::Value::Object(mut map) => {
                let status = map
                    .get("status")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::custom("missing status field"))?;
                match status {
                    "completed" => {
                        let result = map
                            .remove("result")
                            .ok_or_else(|| serde::de::Error::custom("missing result field"))?;
                        let result: ToolResult = serde_json::from_value(result)
                            .map_err(|e| serde::de::Error::custom(e.to_string()))?;
                        Ok(Self::Completed { result })
                    }
                    "failed" => {
                        let error = map
                            .remove("error")
                            .and_then(|v| v.as_str().map(String::from))
                            .ok_or_else(|| serde::de::Error::custom("missing error field"))?;
                        Ok(Self::Failed { error })
                    }
                    "timed_out" => {
                        let error = map
                            .remove("error")
                            .and_then(|v| v.as_str().map(String::from))
                            .ok_or_else(|| serde::de::Error::custom("missing error field"))?;
                        Ok(Self::TimedOut { error })
                    }
                    _ => Err(serde::de::Error::custom(format!(
                        "unknown status: {status}"
                    ))),
                }
            }
            _ => Err(serde::de::Error::custom("expected string or object")),
        }
    }
}

impl std::fmt::Display for AsyncTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl AsyncTaskStatus {
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AsyncTaskStatus::Completed { .. }
                | AsyncTaskStatus::Failed { .. }
                | AsyncTaskStatus::Cancelled
                | AsyncTaskStatus::TimedOut { .. }
        )
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            AsyncTaskStatus::Pending => "pending",
            AsyncTaskStatus::Running => "running",
            AsyncTaskStatus::Completed { .. } => "completed",
            AsyncTaskStatus::Failed { .. } => "failed",
            AsyncTaskStatus::Cancelled => "cancelled",
            AsyncTaskStatus::TimedOut { .. } => "timed_out",
        }
    }
}

/// Opaque async result — tool-specific structure lives inside the Value.
pub type AsyncTaskResult = serde_json::Value;

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

/// Message types for session-to-session communication (A2A)
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
