//! Session Pruning - In-memory context trimming before LLM calls
//!
//! Complements compaction by trimming tool results in-memory before each request.
//! Unlike compaction which persists summaries, pruning is per-request and doesn't
//! modify the stored history.

use crate::types::provider::ChatMessage;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Pruning configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruningConfig {
    /// Enable pruning
    pub enabled: bool,
    /// Maximum tool result length (chars)
    pub max_tool_result_length: usize,
    /// Truncation indicator
    pub truncation_indicator: String,
    /// Always keep N most recent tool results intact
    pub keep_recent_tool_results: usize,
    /// Prune older tool results when context exceeds this (tokens)
    pub prune_threshold_tokens: usize,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tool_result_length: 2000,
            truncation_indicator: "... [truncated]".to_string(),
            keep_recent_tool_results: 3,
            prune_threshold_tokens: 60000,
        }
    }
}

/// Pruner for in-memory context trimming
pub struct Pruner {
    config: PruningConfig,
}

impl Pruner {
    /// Create a new pruner with default config
    #[must_use] 
    pub fn new() -> Self {
        Self::with_config(PruningConfig::default())
    }

    /// Create with custom config
    #[must_use] 
    pub fn with_config(config: PruningConfig) -> Self {
        Self { config }
    }

    /// Check if pruning is needed based on token count
    #[must_use] 
    pub fn should_prune(&self,
        _messages: &[ChatMessage],
        estimated_tokens: usize,
    ) -> bool {
        if !self.config.enabled {
            return false;
        }

        estimated_tokens > self.config.prune_threshold_tokens
    }

    /// Prune messages in-place
    /// 
    /// Strategy:
    /// 1. Keep system messages intact
    /// 2. Keep N most recent tool results intact
    /// 3. Truncate older tool results
    /// 4. Keep user/assistant messages intact (except for truncation)
    pub fn prune(&self,
        messages: &mut Vec<ChatMessage>,
    ) -> PruningResult {
        if !self.config.enabled {
            return PruningResult::no_change();
        }

        let original_count = messages.len();
        let pruned_count = 0;
        let mut truncated_count = 0;
        let mut tokens_saved = 0;

        // Find tool messages and their indices
        let tool_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "tool")
            .map(|(i, _)| i)
            .collect();

        if tool_indices.is_empty() {
            return PruningResult::no_change();
        }

        // Determine which tool results to keep vs prune
        let keep_count = self.config.keep_recent_tool_results.min(tool_indices.len());
        let prune_start = tool_indices.len().saturating_sub(keep_count);

        for (idx, &msg_idx) in tool_indices.iter().enumerate() {
            if idx < prune_start {
                // This tool result should be pruned
                if let Some(msg) = messages.get_mut(msg_idx) {
                    let original_len = msg.content.len();
                    
                    if original_len > self.config.max_tool_result_length {
                        let truncated = self.truncate_content(&msg.content);
                        let new_len = truncated.len();
                        
                        msg.content = truncated;
                        truncated_count += 1;
                        tokens_saved += (original_len - new_len) / 4; // Rough token estimate
                    }
                }
            }
        }

        let result = PruningResult {
            original_message_count: original_count,
            pruned_tool_results: pruned_count,
            truncated_tool_results: truncated_count,
            estimated_tokens_saved: tokens_saved,
            did_prune: truncated_count > 0,
        };

        if result.did_prune {
            debug!(
                "Pruned {} tool results, saved ~{} tokens",
                result.truncated_tool_results,
                result.estimated_tokens_saved
            );
        }

        result
    }

    /// Truncate content to max length
    fn truncate_content(&self,
        content: &str,
    ) -> String {
        if content.len() <= self.config.max_tool_result_length {
            return content.to_string();
        }

        let keep_chars = self.config.max_tool_result_length
            .saturating_sub(self.config.truncation_indicator.len());
        
        let mut truncated: String = content.chars().take(keep_chars).collect();
        truncated.push_str(&self.config.truncation_indicator);
        
        truncated
    }

    /// Quick estimate of tokens in messages
    #[must_use] 
    pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
        messages.iter().map(|m| {
            // Rough estimate: 4 chars ≈ 1 token
            (m.content.len() + m.role.len()) / 4 + 4
        }).sum()
    }

    /// Get status
    #[must_use] 
    pub fn status(&self) -> String {
        format!(
            "✂️  Pruning: {} | Max tool result: {} chars | Keep recent: {}",
            if self.config.enabled { "enabled" } else { "disabled" },
            self.config.max_tool_result_length,
            self.config.keep_recent_tool_results
        )
    }
}

impl Default for Pruner {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of pruning operation
#[derive(Debug, Clone)]
pub struct PruningResult {
    /// Original message count
    pub original_message_count: usize,
    /// Number of tool results removed entirely
    pub pruned_tool_results: usize,
    /// Number of tool results truncated
    pub truncated_tool_results: usize,
    /// Estimated tokens saved
    pub estimated_tokens_saved: usize,
    /// Whether any pruning occurred
    pub did_prune: bool,
}

impl PruningResult {
    /// Create a "no change" result
    fn no_change() -> Self {
        Self {
            original_message_count: 0,
            pruned_tool_results: 0,
            truncated_tool_results: 0,
            estimated_tokens_saved: 0,
            did_prune: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_messages_with_tools() -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("You are a helper"),
            ChatMessage::user("Do task 1"),
            ChatMessage::assistant("[tool: do_something]"),
            ChatMessage {
                role: "tool".to_string(),
                content: "a".repeat(5000), // Long tool result
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("do_something".to_string()),
            },
            ChatMessage::user("Do task 2"),
            ChatMessage::assistant("[tool: do_another]"),
            ChatMessage {
                role: "tool".to_string(),
                content: "b".repeat(5000), // Long tool result
                tool_calls: None,
                tool_call_id: Some("call_2".to_string()),
                name: Some("do_another".to_string()),
            },
        ]
    }

    #[test]
    fn test_prune_disabled() {
        let mut config = PruningConfig::default();
        config.enabled = false;
        let pruner = Pruner::with_config(config);
        
        let mut messages = create_test_messages_with_tools();
        let result = pruner.prune(&mut messages);
        
        assert!(!result.did_prune);
        assert_eq!(result.truncated_tool_results, 0);
    }

    #[test]
    fn test_truncate_long_tool_results() {
        let mut pruner = Pruner::new();
        pruner.config.max_tool_result_length = 100;
        pruner.config.keep_recent_tool_results = 1;
        
        let mut messages = create_test_messages_with_tools();
        let original_tool_len = messages[3].content.len();
        
        let result = pruner.prune(&mut messages);
        
        assert!(result.did_prune);
        assert!(result.truncated_tool_results > 0);
        assert!(messages[3].content.len() < original_tool_len);
        assert!(messages[3].content.contains("[truncated]"));
    }

    #[test]
    fn test_keep_recent_tool_results() {
        let mut pruner = Pruner::new();
        pruner.config.max_tool_result_length = 100;
        pruner.config.keep_recent_tool_results = 1;
        
        let mut messages = create_test_messages_with_tools();
        let original_last_tool = messages[6].content.clone();
        
        pruner.prune(&mut messages);
        
        // Most recent tool result should be unchanged
        assert_eq!(messages[6].content, original_last_tool);
    }

    #[test]
    fn test_estimate_tokens() {
        let messages = create_test_messages_with_tools();
        let tokens = Pruner::estimate_tokens(&messages);
        
        assert!(tokens > 0);
        // Rough check: 5000 chars / 4 = ~1250 tokens per long message
        assert!(tokens > 2000);
    }

    #[test]
    fn test_truncate_content() {
        let pruner = Pruner::with_config(PruningConfig {
            max_tool_result_length: 50,
            truncation_indicator: "...".to_string(),
            ..Default::default()
        });
        
        let content = "a".repeat(100);
        let truncated = pruner.truncate_content(&content);
        
        assert!(truncated.len() <= 50);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_short_content_not_truncated() {
        let pruner = Pruner::new();
        let content = "Short content";
        
        let truncated = pruner.truncate_content(content);
        assert_eq!(truncated, content);
    }
}
