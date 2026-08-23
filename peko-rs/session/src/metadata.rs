//! Session Metadata Value Object
//!
//! B8c.1: `SessionMetadata` is now a type alias for [`SessionEntry`]
//! (`crate::index::SessionEntry`). The two structs carried identical
//! 21 fields and a hand-rolled `from_entry`/`to_entry` pair was used to
//! bridge them at API boundaries. With the alias, `SessionMetadata` is
//! the same type as `SessionEntry` — `to_metadata` / `to_entry` and the
//! duplicate `new` / `record_tokens` / `set_*` / `increment_turn`
//! methods all collapse onto the canonical definitions in
//! `crate::index`.
//!
//! `MetadataDiscrepancy` and `ReconciliationResult` remain here:
//! they describe cross-source (index vs JSONL) reconciliation outcomes
//! and are not part of the per-session value object.

use crate::index::SessionEntry;

/// Backward-compatible alias for [`SessionEntry`].
///
/// `SessionMetadata` predates `SessionEntry`'s full role as the
/// single per-session value object. Existing call sites
/// (`SessionMetadata::new(...)`, `metadata.set_message_count(...)`,
/// `metadata.to_entry()`, etc.) continue to compile and behave
/// identically because they all resolve to the same underlying
/// `SessionEntry` impl block.
///
/// New code should prefer the canonical name `SessionEntry`.
pub type SessionMetadata = SessionEntry;

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
    use super::*;
    use crate::id::SessionId;

    // B8c.1: alias exists; constructors and accessors resolve to
    // `SessionEntry`. These tests guard the alias shape rather than
    // exercising the underlying behavior (which is covered by
    // `crate::index` tests).

    #[test]
    fn test_metadata_alias_is_session_entry() {
        let meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        let entry = SessionEntry::new("sess_123", "test_agent", "sess_123.jsonl");
        assert_eq!(meta.session_id, entry.session_id);
        assert_eq!(meta.agent_name, entry.agent_name);
    }

    #[test]
    fn test_metadata_mutation_through_alias() {
        let mut meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        meta.set_title(Some("Test Title"));
        meta.set_message_count(10);
        meta.record_tokens(1000, 100, 50);
        meta.set_model_context_limit(200_000);

        assert_eq!(meta.title.as_deref(), Some("Test Title"));
        assert_eq!(meta.message_count, 10);
        assert_eq!(meta.last_total_tokens, 1000);
        assert_eq!(meta.total_input_tokens, 100);
        assert_eq!(meta.total_output_tokens, 50);
        assert_eq!(meta.model_context_limit, Some(200_000));
    }

    #[test]
    fn test_archive_flags_via_alias() {
        let mut meta = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        assert!(!meta.archived);
        assert!(!meta.compact_requested);
        assert!(!meta.standing);
        assert!(!meta.privileged);

        meta.archived = true;
        meta.compact_requested = true;
        meta.standing = true;
        meta.privileged = true;
        assert!(meta.archived);
        assert!(meta.compact_requested);
        assert!(meta.standing);
        assert!(meta.privileged);
    }

    #[test]
    fn test_slug_via_alias() {
        let mut meta = SessionMetadata::new("sess_456", "test_agent", "sess_456.jsonl");
        assert_eq!(meta.slug, None);
        meta.set_slug(Some("memory"));
        assert_eq!(meta.slug.as_deref(), Some("memory"));
        meta.set_slug(None::<String>);
        assert_eq!(meta.slug, None);
    }

    #[test]
    fn test_with_parent_via_alias() {
        // `with_parent` is provided by `SessionEntry`; the alias
        // surfaces it on `SessionMetadata` calls without an extra
        // method.
        let parent = SessionId::from("parent");
        let child = SessionMetadata::with_parent(
            SessionId::from("child"),
            "test_agent",
            "child.jsonl",
            parent.clone(),
        );
        assert_eq!(child.parent_session_id, Some(parent));
        assert_eq!(child.trigger, "branch");
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