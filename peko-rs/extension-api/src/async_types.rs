//! Async execution receipt types

use serde::{Deserialize, Serialize};

/// Async execution receipt returned by extensions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncReceipt {
    /// Unique task identifier
    pub task_id: String,

    /// Estimated duration in seconds (for progress estimation)
    pub estimated_duration_secs: Option<u64>,

    /// Path to the task file on disk for polling (Option 3: minimal file-based polling)
    pub task_file: Option<std::path::PathBuf>,

    /// Optional metadata
    pub metadata: Option<serde_json::Value>,
}

impl AsyncReceipt {
    /// Create a new async receipt
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            estimated_duration_secs: None,
            task_file: None,
            metadata: None,
        }
    }

    /// Set estimated duration
    #[must_use]
    pub fn with_duration(mut self, seconds: u64) -> Self {
        self.estimated_duration_secs = Some(seconds);
        self
    }

    /// Set metadata
    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_async_receipt() {
        let receipt = AsyncReceipt::new("task-123").with_duration(60);
        assert_eq!(receipt.task_id, "task-123");
        assert_eq!(receipt.estimated_duration_secs, Some(60));
    }
}
