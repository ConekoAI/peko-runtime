//! Session inbox completion events and steering messages.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Event pushed to the inbox when an async task reaches a terminal
/// state. The agentic loop drains these at iteration start and
/// synthesizes a single user-role message containing all of them.
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub task_id: String,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: peko_tools_core::AsyncTaskStatus,
    pub completed_at: DateTime<Utc>,
    pub output_path: std::path::PathBuf,
    pub parent_session_key: String,
}

/// User-supplied message queued for delivery to a session at the
/// start of the next agentic loop iteration.
#[derive(Debug, Clone)]
pub struct SteeringMessage {
    pub id: Uuid,
    pub content: String,
    pub queued_at: DateTime<Utc>,
}

impl SteeringMessage {
    /// Construct a steering message with a freshly generated id and
    /// the current UTC timestamp.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            content: content.into(),
            queued_at: Utc::now(),
        }
    }
}

/// Item carried in a session inbox. Either a user steering message
/// or a completion event from a background async task.
#[derive(Debug, Clone)]
pub enum InboxItem {
    Steering(SteeringMessage),
    Completion(CompletionEvent),
}

impl From<CompletionEvent> for InboxItem {
    fn from(e: CompletionEvent) -> Self {
        InboxItem::Completion(e)
    }
}

impl From<SteeringMessage> for InboxItem {
    fn from(m: SteeringMessage) -> Self {
        InboxItem::Steering(m)
    }
}

impl From<CompletionEvent> for crate::CompletionEnvelope {
    fn from(e: CompletionEvent) -> Self {
        crate::CompletionEnvelope {
            task_id: e.task_id,
            tool_name: e.tool_name,
            result: e.result,
            status: e.status,
            completed_at: e.completed_at,
            output_path: e.output_path,
            parent_session_key: e.parent_session_key,
        }
    }
}

impl From<SteeringMessage> for crate::SteeringEnvelope {
    fn from(m: SteeringMessage) -> Self {
        crate::SteeringEnvelope {
            id: m.id,
            content: m.content,
            queued_at: m.queued_at,
        }
    }
}

impl From<CompletionEvent> for crate::AsyncInboxItem {
    fn from(e: CompletionEvent) -> Self {
        crate::AsyncInboxItem::Completion(crate::CompletionEnvelope::from(e))
    }
}

impl From<SteeringMessage> for crate::AsyncInboxItem {
    fn from(m: SteeringMessage) -> Self {
        crate::AsyncInboxItem::Steering(crate::SteeringEnvelope::from(m))
    }
}

impl From<InboxItem> for crate::AsyncInboxItem {
    fn from(item: InboxItem) -> Self {
        match item {
            InboxItem::Completion(e) => e.into(),
            InboxItem::Steering(m) => m.into(),
        }
    }
}
