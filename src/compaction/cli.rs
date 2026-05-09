//! CLI Compaction Logic
//!
//! Domain-specific compaction logic for the `session compact` command.
//! This is NOT a generic service — it's the CLI flow that uses the `Compactor`.

use crate::compaction::{CompactionEntry, Compactor};
use crate::providers::MessageRole;
use crate::session::unified::Session;
use crate::types::message::{ContentBlock, LlmMessage};
use anyhow::Result;

/// Result of a CLI compaction operation
#[derive(Debug, Clone)]
pub struct CliCompactionResult {
    /// Messages after compaction (summary + kept messages)
    pub messages: Vec<LlmMessage>,
    /// Compaction entry for persistence
    pub entry: CompactionEntry,
    /// Tokens saved
    pub tokens_saved: usize,
}

/// Dry-run report for previewing compaction
#[derive(Debug, Clone)]
pub struct DryRunReport {
    pub estimated_tokens: usize,
    pub context_window: usize,
    pub percent: usize,
    pub message_count: usize,
    pub messages_to_compact: usize,
}

/// Session compactor for CLI operations
///
/// Encapsulates the CLI-specific compaction flow:
/// - dry-run preview
/// - truncation-based compaction (no LLM required for CLI)
/// - recording compaction events
pub struct SessionCompactor;

impl SessionCompactor {
    /// Create a new session compactor
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Perform a dry-run and return a preview report
    pub async fn dry_run(
        &self,
        session: &Session,
        _instruction: Option<String>,
    ) -> Result<DryRunReport> {
        let messages = session
            .load_context_fast()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load context: {e}"))?;

        let estimated_tokens = Compactor::estimate_tokens(&messages);
        let context_window = 128_000usize; // Default for dry-run display
        let percent = (estimated_tokens * 100) / context_window.max(1);
        let messages_to_compact = messages.len().saturating_sub(2);

        Ok(DryRunReport {
            estimated_tokens,
            context_window,
            percent,
            message_count: messages.len(),
            messages_to_compact,
        })
    }

    /// Compact a session using truncation-based summarization (no LLM required)
    ///
    /// This is the CLI implementation. A production implementation would:
    /// 1. Load the agent's provider configuration
    /// 2. Instantiate the Provider with the correct API key
    /// 3. Call `compactor.compact(&messages, &provider).await`
    pub async fn compact(
        &self,
        session: &mut Session,
        instruction: Option<String>,
    ) -> Result<CliCompactionResult> {
        let messages = session
            .load_context_fast()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load context: {e}"))?;

        let estimated_tokens = Compactor::estimate_tokens(&messages);

        // Simple truncation-based compaction for CLI (no LLM required)
        let (initial_system, conversation): (Vec<_>, Vec<_>) =
            if !messages.is_empty() && matches!(messages[0].role, MessageRole::System) {
                (vec![messages[0].clone()], messages[1..].to_vec())
            } else {
                (vec![], messages.clone())
            };

        // Keep last 4 messages, summarize the rest
        let keep_count = conversation.len().min(4);
        let split_point = conversation.len().saturating_sub(keep_count);
        let to_compact = &conversation[..split_point];
        let to_keep = &conversation[split_point..];

        let summary_text = if let Some(ref instr) = instruction {
            format!(
                "[Custom instruction: {instr}]\n\n[{} messages summarized]",
                to_compact.len()
            )
        } else {
            format!(
                "[{} messages summarized - conversation history]",
                to_compact.len()
            )
        };

        let summary_message = LlmMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: format!(
                    "[Conversation Summary - {} messages]:\n{}",
                    to_compact.len(),
                    summary_text
                ),
            }],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: None,
        };

        let mut compacted = initial_system;
        compacted.push(summary_message);
        compacted.extend(to_keep.to_vec());

        let tokens_after = Compactor::estimate_tokens(&compacted);
        let tokens_saved = estimated_tokens.saturating_sub(tokens_after);

        // Determine the next compaction number by counting existing compaction events
        let existing_compactions = session
            .load_previous_compaction_summary()
            .await
            .ok()
            .flatten()
            .map_or(0, |_| 1); // Simplified: just count 1 if any previous summary exists
        let compaction_number = existing_compactions + 1;

        let entry = CompactionEntry {
            timestamp: chrono::Utc::now(),
            summary: summary_text,
            first_kept_entry_id: format!("kept_{}", to_keep.len()),
            messages_compacted: to_compact.len(),
            tokens_before: estimated_tokens,
            tokens_after,
            compaction_number,
            details: None,
        };

        // Record compaction in session
        session
            .record_compaction(
                &entry.summary,
                entry.messages_compacted,
                entry.tokens_before,
                entry.tokens_after,
                entry.compaction_number,
                entry.details.as_ref(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to record compaction: {e}"))?;

        // Update context cache after compaction event is recorded so checksum matches
        session
            .update_context_cache(&compacted)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to update context cache: {e}"))?;

        Ok(CliCompactionResult {
            messages: compacted,
            entry,
            tokens_saved,
        })
    }
}

impl Default for SessionCompactor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::Peer;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_dry_run_empty_session() {
        let temp = TempDir::new().unwrap();
        let storage = crate::session::jsonl::SessionStorage::new(temp.path().to_path_buf());
        let peer = Peer::User("default".to_string());
        let session_id = "test-dry-run";

        storage.create_session(session_id, None).await.unwrap();
        let session = Session::open_by_id("test-agent", session_id, temp.path(), Some(&peer))
            .await
            .unwrap();

        let compactor = SessionCompactor::new();
        let report = compactor.dry_run(&session, None).await.unwrap();

        assert_eq!(report.message_count, 0);
        assert_eq!(report.messages_to_compact, 0);
    }

    #[tokio::test]
    async fn test_compact_truncates_messages() {
        let temp = TempDir::new().unwrap();
        let storage = crate::session::jsonl::SessionStorage::new(temp.path().to_path_buf());
        let peer = Peer::User("default".to_string());
        let session_id = "test-compact";

        storage.create_session(session_id, None).await.unwrap();
        let mut session = Session::open_by_id("test-agent", session_id, temp.path(), Some(&peer))
            .await
            .unwrap();

        // Add system + 6 messages (enough to compact)
        session.add_system("You are helpful.").await.unwrap();
        session.add_user("Message 1").await.unwrap();
        session.add_assistant("Reply 1", None, None).await.unwrap();
        session.add_user("Message 2").await.unwrap();
        session.add_assistant("Reply 2", None, None).await.unwrap();
        session.add_user("Message 3").await.unwrap();
        session.add_assistant("Reply 3", None, None).await.unwrap();

        let compactor = SessionCompactor::new();
        let result = compactor.compact(&mut session, None).await.unwrap();

        // Should have system + summary + up to 4 kept messages
        assert!(
            result.messages.len() <= 6,
            "Expected at most 6 messages after compaction, got {}",
            result.messages.len()
        );
        assert_eq!(result.entry.compaction_number, 1);
        assert!(result.tokens_saved > 0 || result.entry.messages_compacted > 0);
    }
}
