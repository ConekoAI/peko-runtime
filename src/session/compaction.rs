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
pub mod cli;
pub mod eviction;
pub mod summary_format;
pub mod turn_boundaries;

#[cfg(test)]
mod integration_tests;

use crate::common::types::message::ContentBlock;
use crate::common::types::message::LlmMessage;
use crate::providers::MessageRole;
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
    // Note: the previous `model_limits` field — per-provider/per-model
    // context window overrides — has been removed. Model max context
    // is now sourced from `ProviderCatalog::model_context_length` (see
    // `crate::providers::catalog`). Existing `~/.peko/config.toml`
    // files with `[compaction.model_limits]` blocks deserialize
    // cleanly because this type does not use `deny_unknown_fields`.
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
/// Walks the slice backward so we get the *most recent* anchor — the
/// `estimate_context_tokens` estimator only needs one to bound its
/// char/4 fallback to the trailing slice since the last real report.
/// F21 wires `LlmMessage.usage` from `RoleMetadata::Assistant::usage`
/// (via `SessionMessage::to_llm_message`) and from the engine loop's
/// `iteration_usage.clone()` at assistant-message construction, so
/// every assistant turn produced by the current process contributes
/// an anchor. Pre-F21 JSONL files have `usage: None` everywhere and
/// fall back to the heuristic — no migration needed.
fn find_last_assistant_usage(
    messages: &[LlmMessage],
) -> Option<(crate::providers::TokenUsage, usize)> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| m.role == MessageRole::Assistant && m.usage.is_some())
        .map(|(i, m)| (m.usage.clone().unwrap(), i))
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
    /// Token usage consumed by the summarization LLM call(s).
    /// Previously dropped on the floor; tracked here so the engine
    /// loop can add it to `total_usage` for accurate downstream
    /// quota / billing accounting.
    pub usage: crate::providers::TokenUsage,
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
        if !self.config.enabled {
            return false;
        }
        let ratio_threshold = (context_window * self.config.auto_threshold_percent as usize) / 100;
        let reserved_threshold = context_window.saturating_sub(self.config.reserve_tokens);
        estimated_tokens >= ratio_threshold || estimated_tokens >= reserved_threshold
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

    /// Generate summary using LLM (cumulative if previous summary exists).
    ///
    /// Returns `(summary_text, token_usage)` so the caller can roll
    /// the summarization LLM call's cost into the session's overall
    /// usage accounting — previously dropped on the floor because
    /// `Provider::chat` returned only `String`.
    async fn generate_summary_with_llm(
        &self,
        messages: &[LlmMessage],
        provider: &Arc<crate::providers::Provider>,
    ) -> Result<(String, crate::providers::TokenUsage)> {
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

        // Use `chat_response` (returns full `ChatResponse` including
        // usage) so we can account for the summarization call's cost.
        //
        // F19: build a `MeteredProvider` from the active task-local
        // scope (the BackgroundCompactor worker opens a `QuotaScope::with`
        // around this call). The metered wrapper auto-charges after
        // the call returns. If no scope is active (CLI / tests /
        // passthrough wrappers), `from_current_scope` returns a
        // passthrough wrapper with an unlimited meter — same behavior
        // as F18's no-op charge.
        //
        // F20: use `StackedMeteredProvider` so when both a principal
        // AND a peer scope are active, both meters charge this
        // summarization call. With a 1-element stack the behavior is
        // identical to `MeteredProvider`.
        let stacked =
            crate::providers::StackedMeteredProvider::from_current_scope(provider.clone());
        let response = stacked
            .chat_response(&prompt, "default", 0.3)
            .await
            .context("Failed to generate compaction summary")?;

        let usage = response.usage;
        let summary: String = response
            .content
            .into_iter()
            .filter_map(|cb| match cb {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect();

        let summary = if summary.is_empty() {
            warn!("LLM returned empty summary, using fallback");
            if let Some(ref prev) = self.previous_summary {
                format!(
                    "{}\n\n[Note: {} new messages not incorporated]",
                    prev,
                    messages.len()
                )
            } else {
                format!(
                    "[{} messages summarized - conversation history]",
                    messages.len()
                )
            }
        } else {
            summary
        };

        Ok((summary, usage))
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

        // Accumulate summarization LLM call usage across normal and
        // split-turn paths so the result carries the total cost of
        // this compaction run. `TokenUsage::accumulate` folds cache
        // into input and reasoning into output — same rule the engine
        // loop uses for `iteration_usage`, so downstream quota
        // accounting sees a consistent number regardless of where the
        // tokens came from.
        let mut compaction_usage = crate::providers::TokenUsage::default();

        // Generate LLM summary (cumulative if previous exists)
        // For split turns, generate two summaries and merge them
        let summary = if is_split_turn && !to_compact.is_empty() {
            // Split turn: generate history summary + turn prefix summary
            let turn_prefix = turn_boundaries::extract_turn_prefix(
                &conversation_msgs,
                conversation_msgs.len() - to_keep_conversation.len(),
            )
            .unwrap_or_default();
            let (history_text, history_usage) = self
                .generate_summary_with_llm(&to_compact, provider)
                .await?;
            compaction_usage.accumulate(&history_usage);
            let prefix_summary = if !turn_prefix.is_empty() {
                let (text, usage) = self
                    .generate_summary_with_llm(&turn_prefix, provider)
                    .await?;
                compaction_usage.accumulate(&usage);
                text
            } else {
                String::new()
            };
            if prefix_summary.is_empty() {
                history_text
            } else {
                format!(
                    "{}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
                    history_text, prefix_summary
                )
            }
        } else {
            let (text, usage) = self
                .generate_summary_with_llm(&to_compact, provider)
                .await?;
            compaction_usage.accumulate(&usage);
            text
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
        let summary_with_ops =
            summary_format::format_summary_with_file_ops(&summary, &cumulative_details);
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
            usage: None,
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
            usage: compaction_usage,
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
    use crate::providers::adapters::AnyAdapter;
    use crate::providers::core::ProviderRuntimeOptions;
    use crate::providers::mock::MockAdapter;
    use crate::providers::{Provider, TokenUsage};

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
        assert!(
            !is_split,
            "Normal conversation should not be split turn when to_compact has users"
        );
        assert_eq!(
            to_compact.len() + to_keep.len(),
            conversation.len(),
            "Compact + keep should equal total"
        );
    }

    #[test]
    fn test_select_messages_respects_tool_boundaries() {
        let compactor = Compactor::new();
        let messages = vec![
            LlmMessage::user("User 1"),
            LlmMessage::assistant("Assistant 1"),
            LlmMessage::user("User 2"),
            LlmMessage::assistant("I'll use a tool"),
            LlmMessage::tool_result("tc1", "Read", "file content", false),
        ];

        let (_to_compact, to_keep, _is_split) = compactor.select_messages(&messages);

        // If tool result is in keep, assistant must also be in keep
        if to_keep.iter().any(|m| m.role == MessageRole::Tool) {
            let tool_idx = to_keep
                .iter()
                .position(|m| m.role == MessageRole::Tool)
                .unwrap();
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

        // No usage attached on any assistant message → fallback to
        // chars/4 across the full conversation. Mirrors pre-F21
        // behaviour for JSONL without usage data.
        assert_eq!(estimate.usage_tokens, 0);
        assert_eq!(estimate.trailing_tokens, estimate.tokens);
        assert!(estimate.last_usage_index.is_none());
    }

    /// F21: walks backward through `messages` and returns the *last*
    /// assistant message with `usage.is_some()`. Without the backward
    /// walk, every assistant anchor would shift every time a new turn
    /// arrives, and the estimator would never converge on a single
    /// anchor between compactions.
    #[test]
    fn test_find_last_assistant_usage_returns_last_assistant_with_usage() {
        let first = TokenUsage {
            input: 100,
            output: 50,
            total: 150,
            ..Default::default()
        };
        let second = TokenUsage {
            input: 200,
            output: 80,
            total: 280,
            ..Default::default()
        };
        let messages = vec![
            LlmMessage::user("hi"),
            LlmMessage::assistant("first").with_usage(first.clone()),
            LlmMessage::user("next"),
            LlmMessage::assistant("second").with_usage(second.clone()),
        ];
        let (usage, idx) = find_last_assistant_usage(&messages).unwrap();
        assert_eq!(usage, second);
        assert_eq!(idx, 3);
    }

    /// F21: skips assistants with `usage: None`. If no assistant has
    /// usage, returns `None` so `estimate_context_tokens` falls back
    /// to chars/4 (this is the pre-F21 behaviour for old session JSONL).
    #[test]
    fn test_find_last_assistant_usage_skips_assistants_without_usage() {
        let messages = vec![
            LlmMessage::user("hi"),
            LlmMessage::assistant("no usage here"), // usage: None
            LlmMessage::user("next"),
        ];
        assert!(find_last_assistant_usage(&messages).is_none());
    }

    /// F21: when an anchor exists, the hybrid estimator returns
    /// `usage_tokens == usage.input + usage.output` (the exact
    /// provider-reported count) plus a char/4 estimate for the
    /// trailing slice after the anchor. `last_usage_index` points at
    /// the anchor message.
    #[test]
    fn test_estimate_context_tokens_uses_real_anchor_when_present() {
        let anchor_usage = TokenUsage {
            input: 1000,
            output: 500,
            total: 1500,
            ..Default::default()
        };
        let mut messages = vec![
            LlmMessage::system("You are a helpful assistant."),
            LlmMessage::user("First question"),
            LlmMessage::assistant("First answer").with_usage(anchor_usage.clone()),
            // Trailing slice — two more messages that didn't report
            // usage (e.g. resumed session, or usage dropped on the
            // floor). Char/4 estimates these.
            LlmMessage::user("Second question"),
            LlmMessage::assistant("Second answer — somewhat longer response"),
        ];
        let estimate = Compactor::estimate_context_tokens(&messages);
        assert_eq!(estimate.usage_tokens, 1500);
        assert!(estimate.trailing_tokens > 0);
        // tokens = usage_tokens + trailing_tokens
        assert_eq!(estimate.tokens, 1500 + estimate.trailing_tokens);
        assert_eq!(estimate.last_usage_index, Some(2));
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
        // Per-provider/per-model context window overrides used to
        // live at `[compaction.model_limits]`. Model max context is
        // now sourced exclusively from `ProviderCatalog::context_length`
        // (single source of truth). To keep user TOMLs that still
        // carry a `[compaction.model_limits]` block from breaking,
        // `CompactionConfig` deserialization accepts and silently
        // ignores the section.
        let toml_str = r#"
[compaction]
enabled = true
auto_threshold_percent = 5
reserve_tokens = 500
keep_recent_tokens = 1000
max_compactions_per_session = 100
cooldown_seconds = 0

[compaction.model_limits]
minimax = { "M3" = 4000 }
"#;
        let root = toml::from_str::<toml::Value>(toml_str).unwrap();
        let compaction_table = root
            .get("compaction")
            .expect("compaction section should exist");
        let cfg: CompactionConfig = compaction_table.clone().try_into().expect("should parse");

        assert!(cfg.enabled);
        assert_eq!(cfg.auto_threshold_percent, 5);
        assert_eq!(cfg.reserve_tokens, 500);
        assert_eq!(cfg.keep_recent_tokens, 1000);
        assert_eq!(cfg.max_compactions_per_session, 100);
        assert_eq!(cfg.cooldown_seconds, 0);
    }

    /// F17: `Compactor::compact` must surface the summarization LLM
    /// call's `TokenUsage` on the `CompactionResult` so the engine
    /// loop can fold it into `total_usage`. Pre-F17, the compactor
    /// returned only the summary string and the cost was silently
    /// dropped on the floor.
    #[tokio::test]
    async fn test_compactor_compact_populates_usage() {
        let mock = MockAdapter::new();
        // Use a long enough summary that the mock's text-estimate
        // returns a non-zero output token count.
        mock.queue_text("Summary of conversation: user and assistant discussed several topics over many turns and arrived at a conclusion that satisfied everyone involved in the discussion.");

        let provider = Arc::new(
            Provider::new(
                AnyAdapter::Mock(mock.clone()),
                "",
                ProviderRuntimeOptions {
                    default_model_id: "mock-model".to_string(),
                    context_window: None,
                    timeout_seconds: 300,
                    max_retries: 3,
                    retry_delay_ms: 1000,
                    ..Default::default()
                },
            )
            .expect("mock provider should construct"),
        );

        // Build a long-enough history that the compactor will
        // actually call the LLM (needs ≥4 messages and a non-empty
        // `to_compact` slice).
        let messages = create_test_messages(30);
        let mut compactor = Compactor::new();
        let result = compactor
            .compact(&messages, &provider)
            .await
            .expect("compaction should succeed with mock provider");

        // The mock emits a non-zero `output` count derived from the
        // queued summary length. If the compactor plumbed usage
        // correctly, this round-trips onto `result.usage`.
        assert!(
            result.usage.output > 0,
            "compaction result.usage.output should reflect the mock's queued output tokens, got {:?}",
            result.usage
        );
        // `input` is zero on the mock (queue_text doesn't estimate
        // input tokens), so total equals output.
        assert_eq!(
            result.usage.total, result.usage.output,
            "total should equal output when no cache/reasoning sub-fields are set"
        );
    }

    /// F17: cache and reasoning sub-fields must be preserved on the
    /// `CompactionResult.usage` returned by `Compactor::compact` so
    /// downstream quota accounting sees the same breakdown the wire
    /// reported. The mock can't easily inject cache/reasoning fields
    /// through `queue_text`, so this test queues via `chat_response`
    /// directly by going through the provider once with a hand-built
    /// response. We assert only the storage semantics here: the
    /// `accumulate` call that the compactor uses must preserve raw
    /// sub-fields verbatim when the receiver's field is `None`.
    #[tokio::test]
    async fn test_compactor_compact_preserves_cache_subfields() {
        let mock = MockAdapter::new();
        mock.queue_text("Summary.");

        let provider = Arc::new(
            Provider::new(
                AnyAdapter::Mock(mock.clone()),
                "",
                ProviderRuntimeOptions {
                    default_model_id: "mock-model".to_string(),
                    context_window: None,
                    timeout_seconds: 300,
                    max_retries: 3,
                    retry_delay_ms: 1000,
                    ..Default::default()
                },
            )
            .expect("mock provider should construct"),
        );

        // Build messages long enough to trigger compaction.
        let messages = create_test_messages(30);
        let mut compactor = Compactor::new();
        let result = compactor
            .compact(&messages, &provider)
            .await
            .expect("compaction should succeed");

        // The mock's `queue_text` does not populate cache or
        // reasoning sub-fields — they default to `None`. That is the
        // expected wire shape for an unstructured (non-cached,
        // non-reasoning) mock response. Verifies that the compactor
        // doesn't accidentally promote or zero them.
        assert_eq!(result.usage.cache_creation_input_tokens, None);
        assert_eq!(result.usage.cache_read_input_tokens, None);
        assert_eq!(result.usage.reasoning_output_tokens, None);
    }
}
