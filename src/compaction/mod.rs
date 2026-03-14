//! Context Compaction - LLM-based conversation history summarization
//!
//! When sessions approach context window limits, compaction:
//! 1. Sends older conversation to LLM for summarization
//! 2. Replaces old messages with a compact summary
//! 3. Supports cumulative summaries (updating previous summaries)
//! 4. Uses structured format (Goal, Progress, Decisions, Next Steps)
//!
//! Based on pi_agent_rust compaction algorithm.

pub mod background;
pub mod flush;

use crate::providers::{ChatMessage, MessageRole, Provider};
use crate::types::message::ContentBlock;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Approximate characters per token for estimation
const CHARS_PER_TOKEN: usize = 4;

/// System prompt for initial summarization
const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// Prompt for initial summarization (when no previous summary exists)
const INITIAL_SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another AI will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// Prompt for updating an existing summary
const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// Compaction configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Enable auto-compaction
    pub enabled: bool,
    /// Context window size (tokens)
    pub context_window_tokens: usize,
    /// Reserve tokens (keep this many free)
    pub reserve_tokens: usize,
    /// Keep this many recent tokens intact
    pub keep_recent_tokens: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            context_window_tokens: 128_000, // Default to kimi context
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

/// A compaction entry in the conversation history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEntry {
    /// When compaction occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Summary text (structured format)
    pub summary: String,
    /// Entry ID of first kept message (for reference)
    pub first_kept_entry_id: String,
    /// Number of messages that were compacted
    pub messages_compacted: usize,
    /// Approximate tokens before compaction
    pub tokens_before: usize,
    /// Approximate tokens after compaction
    pub tokens_after: usize,
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
}

/// Result of a compaction operation
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Messages after compaction (summary + kept messages)
    pub messages: Vec<ChatMessage>,
    /// Compaction entry for persistence
    pub entry: CompactionEntry,
    /// State update
    pub state: CompactionState,
}

/// Compactor for managing context window
pub struct Compactor {
    config: CompactionConfig,
    state: CompactionState,
    /// Previous summary for cumulative updates
    previous_summary: Option<String>,
}

impl Compactor {
    /// Create a new compactor with default config
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(CompactionConfig::default(), None)
    }

    /// Create a new compactor with custom config and optional previous summary
    #[must_use]
    pub fn with_config(config: CompactionConfig, previous_summary: Option<String>) -> Self {
        Self {
            config,
            state: CompactionState::default(),
            previous_summary,
        }
    }

    /// Create compactor with previous summary loaded from session
    #[must_use]
    pub fn with_previous_summary(mut self, summary: Option<String>) -> Self {
        self.previous_summary = summary;
        self
    }

    /// Get current compaction state
    #[must_use]
    pub fn state(&self) -> &CompactionState {
        &self.state
    }

    /// Get the current summary (for cumulative updates)
    #[must_use]
    pub fn current_summary(&self) -> Option<&String> {
        self.previous_summary.as_ref()
    }

    /// Estimate token count for messages
    #[must_use]
    pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .map(|m| {
                let content_len: usize = m
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => text.len(),
                        _ => 50, // Estimate for other blocks
                    })
                    .sum();
                (content_len + 20) / CHARS_PER_TOKEN + 4
            })
            .sum()
    }

    /// Check if compaction is needed based on current token count
    #[must_use]
    pub fn should_compact(&self, estimated_tokens: usize) -> bool {
        if !self.config.enabled {
            return false;
        }

        // Calculate threshold: context window minus reserve and keep_recent
        let threshold = self
            .config
            .context_window_tokens
            .saturating_sub(self.config.reserve_tokens + self.config.keep_recent_tokens);

        debug!(
            "Checking compaction: {} tokens, threshold: {}, window: {}",
            estimated_tokens, threshold, self.config.context_window_tokens
        );

        estimated_tokens >= threshold
    }

    /// Select messages to compact vs keep (system messages should be pre-filtered)
    /// Returns (`messages_to_compact`, `messages_to_keep`)
    fn select_messages(&self, messages: &[ChatMessage]) -> (Vec<ChatMessage>, Vec<ChatMessage>) {
        if messages.len() < 3 {
            return (vec![], messages.to_vec());
        }

        // Strategy: Keep recent messages that fit within keep_recent_tokens
        // Start from the end and work backwards
        let mut keep_count = 0usize;
        let mut keep_tokens = 0usize;

        for msg in messages.iter().rev() {
            let content_len: usize = msg
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.len(),
                    _ => 50,
                })
                .sum();
            let msg_tokens = (content_len + 20) / CHARS_PER_TOKEN + 4;

            if keep_tokens + msg_tokens > self.config.keep_recent_tokens {
                break;
            }

            keep_tokens += msg_tokens;
            keep_count += 1;
        }

        // Always keep at least the last 2 messages (user + assistant)
        keep_count = keep_count.max(2).min(messages.len());

        let split_point = messages.len() - keep_count;
        let to_compact = messages[..split_point].to_vec();
        let to_keep = messages[split_point..].to_vec();

        info!(
            "Selected {} messages to compact, keeping {} messages (~{} tokens)",
            to_compact.len(),
            to_keep.len(),
            keep_tokens
        );

        (to_compact, to_keep)
    }

    /// Format messages for summarization prompt
    fn format_history_for_summary(&self, messages: &[ChatMessage]) -> String {
        let mut formatted = String::new();

        for msg in messages {
            let role_label = match msg.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::Tool => "Tool",
            };

            // Extract text content
            let content: String = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    ContentBlock::ToolCall { name, .. } => Some(format!("[Tool: {name}]")),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");

            // Truncate very long messages for the summary prompt
            let display = if content.len() > 500 {
                format!("{}... [truncated]", &content[..500])
            } else {
                content
            };

            formatted.push_str(&format!("{role_label}: {display}\n\n"));
        }

        formatted
    }

    /// Generate summary using LLM (cumulative if previous summary exists)
    async fn generate_summary_with_llm(
        &self,
        messages: &[ChatMessage],
        provider: &Arc<dyn Provider>,
    ) -> Result<String> {
        let history = self.format_history_for_summary(messages);

        // Choose prompt based on whether we have a previous summary
        let (base_prompt, is_update) = if let Some(ref prev) = self.previous_summary {
            (
                format!(
                    "<previous-summary>\n{prev}\n</previous-summary>\n\n{UPDATE_SUMMARIZATION_PROMPT}"
                ),
                true,
            )
        } else {
            (INITIAL_SUMMARIZATION_PROMPT.to_string(), false)
        };

        let prompt = format!(
            "<conversation>\n{history}\n</conversation>\n\n{base_prompt}"
        );

        debug!(
            "Requesting {} summary from LLM ({} messages)",
            if is_update { "cumulative" } else { "initial" },
            messages.len()
        );

        // Use the simple chat method
        let summary = provider
            .chat(&prompt, "default", 0.3)
            .await
            .context("Failed to generate compaction summary")?;

        if summary.is_empty() {
            warn!("LLM returned empty summary, using fallback");
            if let Some(ref prev) = self.previous_summary {
                // If update failed, return previous summary with note
                Ok(format!(
                    "{}\n\n[Note: {} new messages not incorporated]",
                    prev,
                    messages.len()
                ))
            } else {
                Ok(format!(
                    "[{} messages summarized - conversation history]",
                    messages.len()
                ))
            }
        } else {
            Ok(summary)
        }
    }

    /// Perform compaction using LLM for summarization
    pub async fn compact(
        &mut self,
        messages: &[ChatMessage],
        provider: &Arc<dyn Provider>,
    ) -> Result<CompactionResult> {
        if messages.len() < 4 {
            return Err(anyhow::anyhow!(
                "Not enough messages to compact (need at least 4, got {})",
                messages.len()
            ));
        }

        let tokens_before = Self::estimate_tokens(messages);

        // Extract ONLY the initial system prompt (first message if it's system)
        // Runtime-injected system messages (compaction summaries, interceptors) are treated as conversation
        let (initial_system_msg, conversation_msgs): (Vec<_>, Vec<_>) =
            if !messages.is_empty() && messages[0].role == MessageRole::System {
                (vec![messages[0].clone()], messages[1..].to_vec())
            } else {
                (vec![], messages.to_vec())
            };

        // Select from conversation messages only (includes runtime system messages)
        let (to_compact, to_keep_conversation) = self.select_messages(&conversation_msgs);

        if to_compact.is_empty() {
            return Err(anyhow::anyhow!(
                "No messages selected for compaction (conversation too short)"
            ));
        }

        // Generate LLM summary (cumulative if previous exists)
        let summary = self
            .generate_summary_with_llm(&to_compact, provider)
            .await?;

        // Update previous summary for future cumulative updates
        self.previous_summary = Some(summary.clone());

        // Create summary message (as a system message after original system prompts)
        let summary_content = format!(
            "[Conversation Summary - {} messages]:\n{}",
            to_compact.len(),
            summary
        );
        let summary_message = ChatMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: summary_content,
            }],
            tool_calls: None,
            tool_call_id: None,
        };

        // Build compacted message list: Initial system prompt + New Summary + Recent conversation
        // Note: Old compaction summaries in to_compact get summarized into the new summary
        let mut compacted = initial_system_msg.clone();
        compacted.push(summary_message);
        compacted.extend(to_keep_conversation.clone());

        let tokens_after = Self::estimate_tokens(&compacted);
        let tokens_saved = tokens_before.saturating_sub(tokens_after);

        // Update state
        self.state.compaction_count += 1;
        self.state.total_tokens_saved += tokens_saved;
        self.state.last_compaction_at = Some(chrono::Utc::now());

        let entry = CompactionEntry {
            timestamp: chrono::Utc::now(),
            summary,
            first_kept_entry_id: format!("kept_{}", to_keep_conversation.len()),
            messages_compacted: to_compact.len(),
            tokens_before,
            tokens_after,
            compaction_number: self.state.compaction_count,
        };

        info!(
            "Compaction #{} {}: {} messages → summary, saved {} tokens ({} → {}), kept {} initial system prompt",
            self.state.compaction_count,
            if self.state.compaction_count > 1 { "(cumulative)" } else { "" },
            entry.messages_compacted,
            tokens_saved,
            tokens_before,
            tokens_after,
            initial_system_msg.len()
        );

        Ok(CompactionResult {
            messages: compacted,
            entry,
            state: self.state.clone(),
        })
    }

    /// Quick check if compaction would help
    #[must_use]
    pub fn would_help(&self, estimated_tokens: usize) -> bool {
        self.config.enabled && estimated_tokens > self.config.context_window_tokens / 2
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

        messages.push(ChatMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: "You are a helpful assistant.".to_string(),
            }],
            tool_calls: None,
            tool_call_id: None,
        });

        for i in 0..count {
            if i % 2 == 0 {
                messages.push(ChatMessage {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: format!("User message {}", i),
                    }],
                    tool_calls: None,
                    tool_call_id: None,
                });
            } else {
                messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::Text {
                        text: format!(
                            "Assistant response {} with some additional text to make it longer",
                            i
                        ),
                    }],
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        messages
    }

    #[test]
    fn test_should_compact() {
        let compactor = Compactor::new();

        // Should compact when near threshold
        assert!(compactor.should_compact(100_000)); // Near 128k window with reserves

        // Should not compact when well under
        assert!(!compactor.should_compact(50_000));
    }

    #[test]
    fn test_estimate_tokens() {
        let messages = create_test_messages(5);
        let tokens = Compactor::estimate_tokens(&messages);

        assert!(tokens > 0);
        assert!(tokens < 5000);
    }

    // TODO: Fix test_select_messages - compaction logic changed
    // #[test]
    // fn test_select_messages() { ... }

    #[test]
    fn test_format_history() {
        let compactor = Compactor::new();
        let messages = create_test_messages(3);

        let formatted = compactor.format_history_for_summary(&messages);

        assert!(formatted.contains("User:"));
        assert!(formatted.contains("Assistant:"));
    }

    #[test]
    fn test_would_help() {
        let compactor = Compactor::new();

        assert!(compactor.would_help(100_000)); // Over half window
        assert!(!compactor.would_help(30_000)); // Well under half
    }

    #[test]
    fn test_cumulative_summary_tracking() {
        let mut compactor = Compactor::with_previous_summary(
            Compactor::with_config(CompactionConfig::default(), None),
            Some("Initial summary".to_string()),
        );

        assert_eq!(
            compactor.current_summary(),
            Some(&"Initial summary".to_string())
        );
    }
}
