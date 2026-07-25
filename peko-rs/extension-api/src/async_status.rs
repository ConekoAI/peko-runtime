//! `AsyncTaskStatus` — the status of an async tool task.
//!
//! Moved from `src/extensions/framework/async_exec/executor/types.rs` in
//! Phase 7. The variant is part of the `HookOutput::TaskStatus` contract,
//! so it must live in the API crate even though the surrounding executor
//! is a host-only implementation. Uses `peko_tools_core::ToolResult` for
//! the completed-result payload; that crate is a permitted dependency
//! of the extension API.

use peko_tools_core::ToolResult;
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_status_terminal_set() {
        assert!(AsyncTaskStatus::Cancelled.is_terminal());
        assert!(!AsyncTaskStatus::Pending.is_terminal());
        assert!(!AsyncTaskStatus::Running.is_terminal());
    }

    #[test]
    fn async_status_as_str() {
        assert_eq!(AsyncTaskStatus::Pending.as_str(), "pending");
        assert_eq!(AsyncTaskStatus::Running.as_str(), "running");
        assert_eq!(AsyncTaskStatus::Cancelled.as_str(), "cancelled");
    }
}
