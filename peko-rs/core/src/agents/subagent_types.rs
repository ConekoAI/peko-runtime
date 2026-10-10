//! Subagent domain types
//!
//! These types provide a subagent-specific view over the unified
//! `AsyncTaskEntry` data model. No registry storage uses these types
//! directly — they are read-only projections constructed on demand.

use crate::async_exec::executor::{AsyncTaskEntry, TaskMetadata};
use chrono::Utc;

pub use crate::tools::builtin::messaging::SubagentRunView;

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
        })
    }

    /// Get duration of the run
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        let end = self.completed_at.unwrap_or_else(Utc::now);
        Some(end.signed_duration_since(self.started_at))
    }
}
