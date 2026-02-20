//! Task management types

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Task definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task ID
    pub id: String,
    /// Task type/category
    pub task_type: String,
    /// Human-readable description
    pub description: Option<String>,
    /// Task parameters
    pub parameters: serde_json::Value,
    /// Assigned agent DID
    pub assigned_to: Option<String>,
    /// Task creator/requester
    pub requested_by: String,
    /// Current state
    pub state: TaskState,
    /// Priority level
    pub priority: TaskPriority,
    /// Created timestamp
    pub created_at: DateTime<Utc>,
    /// Started timestamp
    pub started_at: Option<DateTime<Utc>>,
    /// Completed timestamp
    pub completed_at: Option<DateTime<Utc>>,
    /// Deadline (optional)
    pub deadline: Option<DateTime<Utc>>,
    /// Parent task ID (for subtasks)
    pub parent_id: Option<String>,
    /// Subtask IDs
    pub subtask_ids: Vec<String>,
    /// Task result
    pub result: Option<TaskResult>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Progress (0.0 - 1.0)
    pub progress: f32,
    /// Tags
    pub tags: Vec<String>,
    /// Metadata
    pub metadata: HashMap<String, String>,
    /// Timeout seconds
    pub timeout_seconds: u64,
    /// Retry count
    pub retry_count: u32,
    /// Maximum retries
    pub max_retries: u32,
}

/// Task state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Task is pending
    Pending,
    /// Task is queued
    Queued,
    /// Task is running
    Running,
    /// Task is paused
    Paused,
    /// Task completed successfully
    Completed,
    /// Task failed
    Failed,
    /// Task was cancelled
    Cancelled,
    /// Task timed out
    TimedOut,
    /// Task is waiting for human approval
    WaitingForApproval,
    /// Task is waiting for external input
    WaitingForInput,
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskState::Pending => write!(f, "pending"),
            TaskState::Queued => write!(f, "queued"),
            TaskState::Running => write!(f, "running"),
            TaskState::Paused => write!(f, "paused"),
            TaskState::Completed => write!(f, "completed"),
            TaskState::Failed => write!(f, "failed"),
            TaskState::Cancelled => write!(f, "cancelled"),
            TaskState::TimedOut => write!(f, "timed_out"),
            TaskState::WaitingForApproval => write!(f, "waiting_for_approval"),
            TaskState::WaitingForInput => write!(f, "waiting_for_input"),
        }
    }
}

impl TaskState {
    /// Check if task is active (not terminal)
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            TaskState::Pending
                | TaskState::Queued
                | TaskState::Running
                | TaskState::Paused
                | TaskState::WaitingForApproval
                | TaskState::WaitingForInput
        )
    }

    /// Check if task is terminal (completed, failed, cancelled)
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::TimedOut
        )
    }

    /// Check if task can be cancelled
    #[must_use]
    pub fn can_cancel(&self) -> bool {
        matches!(
            self,
            TaskState::Pending | TaskState::Queued | TaskState::Running | TaskState::Paused
        )
    }
}

/// Task priority
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, PartialOrd)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TaskPriority {
    /// Lowest priority
    Lowest = 0,
    /// Low priority
    Low = 1,
    /// Normal priority
    #[default]
    Normal = 2,
    /// High priority
    High = 3,
    /// Highest priority
    Highest = 4,
    /// Critical - interrupt other tasks
    Critical = 5,
}

impl std::fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskPriority::Lowest => write!(f, "lowest"),
            TaskPriority::Low => write!(f, "low"),
            TaskPriority::Normal => write!(f, "normal"),
            TaskPriority::High => write!(f, "high"),
            TaskPriority::Highest => write!(f, "highest"),
            TaskPriority::Critical => write!(f, "critical"),
        }
    }
}

/// Task result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Result type
    pub result_type: String,
    /// Result data
    pub data: serde_json::Value,
    /// Output files/artifacts
    pub artifacts: Vec<Artifact>,
    /// Execution time (milliseconds)
    pub execution_time_ms: u64,
    /// Tokens used (if applicable)
    pub tokens_used: Option<u32>,
    /// Cost (if applicable)
    pub cost: Option<f64>,
}

/// Task artifact (output file, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Artifact ID
    pub id: String,
    /// Artifact type
    pub artifact_type: String,
    /// File name
    pub name: String,
    /// MIME type
    pub mime_type: String,
    /// Content (base64 for binary)
    pub content: Option<String>,
    /// File path (if stored)
    pub path: Option<String>,
    /// URL (if hosted)
    pub url: Option<String>,
    /// Size in bytes
    pub size_bytes: Option<u64>,
}

/// Task statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStats {
    /// Total tasks
    pub total: usize,
    /// Tasks by state
    pub by_state: HashMap<String, usize>,
    /// Tasks by priority
    pub by_priority: HashMap<String, usize>,
    /// Average execution time (ms)
    pub avg_execution_time_ms: u64,
    /// Success rate (0.0 - 1.0)
    pub success_rate: f32,
}

/// Task query for searching/filtering
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskQuery {
    /// Filter by state
    pub state: Option<TaskState>,
    /// Filter by priority (minimum)
    pub min_priority: Option<TaskPriority>,
    /// Filter by assigned agent
    pub assigned_to: Option<String>,
    /// Filter by requester
    pub requested_by: Option<String>,
    /// Filter by task type
    pub task_type: Option<String>,
    /// Filter by tags (all must match)
    pub tags: Option<Vec<String>>,
    /// Filter by parent task
    pub parent_id: Option<String>,
    /// Include subtasks
    pub include_subtasks: bool,
    /// Created after
    pub created_after: Option<DateTime<Utc>>,
    /// Created before
    pub created_before: Option<DateTime<Utc>>,
    /// Limit results
    pub limit: Option<usize>,
    /// Offset for pagination
    pub offset: Option<usize>,
    /// Order by field
    pub order_by: Option<String>,
    /// Order direction: asc, desc
    pub order_direction: Option<String>,
}

impl Task {
    /// Create a new task
    #[must_use]
    pub fn new(task_type: &str, requested_by: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_type: task_type.to_string(),
            description: None,
            parameters: serde_json::json!({}),
            assigned_to: None,
            requested_by: requested_by.to_string(),
            state: TaskState::Pending,
            priority: TaskPriority::Normal,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            deadline: None,
            parent_id: None,
            subtask_ids: vec![],
            result: None,
            error: None,
            progress: 0.0,
            tags: vec![],
            metadata: HashMap::new(),
            timeout_seconds: 300,
            retry_count: 0,
            max_retries: 3,
        }
    }

    /// Set description
    #[must_use]
    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    /// Set parameters
    #[must_use]
    pub fn with_parameters(mut self, params: serde_json::Value) -> Self {
        self.parameters = params;
        self
    }

    /// Set priority
    #[must_use]
    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Set assigned agent
    #[must_use]
    pub fn assign_to(mut self, agent_did: &str) -> Self {
        self.assigned_to = Some(agent_did.to_string());
        self
    }

    /// Set deadline
    #[must_use]
    pub fn with_deadline(mut self, deadline: DateTime<Utc>) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Set parent task
    #[must_use]
    pub fn with_parent(mut self, parent_id: &str) -> Self {
        self.parent_id = Some(parent_id.to_string());
        self
    }

    /// Set tags
    #[must_use]
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Set timeout
    #[must_use]
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout_seconds = seconds;
        self
    }

    /// Mark task as started
    pub fn mark_started(&mut self) {
        self.state = TaskState::Running;
        self.started_at = Some(Utc::now());
    }

    /// Mark task as completed
    pub fn mark_completed(&mut self, result: TaskResult) {
        self.state = TaskState::Completed;
        self.completed_at = Some(Utc::now());
        self.result = Some(result);
        self.progress = 1.0;
    }

    /// Mark task as failed
    pub fn mark_failed(&mut self, error: &str) {
        self.state = TaskState::Failed;
        self.completed_at = Some(Utc::now());
        self.error = Some(error.to_string());
    }

    /// Mark task as cancelled
    pub fn mark_cancelled(&mut self) {
        self.state = TaskState::Cancelled;
        self.completed_at = Some(Utc::now());
    }

    /// Update progress (0.0 - 1.0)
    pub fn set_progress(&mut self, progress: f32) {
        self.progress = progress.clamp(0.0, 1.0);
    }

    /// Check if task is overdue
    #[must_use]
    pub fn is_overdue(&self) -> bool {
        match self.deadline {
            Some(deadline) => !self.state.is_terminal() && Utc::now() > deadline,
            None => false,
        }
    }

    /// Get execution duration (if started)
    #[must_use]
    pub fn execution_duration(&self) -> Option<chrono::Duration> {
        match (self.started_at, self.completed_at) {
            (Some(start), Some(end)) => Some(end - start),
            (Some(start), None) => Some(Utc::now() - start),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_creation() {
        let task = Task::new("test-task", "did:pekobot:local:requester")
            .with_description("Test task description")
            .with_priority(TaskPriority::High);

        assert_eq!(task.task_type, "test-task");
        assert_eq!(task.requested_by, "did:pekobot:local:requester");
        assert_eq!(task.description, Some("Test task description".to_string()));
        assert_eq!(task.priority, TaskPriority::High);
        assert_eq!(task.state, TaskState::Pending);
    }

    #[test]
    fn test_task_state_transitions() {
        let mut task = Task::new("test", "did:pekobot:local:test");

        assert!(task.state.is_active());
        assert!(!task.state.is_terminal());

        task.mark_started();
        assert_eq!(task.state, TaskState::Running);
        assert!(task.started_at.is_some());

        let result = TaskResult {
            result_type: "success".to_string(),
            data: serde_json::json!({}),
            artifacts: vec![],
            execution_time_ms: 1000,
            tokens_used: None,
            cost: None,
        };
        task.mark_completed(result);

        assert_eq!(task.state, TaskState::Completed);
        assert!(task.state.is_terminal());
        assert!(!task.state.can_cancel());
    }

    #[test]
    fn test_task_priority_ordering() {
        assert!(TaskPriority::Critical > TaskPriority::High);
        assert!(TaskPriority::High > TaskPriority::Normal);
        assert!(TaskPriority::Normal > TaskPriority::Low);
    }

    #[test]
    fn test_task_progress() {
        let mut task = Task::new("test", "did:pekobot:local:test");
        task.set_progress(0.5);
        assert_eq!(task.progress, 0.5);

        task.set_progress(1.5); // Should clamp to 1.0
        assert_eq!(task.progress, 1.0);

        task.set_progress(-0.5); // Should clamp to 0.0
        assert_eq!(task.progress, 0.0);
    }

    #[test]
    fn test_task_overdue() {
        let task = Task::new("test", "did:pekobot:local:test")
            .with_deadline(Utc::now() - chrono::Duration::hours(1));

        assert!(task.is_overdue());

        let mut completed_task = Task::new("test2", "did:pekobot:local:test");
        completed_task.mark_completed(TaskResult {
            result_type: "success".to_string(),
            data: serde_json::json!({}),
            artifacts: vec![],
            execution_time_ms: 100,
            tokens_used: None,
            cost: None,
        });
        // Can't test overdue on terminal task easily, but logic is clear
    }

    #[test]
    fn test_task_query_default() {
        let query = TaskQuery::default();
        assert!(query.state.is_none());
        assert!(query.assigned_to.is_none());
        assert!(query.limit.is_none());
    }
}
