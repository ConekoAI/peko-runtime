//! Background tool task status and results.

use crate::ToolResult;

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
