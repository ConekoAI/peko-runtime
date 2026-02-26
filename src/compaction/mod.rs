//! Context Compaction - Summarize and compact conversation history
//!
//! When sessions approach context window limits, compaction:
//! 1. Summarizes older conversation into a compact summary
//! 2. Persists the summary in session history
//! 3. Keeps recent messages intact
//!
//! This allows long-running sessions without hitting token limits.

pub mod flush;

use crate::types::provider::ChatMessage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Compaction configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Enable auto-compaction
    pub enabled: bool,
    /// Reserve tokens floor (keep this many tokens free)
    pub reserve_tokens_floor: usize,
    /// Soft threshold for triggering compaction
    pub soft_threshold_tokens: usize,
    /// System prompt for compaction summary
    pub system_prompt: String,
    /// User prompt for compaction summary
    pub user_prompt: String,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens_floor: 20000,
            soft_threshold_tokens: 4000,
            system_prompt: "Summarize the following conversation concisely. Focus on key decisions, facts, and open questions. Be brief but complete.".to_string(),
            user_prompt: "Please summarize this conversation history:\n\n{history}\n\nProvide a concise summary of what was discussed and any decisions made.".to_string(),
        }
    }
}

/// A compaction entry in the conversation history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEntry {
    /// When compaction occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Summary text
    pub summary: String,
    /// Number of messages that were compacted
    pub messages_compacted: usize,
    /// Approximate tokens saved
    pub tokens_saved: usize,
    /// Compaction number (1st, 2nd, etc.)
    pub compaction_number: usize,
}

/// Tracks compaction state for a session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionState {
    /// Number of compactions performed
    pub compaction_count: usize,
    /// Total tokens saved through compaction
    pub total_tokens_saved: usize,
    /// Last compaction timestamp
    pub last_compaction_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether a memory flush was performed before last compaction
    pub flush_performed: bool,
}

/// Compactor for managing context window
pub struct Compactor {
    config: CompactionConfig,
    state: CompactionState,
}

impl Compactor {
    /// Create a new compactor with default config
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(CompactionConfig::default())
    }

    /// Create a new compactor with custom config
    #[must_use]
    pub fn with_config(config: CompactionConfig) -> Self {
        Self {
            config,
            state: CompactionState::default(),
        }
    }

    /// Get current compaction state
    #[must_use]
    pub fn state(&self) -> &CompactionState {
        &self.state
    }

    /// Check if compaction is needed based on current token count
    pub fn should_compact(&self, current_tokens: usize, context_window: usize) -> bool {
        if !self.config.enabled {
            return false;
        }

        let threshold = context_window
            .saturating_sub(self.config.reserve_tokens_floor + self.config.soft_threshold_tokens);

        debug!(
            "Checking compaction: {} tokens, threshold: {}, window: {}",
            current_tokens, threshold, context_window
        );

        current_tokens >= threshold
    }

    /// Estimate token count for a message (rough approximation)
    #[must_use]
    pub fn estimate_tokens(message: &ChatMessage) -> usize {
        // Rough approximation: 4 chars ≈ 1 token
        let content_len = message.content.len();
        let role_len = message.role.len();
        (content_len + role_len) / 4 + 10 // Base overhead
    }

    /// Calculate total tokens for a message list
    #[must_use]
    pub fn calculate_tokens(messages: &[ChatMessage]) -> usize {
        messages.iter().map(Self::estimate_tokens).sum()
    }

    /// Select messages to compact (older portion, keeping recent context)
    /// Returns (`messages_to_compact`, `messages_to_keep`)
    pub fn select_messages_for_compaction(
        &self,
        messages: &[ChatMessage],
        _target_tokens: usize,
    ) -> (Vec<ChatMessage>, Vec<ChatMessage>) {
        if messages.len() < 4 {
            // Don't compact very short conversations
            return (vec![], messages.to_vec());
        }

        // Always keep the most recent messages (system + last 3-4 exchanges)
        let keep_count = messages.len().min(6); // Keep ~6 most recent messages
        let split_point = messages.len() - keep_count;

        let to_compact = messages[..split_point].to_vec();
        let to_keep = messages[split_point..].to_vec();

        let compact_tokens = Self::calculate_tokens(&to_compact);
        let keep_tokens = Self::calculate_tokens(&to_keep);

        info!(
            "Selected {} messages to compact ({} tokens), keeping {} messages ({} tokens)",
            to_compact.len(),
            compact_tokens,
            to_keep.len(),
            keep_tokens
        );

        (to_compact, to_keep)
    }

    /// Generate a summary from messages
    /// This is a placeholder - in production, you'd call an LLM
    #[must_use]
    pub fn generate_summary(&self, messages: &[ChatMessage]) -> String {
        // Extract key information
        let mut summary_parts = vec![];

        // Add conversation overview
        let user_msg_count = messages.iter().filter(|m| m.role == "user").count();
        let assistant_msg_count = messages.iter().filter(|m| m.role == "assistant").count();

        summary_parts.push(format!(
            "Conversation with {user_msg_count} user messages and {assistant_msg_count} assistant responses."
        ));

        // Extract tool calls if any
        let tool_calls: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == "assistant")
            .filter_map(|m| {
                if m.content.contains("[tool:") {
                    Some("Tool usage detected")
                } else {
                    None
                }
            })
            .collect();

        if !tool_calls.is_empty() {
            summary_parts.push("Tools were used during this conversation.".to_string());
        }

        // Try to extract key topics (first user message often has the main topic)
        if let Some(first_user) = messages.iter().find(|m| m.role == "user") {
            let preview: String = first_user.content.chars().take(100).collect();
            summary_parts.push(format!("Topic: {preview}..."));
        }

        summary_parts.join(" ")
    }

    /// Perform compaction on a message list
    /// Returns (`compacted_messages`, `compaction_entry`)
    pub fn compact(
        &mut self,
        messages: &[ChatMessage],
    ) -> Result<(Vec<ChatMessage>, CompactionEntry)> {
        if messages.len() < 4 {
            return Err(anyhow::anyhow!("Not enough messages to compact"));
        }

        let original_tokens = Self::calculate_tokens(messages);

        // Select messages to compact vs keep
        let (to_compact, to_keep) =
            self.select_messages_for_compaction(messages, self.config.reserve_tokens_floor);

        if to_compact.is_empty() {
            return Err(anyhow::anyhow!("No messages selected for compaction"));
        }

        // Generate summary
        let summary = self.generate_summary(&to_compact);

        // Create system message with summary
        let summary_message =
            ChatMessage::system(&format!("[Previous conversation summary]: {summary}"));

        // Build new message list
        let mut compacted = vec![summary_message];
        compacted.extend(to_keep);

        let new_tokens = Self::calculate_tokens(&compacted);
        let tokens_saved = original_tokens.saturating_sub(new_tokens);

        // Update state
        self.state.compaction_count += 1;
        self.state.total_tokens_saved += tokens_saved;
        self.state.last_compaction_at = Some(chrono::Utc::now());

        let entry = CompactionEntry {
            timestamp: chrono::Utc::now(),
            summary,
            messages_compacted: to_compact.len(),
            tokens_saved,
            compaction_number: self.state.compaction_count,
        };

        info!(
            "Compaction #{} complete: {} messages → summary, saved {} tokens",
            entry.compaction_number, entry.messages_compacted, entry.tokens_saved
        );

        Ok((compacted, entry))
    }

    /// Get a status summary
    #[must_use]
    pub fn status(&self) -> String {
        format!(
            "🧹 Compactions: {} | Tokens saved: {} | Last: {}",
            self.state.compaction_count,
            self.state.total_tokens_saved,
            self.state.last_compaction_at.map_or_else(
                || "Never".to_string(),
                |t| t.format("%Y-%m-%d %H:%M UTC").to_string()
            )
        )
    }
}

impl Default for Compactor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_messages(count: usize) -> Vec<ChatMessage> {
        let mut messages = vec![];

        // Add system message
        messages.push(ChatMessage::system("You are a helpful assistant."));

        // Add alternating user/assistant messages
        for i in 0..count {
            if i % 2 == 0 {
                messages.push(ChatMessage::user(&format!("User message {}", i)));
            } else {
                messages.push(ChatMessage::assistant(&format!("Assistant response {}", i)));
            }
        }

        messages
    }

    #[test]
    fn test_should_compact() {
        let compactor = Compactor::new();

        // Should compact when near limit
        assert!(compactor.should_compact(90000, 100000)); // 90k of 100k window

        // Should not compact when well under limit
        assert!(!compactor.should_compact(50000, 100000)); // 50k of 100k window
    }

    #[test]
    fn test_calculate_tokens() {
        let messages = create_test_messages(5);
        let tokens = Compactor::calculate_tokens(&messages);

        assert!(tokens > 0);
        assert!(tokens < 10000); // Sanity check
    }

    #[test]
    fn test_select_messages() {
        let compactor = Compactor::new();
        let messages = create_test_messages(10);

        let (to_compact, to_keep) = compactor.select_messages_for_compaction(&messages, 20000);

        assert!(!to_compact.is_empty());
        assert!(!to_keep.is_empty());
        assert_eq!(messages.len(), to_compact.len() + to_keep.len());
    }

    #[test]
    fn test_compact() {
        let mut compactor = Compactor::new();
        let messages = create_test_messages(10);

        let (compacted, entry) = compactor.compact(&messages).unwrap();

        assert!(compacted.len() < messages.len()); // Should be smaller
        assert_eq!(compactor.state.compaction_count, 1);
        assert!(entry.tokens_saved > 0);
        assert!(entry.messages_compacted > 0);
    }

    #[test]
    fn test_status() {
        let compactor = Compactor::new();
        let status = compactor.status();

        assert!(status.contains("Compactions"));
        assert!(status.contains("0")); // No compactions yet
    }
}
