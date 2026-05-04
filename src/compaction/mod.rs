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
pub mod registry;
pub mod summary_format;
pub mod turn_boundaries;

#[cfg(test)]
mod integration_tests;

use crate::providers::MessageRole;
use crate::types::message::LlmMessage;
use crate::types::message::ContentBlock;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Approximate characters per token for estimation
const CHARS_PER_TOKEN: usize = 4;

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
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Auto-compaction trigger threshold as percent of context window (0-100)
    #[serde(default = "default_auto_threshold_percent")]
    pub auto_threshold_percent: u8,
    /// Tokens to reserve for LLM response headroom
    #[serde(default = "default_reserve_tokens")]
    pub reserve_tokens: usize,
    /// Minimum recent conversation to preserve during compaction
    #[serde(default = "default_keep_recent_tokens")]
    pub keep_recent_tokens: usize,
    /// Maximum compactions per session (quota)
    #[serde(default = "default_max_compactions_per_session")]
    pub max_compactions_per_session: usize,
    /// Cooldown between compactions in seconds
    #[serde(default = "default_cooldown_seconds")]
    pub cooldown_seconds: u64,
    /// Per-provider/per-model context window overrides
    #[serde(default)]
    pub model_limits: std::collections::HashMap<String, std::collections::HashMap<String, usize>>,
}

fn default_enabled() -> bool {
    true
}

fn default_auto_threshold_percent() -> u8 {
    85
}

fn default_reserve_tokens() -> usize {
    16_384
}

fn default_keep_recent_tokens() -> usize {
    20_000
}

fn default_max_compactions_per_session() -> usize {
    100
}

fn default_cooldown_seconds() -> u64 {
    60
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            auto_threshold_percent: default_auto_threshold_percent(),
            reserve_tokens: default_reserve_tokens(),
            keep_recent_tokens: default_keep_recent_tokens(),
            max_compactions_per_session: default_max_compactions_per_session(),
            cooldown_seconds: default_cooldown_seconds(),
            model_limits: std::collections::HashMap::new(),
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
    /// Tracked file operations from compacted messages
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<summary_format::CompactionDetails>,
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

/// Detailed token usage estimate with breakdown.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ContextUsageEstimate {
    /// Total estimated tokens
    pub tokens: usize,
    /// Tokens from the last assistant usage record
    pub usage_tokens: usize,
    /// Tokens estimated for trailing messages after last usage
    pub trailing_tokens: usize,
    /// Index of the last assistant message with usage data
    pub last_usage_index: Option<usize>,
}

/// Find the last assistant message with usage data.
/// Returns `(usage, index)` if found.
///
/// TODO: Wire this up when LlmMessage carries usage metadata from provider responses.
/// For now, always returns None, causing fallback to heuristic estimation.
#[allow(dead_code)]
fn find_last_assistant_usage(
    _messages: &[LlmMessage],
) -> Option<(crate::providers::TokenUsage, usize)> {
    None
}

/// Result of a compaction operation
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Messages after compaction (summary + kept messages)
    pub messages: Vec<LlmMessage>,
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

#[allow(dead_code)]
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

    /// Estimate token count for messages using simple heuristic.
    #[must_use]
    pub fn estimate_tokens(messages: &[LlmMessage]) -> usize {
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

    /// Hybrid token estimation using the last assistant usage when available.
    ///
    /// Walks backward through messages to find the last assistant message with
    /// valid usage data. Uses that as a baseline and adds char/4 heuristic for
    /// trailing messages.
    ///
    /// Returns `ContextUsageEstimate` with detailed breakdown.
    #[must_use]
    pub fn estimate_context_tokens(messages: &[LlmMessage]) -> ContextUsageEstimate {
        // Find last assistant message with usage data
        if let Some((usage, index)) = find_last_assistant_usage(messages) {
            let usage_tokens = usage.input + usage.output;
            let trailing_tokens: usize = messages[index + 1..]
                .iter()
                .map(|m| {
                    let content_len: usize = m
                        .content
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text { text } => text.len(),
                            _ => 50,
                        })
                        .sum();
                    (content_len + 20) / CHARS_PER_TOKEN + 4
                })
                .sum();
            ContextUsageEstimate {
                tokens: (usage_tokens as usize) + trailing_tokens,
                usage_tokens: usage_tokens as usize,
                trailing_tokens,
                last_usage_index: Some(index),
            }
        } else {
            // No usage available — fall back to heuristic for all messages
            let estimated = Self::estimate_tokens(messages);
            ContextUsageEstimate {
                tokens: estimated,
                usage_tokens: 0,
                trailing_tokens: estimated,
                last_usage_index: None,
            }
        }
    }

    /// Check if compaction is needed based on current token count.
    ///
    /// Uses the dual-threshold logic from ADR-022:
    /// - Ratio threshold: `context_window * auto_threshold_percent / 100`
    /// - Reserved threshold: `context_window - reserve_tokens`
    ///
    /// Compaction triggers when **either** threshold is met.
    #[must_use]
    pub fn should_compact(&self, estimated_tokens: usize, context_window: usize) -> bool {
        registry::should_auto_compact(estimated_tokens, context_window, &self.config)
    }

    /// Legacy check using a hard-coded context window (deprecated).
    /// Prefer `should_compact(estimated_tokens, context_window)`.
    #[must_use]
    pub fn should_compact_legacy(&self, estimated_tokens: usize) -> bool {
        if !self.config.enabled {
            return false;
        }
        let threshold = 128_000usize
            .saturating_sub(self.config.reserve_tokens + self.config.keep_recent_tokens);
        estimated_tokens >= threshold
    }

    /// Select messages to compact vs keep, respecting turn boundaries.
    ///
    /// Never cuts at tool results — they must stay paired with their tool call.
    /// Returns (`messages_to_compact`, `messages_to_keep`, `is_split_turn`).
    fn select_messages(&self, messages: &[LlmMessage]) -> (Vec<LlmMessage>, Vec<LlmMessage>, bool) {
        turn_boundaries::select_messages_respecting_boundaries(
            messages,
            self.config.keep_recent_tokens,
        )
    }

    /// Format messages for summarization prompt
    fn format_history_for_summary(&self, messages: &[LlmMessage]) -> String {
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
        messages: &[LlmMessage],
        provider: &Arc<crate::providers::Provider>,
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

        let prompt = format!("<conversation>\n{history}\n</conversation>\n\n{base_prompt}");

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
        messages: &[LlmMessage],
        provider: &Arc<crate::providers::Provider>,
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
        let (to_compact, to_keep_conversation, is_split_turn) =
            self.select_messages(&conversation_msgs);

        if to_compact.is_empty() && !is_split_turn {
            return Err(anyhow::anyhow!(
                "No messages selected for compaction (conversation too short)"
            ));
        }

        // Generate LLM summary (cumulative if previous exists)
        // For split turns, generate two summaries and merge them
        let summary = if is_split_turn && !to_compact.is_empty() {
            // Split turn: generate history summary + turn prefix summary
            let turn_prefix = turn_boundaries::extract_turn_prefix(&conversation_msgs, conversation_msgs.len() - to_keep_conversation.len())
                .unwrap_or_default();
            let history_summary = self.generate_summary_with_llm(&to_compact, provider).await?;
            let prefix_summary = if !turn_prefix.is_empty() {
                self.generate_summary_with_llm(&turn_prefix, provider).await?
            } else {
                String::new()
            };
            if prefix_summary.is_empty() {
                history_summary
            } else {
                format!("{}\n\n---\n\n**Turn Context (split turn):**\n\n{}", history_summary, prefix_summary)
            }
        } else {
            self.generate_summary_with_llm(&to_compact, provider).await?
        };

        // Track file operations from messages being summarized
        let _file_ops = summary_format::extract_file_ops_from_messages(&to_compact);
        let cumulative_details = summary_format::compute_cumulative_details(
            None, // TODO: pass previous details when available
            &to_compact,
        );

        // Update previous summary for future cumulative updates
        self.previous_summary = Some(summary.clone());

        // Create summary message with structured format and file operations
        let summary_with_ops = summary_format::format_summary_with_file_ops(&summary, &cumulative_details);
        let summary_content = format!(
            "[Conversation Summary - {} messages]:\n{}",
            to_compact.len(),
            summary_with_ops
        );
        let summary_message = LlmMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: summary_content,
            }],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
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
            details: Some(cumulative_details),
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
    pub fn would_help(&self, estimated_tokens: usize, context_window: usize) -> bool {
        self.config.enabled && estimated_tokens > context_window / 2
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

    fn create_test_messages(count: usize) -> Vec<LlmMessage> {
        let mut messages = vec![];

        messages.push(LlmMessage::system("You are a helpful assistant."));

        for i in 0..count {
            if i % 2 == 0 {
                messages.push(LlmMessage::user(format!("User message {i}")));
            } else {
                messages.push(LlmMessage::assistant(format!(
                    "Assistant response {i} with some additional text to make it longer"
                )));
            }
        }

        messages
    }

    #[test]
    fn test_should_compact() {
        let compactor = Compactor::new();

        // Should compact when near threshold (128k window, 85% = 108.8k)
        assert!(compactor.should_compact(110_000, 128_000));

        // Should not compact when well under
        assert!(!compactor.should_compact(50_000, 128_000));
    }

    #[test]
    fn test_estimate_tokens() {
        let messages = create_test_messages(5);
        let tokens = Compactor::estimate_tokens(&messages);

        assert!(tokens > 0);
        assert!(tokens < 5000);
    }

    #[test]
    fn test_select_messages() {
        // Create many long messages to force compaction
        let mut messages = vec![LlmMessage::system("You are a helpful assistant.")];

        // Add 30 very long messages to exceed keep_recent_tokens (20_000)
        // Each message: 3000 chars ≈ 750 tokens. 30 messages ≈ 22,500 tokens.
        for i in 0..30 {
            let text = "x".repeat(3000); // ~750 tokens each
            if i % 2 == 0 {
                messages.push(LlmMessage::user(format!("User {i}: {text}")));
            } else {
                messages.push(LlmMessage::assistant(format!("Assistant {i}: {text}")));
            }
        }

        let compactor = Compactor::new();
        let conversation = &messages[1..]; // skip system
        let (to_compact, to_keep, is_split) = compactor.select_messages(conversation);

        // Should keep at least 2 messages
        assert!(
            to_keep.len() >= 2,
            "Should keep at least 2 messages, got {}",
            to_keep.len()
        );
        // With many long messages, we should have some to compact
        assert!(!to_compact.is_empty(), "Should have messages to compact");
        assert!(!is_split, "Normal conversation should not be split turn when to_compact has users");
        assert_eq!(
            to_compact.len() + to_keep.len(),
            conversation.len(),
            "Compact + keep should equal total"
        );
    }

    #[test]
    fn test_select_messages_respects_tool_boundaries() {
        use crate::types::message::ContentBlock;

        let compactor = Compactor::new();
        let mut messages = vec![
            LlmMessage::user("User 1"),
            LlmMessage::assistant("Assistant 1"),
            LlmMessage::user("User 2"),
            LlmMessage::assistant("I'll use a tool"),
            LlmMessage::tool_result("tc1", "read_file", "file content"),
        ];

        let (to_compact, to_keep, _is_split) = compactor.select_messages(&messages);

        // If tool result is in keep, assistant must also be in keep
        if to_keep.iter().any(|m| m.role == MessageRole::Tool) {
            let tool_idx = to_keep.iter().position(|m| m.role == MessageRole::Tool).unwrap();
            assert!(
                tool_idx > 0 && to_keep[tool_idx - 1].role == MessageRole::Assistant,
                "Tool result must follow assistant in kept messages"
            );
        }
    }

    #[test]
    fn test_estimate_context_tokens_fallback() {
        let messages = create_test_messages(5);
        let estimate = Compactor::estimate_context_tokens(&messages);

        // Since find_last_assistant_usage returns None, this should fallback
        assert_eq!(estimate.usage_tokens, 0);
        assert_eq!(estimate.trailing_tokens, estimate.tokens);
        assert!(estimate.last_usage_index.is_none());
    }

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

        assert!(compactor.would_help(100_000, 128_000)); // Over half window
        assert!(!compactor.would_help(30_000, 128_000)); // Well under half
    }

    #[test]
    fn test_cumulative_summary_tracking() {
        let compactor = Compactor::with_previous_summary(
            Compactor::with_config(CompactionConfig::default(), None),
            Some("Initial summary".to_string()),
        );

        assert_eq!(
            compactor.current_summary(),
            Some(&"Initial summary".to_string())
        );
    }

    #[test]
    fn test_compaction_config_from_toml_section() {
        let toml_str = r#"
[compaction]
enabled = true
auto_threshold_percent = 5
reserve_tokens = 500
keep_recent_tokens = 1000
max_compactions_per_session = 100
cooldown_seconds = 0

[compaction.model_limits]
minimax = { "M2.7" = 4000 }
"#;
        let root = toml::from_str::<toml::Value>(toml_str).unwrap();
        let compaction_table = root.get("compaction").expect("compaction section should exist");
        let cfg: CompactionConfig = compaction_table.clone().try_into().expect("should parse");

        assert!(cfg.enabled);
        assert_eq!(cfg.auto_threshold_percent, 5);
        assert_eq!(cfg.reserve_tokens, 500);
        assert_eq!(cfg.keep_recent_tokens, 1000);
        assert_eq!(cfg.max_compactions_per_session, 100);
        assert_eq!(cfg.cooldown_seconds, 0);
        assert_eq!(cfg.model_limits.get("minimax").unwrap().get("M2.7"), Some(&4000));
    }
}
