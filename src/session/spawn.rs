//! Spawn overlay implementation
//!
//! Provides isolated or inherited sessions for subagent spawning.
//! Spawn overlays enable parallel task execution with configurable
//! context inheritance and lifecycle policies.

use super::overlay::SessionOverlay;
use super::types::{OverlayType, SpawnCleanupPolicy};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use peko_auth::Subject;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Spawn overlay for subagent task isolation
///
/// A spawn overlay creates an isolated execution context for a subagent.
/// It can either:
/// - Inherit the parent's base session context (isolated=false)
/// - Create a completely isolated session (isolated=true)
#[derive(Debug, Clone)]
pub struct SpawnOverlay {
    /// Unique spawn ID
    pub spawn_id: String,
    /// Parent base session key
    pub base_session_key: String,
    /// The peer this overlay belongs to (may be synthetic for isolated spawns)
    pub peer: Subject,
    /// Parent session key (for result routing)
    pub parent_session_key: String,
    /// Task description
    pub task_description: String,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Start time (when spawn actually begins execution)
    pub started_at: Option<DateTime<Utc>>,
    /// End time (when spawn completes)
    pub ended_at: Option<DateTime<Utc>>,
    /// If true, spawn doesn't inherit base context
    pub isolated: bool,
    /// Run timeout in seconds
    pub timeout_seconds: Option<u64>,
    /// Cleanup policy
    pub cleanup: SpawnCleanupPolicy,
    /// Spawn depth (for limiting nesting)
    pub depth: u32,
    /// Run ID assigned by the execution engine
    pub run_id: Option<String>,
    /// Spawn status
    pub status: SpawnStatus,
    /// Result data (if completed)
    pub result: Option<SpawnResult>,
}

/// Status of a spawn overlay
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SpawnStatus {
    /// Spawn created but not yet started
    #[default]
    Created,
    /// Spawn is running
    Running,
    /// Spawn completed successfully
    Completed,
    /// Spawn failed
    Failed,
    /// Spawn was cancelled/timed out
    Cancelled,
}

impl SpawnStatus {
    /// Get the status as a string
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            SpawnStatus::Created => "created",
            SpawnStatus::Running => "running",
            SpawnStatus::Completed => "completed",
            SpawnStatus::Failed => "failed",
            SpawnStatus::Cancelled => "cancelled",
        }
    }

    /// Check if the spawn is in a terminal state
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            SpawnStatus::Completed | SpawnStatus::Failed | SpawnStatus::Cancelled
        )
    }

    /// Check if the spawn is active
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, SpawnStatus::Created | SpawnStatus::Running)
    }
}

/// Result of a completed spawn
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResult {
    /// Success or failure
    pub success: bool,
    /// Output/result data
    pub output: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
    /// Exit code or status
    pub status: String,
}

impl SpawnOverlay {
    /// Create a new spawn overlay
    ///
    /// # Arguments
    /// * `base_session_key` - The base session key (may be new for isolated spawns)
    /// * `peer` - The peer this overlay belongs to
    /// * `parent_session_key` - The parent's session key for result routing
    /// * `task` - Description of the task to execute
    /// * `isolated` - If true, doesn't inherit parent's conversation context
    pub fn new(
        base_session_key: impl Into<String>,
        peer: Subject,
        parent_session_key: impl Into<String>,
        task: impl Into<String>,
        isolated: bool,
    ) -> Self {
        let spawn_id = format!("spawn_{}", Uuid::new_v4());

        Self {
            spawn_id: spawn_id.clone(),
            base_session_key: base_session_key.into(),
            peer,
            parent_session_key: parent_session_key.into(),
            task_description: task.into(),
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            isolated,
            timeout_seconds: None,
            cleanup: SpawnCleanupPolicy::default(),
            depth: 0,
            run_id: None,
            status: SpawnStatus::Created,
            result: None,
        }
    }

    /// Create with a specific spawn ID (for deserialization)
    pub fn with_id(
        spawn_id: impl Into<String>,
        base_session_key: impl Into<String>,
        peer: Subject,
        parent_session_key: impl Into<String>,
        task: impl Into<String>,
        isolated: bool,
    ) -> Self {
        Self {
            spawn_id: spawn_id.into(),
            base_session_key: base_session_key.into(),
            peer,
            parent_session_key: parent_session_key.into(),
            task_description: task.into(),
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            isolated,
            timeout_seconds: None,
            cleanup: SpawnCleanupPolicy::default(),
            depth: 0,
            run_id: None,
            status: SpawnStatus::Created,
            result: None,
        }
    }

    /// Set the timeout
    #[must_use]
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout_seconds = Some(seconds);
        self
    }

    /// Set the cleanup policy
    #[must_use]
    pub fn with_cleanup(mut self, cleanup: SpawnCleanupPolicy) -> Self {
        self.cleanup = cleanup;
        self
    }

    /// Set the spawn depth
    #[must_use]
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Set the run ID
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// Mark the spawn as started
    pub fn mark_started(&mut self, run_id: impl Into<String>) {
        self.status = SpawnStatus::Running;
        self.started_at = Some(Utc::now());
        self.run_id = Some(run_id.into());
    }

    /// Mark the spawn as completed
    pub fn mark_completed(&mut self, output: impl Into<String>) {
        self.status = SpawnStatus::Completed;
        self.ended_at = Some(Utc::now());
        self.result = Some(SpawnResult {
            success: true,
            output: Some(output.into()),
            error: None,
            status: "completed".to_string(),
        });
    }

    /// Mark the spawn as failed
    pub fn mark_failed(&mut self, error: impl Into<String>) {
        self.status = SpawnStatus::Failed;
        self.ended_at = Some(Utc::now());
        self.result = Some(SpawnResult {
            success: false,
            output: None,
            error: Some(error.into()),
            status: "failed".to_string(),
        });
    }

    /// Mark the spawn as cancelled
    pub fn mark_cancelled(&mut self, reason: impl Into<String>) {
        self.status = SpawnStatus::Cancelled;
        self.ended_at = Some(Utc::now());
        self.result = Some(SpawnResult {
            success: false,
            output: None,
            error: Some(reason.into()),
            status: "cancelled".to_string(),
        });
    }

    /// Get the duration if the spawn has ended
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        match (self.started_at, self.ended_at) {
            (Some(start), Some(end)) => Some(end - start),
            _ => None,
        }
    }

    /// Get elapsed time since creation
    #[must_use]
    pub fn elapsed(&self) -> chrono::Duration {
        Utc::now() - self.created_at
    }

    /// Check if this spawn should be cleaned up
    #[must_use]
    pub fn should_cleanup(&self) -> bool {
        match self.cleanup {
            SpawnCleanupPolicy::Delete => self.status.is_terminal(),
            SpawnCleanupPolicy::Keep => false,
        }
    }

    /// Create from stored data (for deserialization)
    pub fn from_stored(
        spawn_id: impl Into<String>,
        base_session_key: impl Into<String>,
        peer: Subject,
        parent_session_key: impl Into<String>,
        task_description: impl Into<String>,
        created_at: DateTime<Utc>,
        started_at: Option<DateTime<Utc>>,
        ended_at: Option<DateTime<Utc>>,
        isolated: bool,
        timeout_seconds: Option<u64>,
        cleanup: SpawnCleanupPolicy,
        depth: u32,
        run_id: Option<String>,
        status: SpawnStatus,
        result: Option<SpawnResult>,
    ) -> Self {
        Self {
            spawn_id: spawn_id.into(),
            base_session_key: base_session_key.into(),
            peer,
            parent_session_key: parent_session_key.into(),
            task_description: task_description.into(),
            created_at,
            started_at,
            ended_at,
            isolated,
            timeout_seconds,
            cleanup,
            depth,
            run_id,
            status,
            result,
        }
    }
}

#[async_trait]
impl SessionOverlay for SpawnOverlay {
    fn overlay_type(&self) -> OverlayType {
        OverlayType::Spawn
    }

    fn overlay_id(&self) -> &str {
        &self.spawn_id
    }

    fn persist(&self) -> bool {
        // Spawn overlays follow the cleanup policy
        // They persist if the policy is Keep or if still active
        match self.cleanup {
            SpawnCleanupPolicy::Keep => true,
            SpawnCleanupPolicy::Delete => !self.status.is_terminal(),
        }
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "type": "spawn",
            "spawn_id": self.spawn_id,
            "base_session_key": self.base_session_key,
            "peer": self.peer,
            "parent_session_key": self.parent_session_key,
            "task_description": self.task_description,
            "created_at": self.created_at,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
            "isolated": self.isolated,
            "timeout_seconds": self.timeout_seconds,
            "cleanup": self.cleanup.as_str(),
            "depth": self.depth,
            "run_id": self.run_id,
            "status": self.status.as_str(),
            "result": self.result,
        })
    }

    fn base_session_key(&self) -> &str {
        &self.base_session_key
    }

    fn peer(&self) -> &Subject {
        &self.peer
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

/// Serializable representation of a spawn overlay for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnOverlayData {
    pub spawn_id: String,
    pub base_session_key: String,
    pub peer: Subject,
    pub parent_session_key: String,
    pub task_description: String,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub isolated: bool,
    pub timeout_seconds: Option<u64>,
    pub cleanup: SpawnCleanupPolicy,
    pub depth: u32,
    pub run_id: Option<String>,
    pub status: SpawnStatus,
    pub result: Option<SpawnResult>,
}

impl From<SpawnOverlay> for SpawnOverlayData {
    fn from(overlay: SpawnOverlay) -> Self {
        Self {
            spawn_id: overlay.spawn_id,
            base_session_key: overlay.base_session_key,
            peer: overlay.peer,
            parent_session_key: overlay.parent_session_key,
            task_description: overlay.task_description,
            created_at: overlay.created_at,
            started_at: overlay.started_at,
            ended_at: overlay.ended_at,
            isolated: overlay.isolated,
            timeout_seconds: overlay.timeout_seconds,
            cleanup: overlay.cleanup,
            depth: overlay.depth,
            run_id: overlay.run_id,
            status: overlay.status,
            result: overlay.result,
        }
    }
}

impl From<SpawnOverlayData> for SpawnOverlay {
    fn from(data: SpawnOverlayData) -> Self {
        Self {
            spawn_id: data.spawn_id,
            base_session_key: data.base_session_key,
            peer: data.peer,
            parent_session_key: data.parent_session_key,
            task_description: data.task_description,
            created_at: data.created_at,
            started_at: data.started_at,
            ended_at: data.ended_at,
            isolated: data.isolated,
            timeout_seconds: data.timeout_seconds,
            cleanup: data.cleanup,
            depth: data.depth,
            run_id: data.run_id,
            status: data.status,
            result: data.result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_overlay_new() {
        let overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "agent:test:peer:user:alice:overlay:cli:default",
            "Research async patterns",
            false,
        );

        assert!(overlay.spawn_id.starts_with("spawn_"));
        assert_eq!(overlay.base_session_key, "agent:test:peer:user:alice");
        assert_eq!(
            overlay.parent_session_key,
            "agent:test:peer:user:alice:overlay:cli:default"
        );
        assert_eq!(overlay.task_description, "Research async patterns");
        assert!(!overlay.isolated);
        assert_eq!(overlay.status, SpawnStatus::Created);
        assert!(overlay.persist()); // Created = not terminal, so persists
    }

    #[test]
    fn test_spawn_overlay_with_options() {
        let overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            true, // isolated
        )
        .with_timeout(300)
        .with_cleanup(SpawnCleanupPolicy::Delete)
        .with_depth(1)
        .with_run_id("run_123");

        assert!(overlay.isolated);
        assert_eq!(overlay.timeout_seconds, Some(300));
        assert_eq!(overlay.cleanup, SpawnCleanupPolicy::Delete);
        assert_eq!(overlay.depth, 1);
        assert_eq!(overlay.run_id, Some("run_123".to_string()));
    }

    #[test]
    fn test_spawn_status_lifecycle() {
        let mut overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        );

        assert_eq!(overlay.status, SpawnStatus::Created);
        assert!(overlay.status.is_active());
        assert!(!overlay.status.is_terminal());

        overlay.mark_started("run_123");
        assert_eq!(overlay.status, SpawnStatus::Running);
        assert!(overlay.status.is_active());
        assert!(overlay.started_at.is_some());

        overlay.mark_completed("Task completed successfully");
        assert_eq!(overlay.status, SpawnStatus::Completed);
        assert!(!overlay.status.is_active());
        assert!(overlay.status.is_terminal());
        assert!(overlay.ended_at.is_some());
        assert!(overlay.result.is_some());
        assert!(overlay.result.as_ref().unwrap().success);
    }

    #[test]
    fn test_spawn_mark_failed() {
        let mut overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        );

        overlay.mark_started("run_123");
        overlay.mark_failed("Something went wrong");

        assert_eq!(overlay.status, SpawnStatus::Failed);
        assert!(overlay.result.is_some());
        assert!(!overlay.result.as_ref().unwrap().success);
        assert_eq!(
            overlay.result.as_ref().unwrap().error,
            Some("Something went wrong".to_string())
        );
    }

    #[test]
    fn test_spawn_mark_cancelled() {
        let mut overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        );

        overlay.mark_cancelled("Timeout exceeded");

        assert_eq!(overlay.status, SpawnStatus::Cancelled);
        assert!(overlay.result.is_some());
        assert!(!overlay.result.as_ref().unwrap().success);
    }

    #[test]
    fn test_spawn_duration() {
        let mut overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        );

        // Not started, no duration
        assert!(overlay.duration().is_none());

        // Started but not ended
        overlay.mark_started("run_123");
        assert!(overlay.duration().is_none());

        // Completed
        overlay.mark_completed("Done");
        assert!(overlay.duration().is_some());
    }

    #[test]
    fn test_should_cleanup() {
        // Delete policy + terminal = cleanup
        let mut overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        )
        .with_cleanup(SpawnCleanupPolicy::Delete);

        assert!(!overlay.should_cleanup()); // Not terminal yet

        overlay.mark_completed("Done");
        assert!(overlay.should_cleanup()); // Now terminal

        // Keep policy = never cleanup
        let overlay2 = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        )
        .with_cleanup(SpawnCleanupPolicy::Keep);

        // Clone to avoid move
        let mut overlay2_completed = overlay2.clone();
        overlay2_completed.mark_completed("Done");
        assert!(!overlay2_completed.should_cleanup());
    }

    #[test]
    fn test_spawn_overlay_trait_impl() {
        let overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        );

        assert_eq!(overlay.overlay_type(), OverlayType::Spawn);
        assert!(overlay.overlay_id().starts_with("spawn_"));
        assert_eq!(overlay.base_session_key(), "agent:test:peer:user:alice");
        assert_eq!(overlay.peer().subject_id(), "alice");
    }

    #[test]
    fn test_spawn_to_json() {
        let overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            true,
        )
        .with_timeout(300);

        let json = overlay.to_json();

        assert_eq!(json["type"], "spawn");
        assert_eq!(json["task_description"], "Test task");
        assert_eq!(json["isolated"], true);
        assert_eq!(json["timeout_seconds"], 300);
        assert_eq!(json["cleanup"], "keep");
        assert_eq!(json["status"], "created");
    }

    #[test]
    fn test_spawn_serialization() {
        let overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        )
        .with_depth(2);

        // Convert to data struct and serialize
        let data: SpawnOverlayData = overlay.clone().into();
        let json = serde_json::to_string(&data).unwrap();

        // Deserialize back
        let data2: SpawnOverlayData = serde_json::from_str(&json).unwrap();
        let overlay2: SpawnOverlay = data2.into();

        assert_eq!(overlay.spawn_id, overlay2.spawn_id);
        assert_eq!(overlay.task_description, overlay2.task_description);
        assert_eq!(overlay.isolated, overlay2.isolated);
        assert_eq!(overlay.depth, overlay2.depth);
    }

    #[test]
    fn test_spawn_status_as_str() {
        assert_eq!(SpawnStatus::Created.as_str(), "created");
        assert_eq!(SpawnStatus::Running.as_str(), "running");
        assert_eq!(SpawnStatus::Completed.as_str(), "completed");
        assert_eq!(SpawnStatus::Failed.as_str(), "failed");
        assert_eq!(SpawnStatus::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn test_spawn_status_is_terminal() {
        assert!(!SpawnStatus::Created.is_terminal());
        assert!(!SpawnStatus::Running.is_terminal());
        assert!(SpawnStatus::Completed.is_terminal());
        assert!(SpawnStatus::Failed.is_terminal());
        assert!(SpawnStatus::Cancelled.is_terminal());
    }

    #[test]
    fn test_elapsed() {
        let overlay = SpawnOverlay::new(
            "agent:test:peer:user:alice",
            Subject::User("alice".to_string()),
            "parent_key",
            "Test task",
            false,
        );

        // Small delay
        std::thread::sleep(std::time::Duration::from_millis(10));

        let elapsed = overlay.elapsed();
        assert!(elapsed.num_milliseconds() >= 10);
    }
}
