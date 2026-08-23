//! Subagent domain types
//!
//! These types provide a subagent-specific view over the unified
//! `AsyncTaskEntry` data model. No registry storage uses these types
//! directly — they are read-only projections constructed on demand.

use crate::extensions::framework::async_exec::executor::{
    AsyncTaskEntry, AsyncTaskStatus, SubagentResult, TaskMetadata,
};
use chrono::{DateTime, Utc};
use peko_session::types::SpawnCleanupPolicy;

/// A read-only view of an async task entry, projected into the
/// subagent domain model.
///
/// This is NOT stored anywhere — it is constructed on demand from
/// the unified registry's `AsyncTaskEntry`.
#[derive(Debug, Clone)]
pub struct SubagentRunView {
    pub run_id: String,
    pub child_session_key: String,
    /// The child's durable session id (UUID) — the form `session list`
    /// shows, Agent's `action = "resume"` consumes, and the metadata-
    /// chain depth check reads. `None` for legacy runs registered before
    /// this field existed. Spawn-registered runs store the overlay key
    /// in `child_session_key`; callers that need to chain a follow-up
    /// spawn against the same child should use THIS field as the new
    /// `parent_session_key`, not `child_session_key`.
    pub child_session_id: Option<String>,
    pub parent_session_key: String,
    pub task: String,
    pub status: AsyncTaskStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub cleanup: SpawnCleanupPolicy,
    pub label: Option<String>,
    pub result: Option<SubagentResult>,
    pub depth: u32,
    pub announce_completion: bool,
}

impl SubagentRunView {
    /// Project an `AsyncTaskEntry` into a `SubagentRunView`.
    ///
    /// Returns `None` if the entry does not have `TaskMetadata::Subagent`.
    #[must_use]
    pub fn from_entry(entry: &AsyncTaskEntry) -> Option<Self> {
        let meta = match &entry.metadata {
            TaskMetadata::Subagent(m) => m,
            _ => return None,
        };

        let task = entry
            .params
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Some(Self {
            run_id: entry.task_id.clone(),
            child_session_key: meta.child_session_key.clone(),
            child_session_id: meta.child_session_id.clone(),
            parent_session_key: entry.parent_session_key.clone(),
            task,
            status: entry.status.clone(),
            started_at: entry.created_at,
            completed_at: entry.completed_at,
            cleanup: meta.cleanup,
            label: entry.config.label.clone(),
            result: meta.subagent_result.clone(),
            depth: meta.depth,
            announce_completion: meta.announce_completion,
        })
    }

    /// Get duration of the run
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        let end = self.completed_at.unwrap_or_else(Utc::now);
        Some(end.signed_duration_since(self.started_at))
    }
}
