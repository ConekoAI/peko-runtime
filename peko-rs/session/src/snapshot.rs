//! Session state exposed to workspace observers.

use std::collections::HashMap;

/// Snapshot of session state
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// Session ID
    pub session_id: String,

    /// Number of messages in session
    pub message_count: usize,

    /// Current context window size (tokens)
    pub context_tokens: usize,

    /// Session metadata
    pub metadata: HashMap<String, serde_json::Value>,
}
