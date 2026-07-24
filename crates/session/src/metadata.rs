//! Session Metadata Value Object
//!
//! This module provides an immutable value object for session metadata,
//! ensuring controlled updates and clear data flow.
//!
//! All metadata mutations go through the `MetadataController`, which is the
//! sole authority for session metadata operations.

use crate::index::SessionEntry;
use std::time::{SystemTime, UNIX_EPOCH};

/// Immutable session metadata
///
/// This is a value object that represents a snapshot of session metadata.
/// To modify metadata, create a new instance and pass it to `MetadataController`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    pub session_id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub turn_count: u32,
    /// `total_tokens` reported by the most recent assistant message.
    /// This is the model's count of *how many tokens the current turn
    /// used* — it is NOT the model's maximum context window.
    pub last_total_tokens: usize,
    /// Cumulative input tokens across all assistant messages
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    pub total_output_tokens: usize,
    /// The model's maximum context window size, in tokens, if known.
    /// `None` when the session has not yet been opened against a
    /// known provider/model — e.g. legacy entries, sessions opened
    /// without a provider reference. Populated by the engine when
    /// the orchestrator pins the registry-resolved model max.
    pub model_context_limit: Option<usize>,
    pub transcript_file: String,
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    pub trigger: String,
    /// Subject type ("user" or "agent")
    pub peer_type: Option<String>,
    /// Subject ID
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
            last_total_tokens: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            model_context_limit: None,
            transcript_file: transcript_file.into(),
            title: None,
            parent_session_id: None,
            trigger: "user".to_string(),
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

    /// Create from existing `SessionEntry` (index data)
    #[must_use]
    pub fn from_entry(entry: SessionEntry) -> Self {
        Self {
            session_id: entry.session_id,
            agent_name: entry.agent_name,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            message_count: entry.message_count,
            turn_count: entry.turn_count,
            last_total_tokens: entry.last_total_tokens,
            total_input_tokens: entry.total_input_tokens,
            total_output_tokens: entry.total_output_tokens,
            model_context_limit: entry.model_context_limit,
            transcript_file: entry.transcript_file,
            title: entry.title,
            parent_session_id: entry.parent_session_id,
            trigger: entry.trigger,
            peer_type: entry.peer_type,
            peer_id: entry.peer_id,
        }
    }

    /// Convert to `SessionEntry` for index storage
    #[must_use]
    pub fn to_entry(self) -> SessionEntry {
        SessionEntry {
            session_id: self.session_id,
            agent_name: self.agent_name,
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_count: self.message_count,
            turn_count: self.turn_count,
            last_total_tokens: self.last_total_tokens,
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            model_context_limit: self.model_context_limit,
            transcript_file: self.transcript_file,
            title: self.title,
            parent_session_id: self.parent_session_id,
            trigger: self.trigger,
            peer_type: self.peer_type,
            peer_id: self.peer_id,
        }
    }

    /// Update timestamp to now
    fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
    }

    /// Record token usage for the most recent assistant message.
    ///
    /// `last_total_tokens` is the `total_tokens` reported by the
    /// provider on the last assistant turn. `input` and `output` are
    /// the incremental tokens for this turn.
    pub fn record_tokens(&mut self, last_total_tokens: usize, input: usize, output: usize) {
        self.last_total_tokens = last_total_tokens;
        self.total_input_tokens += input;
        self.total_output_tokens += output;
        self.touch();
    }

    /// Set the model's maximum context window size (in tokens).
    ///
    /// Called by the engine when the compaction orchestrator pins the
    /// registry-resolved model max. Idempotent; calling with a different
    /// value overwrites the previous one.
    pub fn set_model_context_limit(&mut self, limit: usize) {
        if self.model_context_limit != Some(limit) {
            self.model_context_limit = Some(limit);
            self.touch();
        }
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

    /// Set title
    pub fn set_title(&mut self, title: Option<impl Into<String>>) {
        self.title = title.map(Into::into);
        self.touch();
    }

    /// Set trigger
    pub fn set_trigger(&mut self, trigger: impl Into<String>) {
        self.trigger = trigger.into();
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

    #[must_use]
    pub fn reconciled(mut self, old_count: usize, new_count: usize) -> Self {
        self.was_reconciled = true;
        self.old_message_count = old_count;
        self.new_message_count = new_count;
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn test_metadata_new() {
        let meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        assert_eq!(meta.session_id, "sess_123");
        assert_eq!(meta.agent_name, "test_agent");
        assert_eq!(meta.message_count, 0);
    }

    #[test]
    fn test_metadata_mutation() {
        // Use mutable methods instead of builder pattern
        let mut meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        meta.set_title(Some("Test Title"));
        meta.set_message_count(10);
        // record_tokens(last_total_tokens, input_tokens, output_tokens)
        meta.record_tokens(1000, 100, 50);
        meta.set_model_context_limit(200_000);

        assert_eq!(meta.title, Some("Test Title".to_string()));
        assert_eq!(meta.message_count, 10);
        assert_eq!(meta.last_total_tokens, 1000);
        assert_eq!(meta.total_input_tokens, 100);
        assert_eq!(meta.total_output_tokens, 50);
        assert_eq!(meta.model_context_limit, Some(200_000));
    }

    #[test]
    fn test_metadata_context_limit_unknown_by_default() {
        let meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        assert_eq!(meta.model_context_limit, None);
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
