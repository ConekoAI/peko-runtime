//! Subagent Registry
//!
//! Tracks active subagent runs, their status, and results.
//! Used by the agent spawn tool to manage subagent lifecycle.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Status of a subagent run
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentStatus {
    /// Run is currently executing
    Running,
    /// Run completed successfully
    Completed,
    /// Run failed with an error
    Failed,
    /// Run was cancelled
    Cancelled,
    /// Run timed out
    TimedOut,
}

impl SubagentStatus {
    /// Check if the status is terminal (completed, failed, cancelled, or timed out)
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            SubagentStatus::Completed
                | SubagentStatus::Failed
                | SubagentStatus::Cancelled
                | SubagentStatus::TimedOut
        )
    }

    /// Convert status to string representation
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            SubagentStatus::Running => "running",
            SubagentStatus::Completed => "completed",
            SubagentStatus::Failed => "failed",
            SubagentStatus::Cancelled => "cancelled",
            SubagentStatus::TimedOut => "timed_out",
        }
    }
}

impl std::fmt::Display for SubagentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Result of a subagent run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentResult {
    /// Final status
    pub status: SubagentStatus,
    /// Output content (if successful)
    pub output: Option<String>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Token usage (input, output, total)
    pub token_usage: Option<(usize, usize, usize)>,
    /// Completion timestamp
    pub completed_at: DateTime<Utc>,
}

/// Information about a registered subagent run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRun {
    /// Unique run ID
    pub run_id: String,
    /// Child session key
    pub child_session_key: String,
    /// Parent session key
    pub parent_session_key: String,
    /// Task description
    pub task: String,
    /// Current status
    pub status: SubagentStatus,
    /// When the run started
    pub started_at: DateTime<Utc>,
    /// When the run completed (if terminal)
    pub completed_at: Option<DateTime<Utc>>,
    /// Cleanup policy
    pub cleanup: crate::session::types::SpawnCleanupPolicy,
    /// Optional label for the run
    pub label: Option<String>,
    /// Run result (if completed)
    pub result: Option<SubagentResult>,
    /// Child depth (nesting level)
    pub depth: u32,
    /// Whether to announce completion to parent
    pub announce_completion: bool,
}

impl SubagentRun {
    /// Create a new subagent run
    #[must_use]
    pub fn new(
        run_id: String,
        child_session_key: String,
        parent_session_key: String,
        task: String,
        cleanup: crate::session::types::SpawnCleanupPolicy,
        label: Option<String>,
        depth: u32,
    ) -> Self {
        Self {
            run_id,
            child_session_key,
            parent_session_key,
            task,
            status: SubagentStatus::Running,
            started_at: Utc::now(),
            completed_at: None,
            cleanup,
            label,
            result: None,
            depth,
            announce_completion: true,
        }
    }

    /// Mark the run as completed
    pub fn complete(&mut self, result: SubagentResult) {
        self.status = result.status;
        self.result = Some(result);
        self.completed_at = Some(Utc::now());
    }

    /// Get duration of the run
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        let end = self.completed_at.unwrap_or_else(Utc::now);
        Some(end.signed_duration_since(self.started_at))
    }
}

/// Registry for tracking subagent runs
#[derive(Debug, Default)]
pub struct SubagentRegistry {
    runs: HashMap<String, SubagentRun>,
}

impl SubagentRegistry {
    /// Create a new empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            runs: HashMap::new(),
        }
    }

    /// Register a new subagent run
    pub fn register(&mut self, run: SubagentRun) {
        tracing::info!(
            "Registered subagent run: {} (child: {}, parent: {}, depth: {})",
            run.run_id,
            run.child_session_key,
            run.parent_session_key,
            run.depth
        );
        self.runs.insert(run.run_id.clone(), run);
    }

    /// Get a run by ID
    #[must_use]
    pub fn get(&self, run_id: &str) -> Option<&SubagentRun> {
        self.runs.get(run_id)
    }

    /// Get a mutable run by ID
    #[must_use]
    pub fn get_mut(&mut self, run_id: &str) -> Option<&mut SubagentRun> {
        self.runs.get_mut(run_id)
    }

    /// Update a run's status
    pub fn update_status(&mut self, run_id: &str, status: SubagentStatus) -> Option<()> {
        let run = self.runs.get_mut(run_id)?;
        run.status = status;
        if status.is_terminal() {
            run.completed_at = Some(Utc::now());
        }
        Some(())
    }

    /// Complete a run with a result
    pub fn complete(&mut self, run_id: &str, result: SubagentResult) -> Option<()> {
        let run = self.runs.get_mut(run_id)?;
        run.complete(result);
        tracing::info!(
            "Completed subagent run: {} (status: {})",
            run_id,
            run.status
        );
        Some(())
    }

    /// Remove a run from the registry
    pub fn remove(&mut self, run_id: &str) -> Option<SubagentRun> {
        self.runs.remove(run_id)
    }

    /// Count active (non-terminal) runs for a parent session
    #[must_use]
    pub fn count_active_for_parent(&self, parent_session_key: &str) -> usize {
        self.runs
            .values()
            .filter(|run| run.parent_session_key == parent_session_key && !run.status.is_terminal())
            .count()
    }

    /// Count total runs for a parent session
    #[must_use]
    pub fn count_for_parent(&self, parent_session_key: &str) -> usize {
        self.runs
            .values()
            .filter(|run| run.parent_session_key == parent_session_key)
            .count()
    }

    /// Get all runs for a parent session
    #[must_use]
    pub fn get_for_parent(&self, parent_session_key: &str) -> Vec<&SubagentRun> {
        self.runs
            .values()
            .filter(|run| run.parent_session_key == parent_session_key)
            .collect()
    }

    /// Get active runs for a parent session
    #[must_use]
    pub fn get_active_for_parent(&self, parent_session_key: &str) -> Vec<&SubagentRun> {
        self.runs
            .values()
            .filter(|run| run.parent_session_key == parent_session_key && !run.status.is_terminal())
            .collect()
    }

    /// Get the maximum depth of runs for a parent
    #[must_use]
    pub fn get_max_depth_for_parent(&self, parent_session_key: &str) -> u32 {
        self.runs
            .values()
            .filter(|run| run.parent_session_key == parent_session_key)
            .map(|run| run.depth)
            .max()
            .unwrap_or(0)
    }

    /// List all runs (for debugging)
    #[must_use]
    pub fn list_all(&self) -> Vec<&SubagentRun> {
        self.runs.values().collect()
    }

    /// Get total number of registered runs
    #[must_use]
    pub fn len(&self) -> usize {
        self.runs.len()
    }

    /// Check if registry is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Clean up terminal runs older than a given duration
    pub fn cleanup_old(&mut self, max_age: chrono::Duration) -> usize {
        let now = Utc::now();
        let to_remove: Vec<String> = self
            .runs
            .values()
            .filter(|run| {
                run.status.is_terminal()
                    && run
                        .completed_at
                        .is_some_and(|t| now.signed_duration_since(t) > max_age)
            })
            .map(|run| run.run_id.clone())
            .collect();

        let count = to_remove.len();
        for run_id in to_remove {
            self.runs.remove(&run_id);
        }

        if count > 0 {
            tracing::info!("Cleaned up {} old subagent runs from registry", count);
        }
        count
    }
}

/// Thread-safe wrapper for `SubagentRegistry`
pub type SharedSubagentRegistry = Arc<RwLock<SubagentRegistry>>;

/// Create a new shared registry
#[must_use]
pub fn create_shared_registry() -> SharedSubagentRegistry {
    Arc::new(RwLock::new(SubagentRegistry::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subagent_status_is_terminal() {
        assert!(!SubagentStatus::Running.is_terminal());
        assert!(SubagentStatus::Completed.is_terminal());
        assert!(SubagentStatus::Failed.is_terminal());
        assert!(SubagentStatus::Cancelled.is_terminal());
        assert!(SubagentStatus::TimedOut.is_terminal());
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut registry = SubagentRegistry::new();
        let run = SubagentRun::new(
            "run_1".to_string(),
            "child_key".to_string(),
            "parent_key".to_string(),
            "Test task".to_string(),
            crate::session::types::SpawnCleanupPolicy::Keep,
            None,
            1,
        );

        registry.register(run);
        assert_eq!(registry.len(), 1);

        let retrieved = registry.get("run_1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().task, "Test task");
    }

    #[test]
    fn test_registry_count_active() {
        let mut registry = SubagentRegistry::new();

        // Register a running run
        let run1 = SubagentRun::new(
            "run_1".to_string(),
            "child_1".to_string(),
            "parent_key".to_string(),
            "Task 1".to_string(),
            crate::session::types::SpawnCleanupPolicy::Keep,
            None,
            1,
        );
        registry.register(run1);

        // Register a completed run
        let mut run2 = SubagentRun::new(
            "run_2".to_string(),
            "child_2".to_string(),
            "parent_key".to_string(),
            "Task 2".to_string(),
            crate::session::types::SpawnCleanupPolicy::Keep,
            None,
            1,
        );
        run2.status = SubagentStatus::Completed;
        registry.register(run2);

        assert_eq!(registry.count_active_for_parent("parent_key"), 1);
        assert_eq!(registry.count_for_parent("parent_key"), 2);
    }

    #[test]
    fn test_subagent_run_complete() {
        let mut run = SubagentRun::new(
            "run_1".to_string(),
            "child_key".to_string(),
            "parent_key".to_string(),
            "Task".to_string(),
            crate::session::types::SpawnCleanupPolicy::Keep,
            None,
            1,
        );

        assert_eq!(run.status, SubagentStatus::Running);
        assert!(run.completed_at.is_none());

        let result = SubagentResult {
            status: SubagentStatus::Completed,
            output: Some("Success".to_string()),
            error: None,
            token_usage: Some((10, 20, 30)),
            completed_at: Utc::now(),
        };

        run.complete(result);

        assert_eq!(run.status, SubagentStatus::Completed);
        assert!(run.completed_at.is_some());
        assert!(run.result.is_some());
    }
}
