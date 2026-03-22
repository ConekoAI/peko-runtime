//! Session Metadata Value Object
//!
//! This module provides an immutable value object for session metadata,
//! ensuring controlled updates and clear data flow.
//!
//! All metadata mutations go through the MetadataController, which is the
//! sole authority for session metadata operations.

use crate::session::index::SessionEntry;
use std::time::{SystemTime, UNIX_EPOCH};

/// Immutable session metadata
///
/// This is a value object that represents a snapshot of session metadata.
/// To modify metadata, create a new instance and pass it to MetadataController.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    pub session_id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub turn_count: u32,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
    pub transcript_file: String,
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    pub ended: bool,
    pub trigger: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub channel: Option<String>,
    pub recipient: Option<String>,
    pub cwd: Option<String>,
    /// Peer type ("user" or "agent")
    pub peer_type: Option<String>,
    /// Peer ID
    pub peer_id: Option<String>,
}

impl SessionMetadata {
    /// Create new metadata for a session
    pub fn new(
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        transcript_file: impl Into<String>,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            session_id: session_id.into(),
            agent_name: agent_name.into(),
            created_at: now,
            updated_at: now,
            message_count: 0,
            turn_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            transcript_file: transcript_file.into(),
            title: None,
            parent_session_id: None,
            ended: false,
            trigger: "user".to_string(),
            provider: None,
            model: None,
            channel: None,
            recipient: None,
            cwd: None,
            peer_type: None,
            peer_id: None,
        }
    }

    /// Create metadata with parent session (for branching)
    pub fn with_parent(
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        transcript_file: impl Into<String>,
        parent_session_id: impl Into<String>,
    ) -> Self {
        let mut meta = Self::new(session_id, agent_name, transcript_file);
        meta.parent_session_id = Some(parent_session_id.into());
        meta.trigger = "branch".to_string();
        meta
    }

    /// Create from existing SessionEntry (index data)
    pub fn from_entry(entry: SessionEntry) -> Self {
        Self {
            session_id: entry.session_id,
            agent_name: entry.agent_name,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            message_count: entry.message_count,
            turn_count: entry.turn_count,
            input_tokens: entry.input_tokens,
            output_tokens: entry.output_tokens,
            total_tokens: entry.total_tokens,
            transcript_file: entry.transcript_file,
            title: entry.title,
            parent_session_id: entry.parent_session_id,
            ended: entry.ended,
            trigger: entry.trigger,
            provider: entry.provider,
            model: entry.model,
            channel: entry.channel,
            recipient: entry.recipient,
            cwd: entry.cwd,
            peer_type: entry.peer_type,
            peer_id: entry.peer_id,
        }
    }

    /// Convert to SessionEntry for index storage
    pub fn to_entry(self) -> SessionEntry {
        SessionEntry {
            session_id: self.session_id,
            agent_name: self.agent_name,
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_count: self.message_count,
            turn_count: self.turn_count,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.total_tokens,
            transcript_file: self.transcript_file,
            title: self.title,
            parent_session_id: self.parent_session_id,
            ended: self.ended,
            trigger: self.trigger,
            provider: self.provider,
            model: self.model,
            channel: self.channel,
            recipient: self.recipient,
            cwd: self.cwd,
            peer_type: self.peer_type,
            peer_id: self.peer_id,
        }
    }

    /// Create a builder for controlled mutation
    pub fn into_builder(self) -> SessionMetadataBuilder {
        SessionMetadataBuilder(self)
    }

    /// Update timestamp to now
    fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
    }

    /// Record token usage
    pub fn record_tokens(&mut self, input: usize, output: usize) {
        self.input_tokens += input;
        self.output_tokens += output;
        self.total_tokens = self.input_tokens + self.output_tokens;
        self.touch();
    }

    /// Set message count from computed value (reconciliation)
    pub fn set_message_count(&mut self, count: usize) {
        if self.message_count != count {
            tracing::debug!(
                "Updating message count for {}: {} -> {}",
                self.session_id,
                self.message_count,
                count
            );
            self.message_count = count;
            self.touch();
        }
    }

    /// Increment turn count
    pub fn increment_turn(&mut self) {
        self.turn_count += 1;
        self.touch();
    }

    /// Set model information
    pub fn set_model(&mut self, provider: impl Into<String>, model: impl Into<String>) {
        self.provider = Some(provider.into());
        self.model = Some(model.into());
        self.touch();
    }

    /// Set title
    pub fn set_title(&mut self, title: Option<impl Into<String>>) {
        self.title = title.map(Into::into);
        self.touch();
    }

    /// Mark as ended
    pub fn mark_ended(&mut self) {
        self.ended = true;
        self.touch();
    }

    /// Set trigger
    pub fn set_trigger(&mut self, trigger: impl Into<String>) {
        self.trigger = trigger.into();
    }

    /// Set working directory
    pub fn set_cwd(&mut self, cwd: Option<impl Into<String>>) {
        self.cwd = cwd.map(Into::into);
    }
}

/// Builder for controlled metadata mutation
///
/// Usage:
/// ```rust,ignore
/// let new_metadata = metadata
///     .into_builder()
///     .with_title("New Title")
///     .with_message_count(10)
///     .build();
/// ```
pub struct SessionMetadataBuilder(SessionMetadata);

impl SessionMetadataBuilder {
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.0.title = Some(title.into());
        self.0.touch();
        self
    }

    pub fn with_message_count(mut self, count: usize) -> Self {
        self.0.message_count = count;
        self.0.touch();
        self
    }

    pub fn with_tokens(mut self, input: usize, output: usize) -> Self {
        self.0.input_tokens = input;
        self.0.output_tokens = output;
        self.0.total_tokens = input + output;
        self.0.touch();
        self
    }

    pub fn with_model(mut self, provider: impl Into<String>, model: impl Into<String>) -> Self {
        self.0.provider = Some(provider.into());
        self.0.model = Some(model.into());
        self.0.touch();
        self
    }

    pub fn with_parent_session(mut self, parent_id: impl Into<String>) -> Self {
        self.0.parent_session_id = Some(parent_id.into());
        self
    }

    pub fn with_trigger(mut self, trigger: impl Into<String>) -> Self {
        self.0.trigger = trigger.into();
        self
    }

    pub fn ended(mut self, ended: bool) -> Self {
        self.0.ended = ended;
        self.0.touch();
        self
    }

    pub fn build(self) -> SessionMetadata {
        self.0
    }
}

/// Discrepancy between index and JSONL
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataDiscrepancy {
    pub field: String,
    pub index_value: String,
    pub jsonl_value: String,
}

/// Result of metadata reconciliation
#[derive(Debug, Clone)]
pub struct ReconciliationResult {
    pub session_id: String,
    pub was_reconciled: bool,
    pub discrepancies: Vec<MetadataDiscrepancy>,
    pub old_message_count: usize,
    pub new_message_count: usize,
}

impl ReconciliationResult {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            was_reconciled: false,
            discrepancies: Vec::new(),
            old_message_count: 0,
            new_message_count: 0,
        }
    }

    pub fn with_discrepancy(
        mut self,
        field: impl Into<String>,
        index_value: impl ToString,
        jsonl_value: impl ToString,
    ) -> Self {
        self.discrepancies.push(MetadataDiscrepancy {
            field: field.into(),
            index_value: index_value.to_string(),
            jsonl_value: jsonl_value.to_string(),
        });
        self
    }

    pub fn reconciled(mut self, old_count: usize, new_count: usize) -> Self {
        self.was_reconciled = true;
        self.old_message_count = old_count;
        self.new_message_count = new_count;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_new() {
        let meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        assert_eq!(meta.session_id, "sess_123");
        assert_eq!(meta.agent_name, "test_agent");
        assert_eq!(meta.message_count, 0);
        assert!(!meta.ended);
    }

    #[test]
    fn test_metadata_builder() {
        let meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl")
            .into_builder()
            .with_title("Test Title")
            .with_message_count(10)
            .with_tokens(100, 50)
            .build();

        assert_eq!(meta.title, Some("Test Title".to_string()));
        assert_eq!(meta.message_count, 10);
        assert_eq!(meta.input_tokens, 100);
        assert_eq!(meta.output_tokens, 50);
        assert_eq!(meta.total_tokens, 150);
    }

    #[test]
    fn test_metadata_roundtrip() {
        let entry = SessionEntry::new(
            "sess_123".to_string(),
            "test_agent".to_string(),
            "sess_123.jsonl".to_string(),
        );

        let meta = SessionMetadata::from_entry(entry.clone());
        let entry2 = meta.to_entry();

        assert_eq!(entry.session_id, entry2.session_id);
        assert_eq!(entry.agent_name, entry2.agent_name);
        assert_eq!(entry.message_count, entry2.message_count);
    }

    #[test]
    fn test_reconciliation_result() {
        let result = ReconciliationResult::new("sess_123")
            .with_discrepancy("message_count", 5, 10)
            .reconciled(5, 10);

        assert!(result.was_reconciled);
        assert_eq!(result.old_message_count, 5);
        assert_eq!(result.new_message_count, 10);
        assert_eq!(result.discrepancies.len(), 1);
    }
}
