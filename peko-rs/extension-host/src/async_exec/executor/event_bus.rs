//! Event bus for delivering async task completions to agents

use super::types::AsyncTaskId;
use anyhow::Result;

/// Event sent to agent when an async task completes
#[derive(Debug, Clone)]
pub struct AsyncTaskCompletionEvent {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub result_message: String,
    pub parent_session_key: String,
    pub label: Option<String>,
}

/// Event bus for delivering async task completions to agents
#[derive(Debug, Clone)]
pub struct AsyncTaskEventBus {
    /// Sender for events - agents subscribe to receive events
    sender: tokio::sync::mpsc::UnboundedSender<AsyncTaskCompletionEvent>,
}

impl AsyncTaskEventBus {
    #[must_use]
    pub fn new() -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<AsyncTaskCompletionEvent>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }

    pub fn publish(&self, event: AsyncTaskCompletionEvent) -> Result<()> {
        self.sender
            .send(event)
            .map_err(|_| anyhow::anyhow!("Failed to send async task event - no listeners"))
    }
}
