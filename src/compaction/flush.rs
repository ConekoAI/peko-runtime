//! Pre-compaction Memory Flush
//!
//! Before context is compacted, trigger a silent turn that reminds the model
//! to write durable notes to memory. This prevents loss of important context.

use crate::types::provider::ChatMessage;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Configuration for memory flush before compaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFlushConfig {
    /// Enable pre-compaction flush
    pub enabled: bool,
    /// Soft threshold tokens to trigger flush
    pub soft_threshold_tokens: usize,
    /// System prompt for flush turn
    pub system_prompt: String,
    /// User prompt for flush turn
    pub prompt: String,
}

impl Default for MemoryFlushConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            soft_threshold_tokens: 4000,
            system_prompt: "Session nearing compaction. Store durable memories now.".to_string(),
            prompt: "Write any lasting notes to memory/YYYY-MM-DD.md; reply with NO_REPLY if nothing to store.".to_string(),
        }
    }
}

/// Tracks flush state per session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlushState {
    /// Whether flush was performed for current compaction cycle
    pub flushed_this_cycle: bool,
    /// Last flush timestamp
    pub last_flush_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Number of flushes performed
    pub flush_count: usize,
}

/// Memory flusher for pre-compaction
pub struct MemoryFlusher {
    config: MemoryFlushConfig,
    state: FlushState,
    workspace_writable: bool,
}

impl MemoryFlusher {
    /// Create a new memory flusher
    #[must_use]
    pub fn new(workspace_writable: bool) -> Self {
        Self::with_config(MemoryFlushConfig::default(), workspace_writable)
    }

    /// Create with custom config
    #[must_use]
    pub fn with_config(config: MemoryFlushConfig, workspace_writable: bool) -> Self {
        Self {
            config,
            state: FlushState::default(),
            workspace_writable,
        }
    }

    /// Check if flush should be triggered
    ///
    /// Returns true when:
    /// - Enabled in config
    /// - Workspace is writable
    /// - Token count crosses soft threshold
    /// - Not already flushed this cycle
    pub fn should_flush(
        &self,
        current_tokens: usize,
        context_window: usize,
        reserve_tokens_floor: usize,
    ) -> bool {
        if !self.config.enabled {
            debug!("Memory flush disabled");
            return false;
        }

        if !self.workspace_writable {
            debug!("Workspace not writable, skipping flush");
            return false;
        }

        if self.state.flushed_this_cycle {
            debug!("Already flushed this cycle");
            return false;
        }

        let threshold =
            context_window.saturating_sub(reserve_tokens_floor + self.config.soft_threshold_tokens);

        let should = current_tokens >= threshold;

        if should {
            info!(
                "Memory flush triggered: {} tokens >= {} threshold",
                current_tokens, threshold
            );
        }

        should
    }

    /// Create the flush prompt messages
    ///
    /// These messages are injected to remind the model to store memories
    #[must_use]
    pub fn create_flush_messages(&self) -> Vec<ChatMessage> {
        vec![
            ChatMessage::system(self.config.system_prompt.as_str()),
            ChatMessage::user(self.config.prompt.as_str()),
        ]
    }

    /// Mark flush as performed
    pub fn mark_flushed(&mut self) {
        self.state.flushed_this_cycle = true;
        self.state.last_flush_at = Some(chrono::Utc::now());
        self.state.flush_count += 1;

        info!(
            "Memory flush marked complete (total flushes: {})",
            self.state.flush_count
        );
    }

    /// Reset flush state for new compaction cycle
    pub fn reset_cycle(&mut self) {
        if self.state.flushed_this_cycle {
            debug!("Resetting flush state for new cycle");
            self.state.flushed_this_cycle = false;
        }
    }

    /// Check if a response indicates `NO_REPLY` (silent)
    #[must_use]
    pub fn is_no_reply(response: &str) -> bool {
        let trimmed = response.trim().to_uppercase();
        trimmed == "NO_REPLY" || trimmed.contains("NO_REPLY")
    }

    /// Get current state
    #[must_use]
    pub fn state(&self) -> &FlushState {
        &self.state
    }

    /// Get status summary
    #[must_use]
    pub fn status(&self) -> String {
        format!(
            "💾 Memory flushes: {} | Last: {} | Writable: {}",
            self.state.flush_count,
            self.state
                .last_flush_at
                .map_or_else(|| "Never".to_string(), |t| t.format("%H:%M").to_string()),
            self.workspace_writable
        )
    }
}

impl Default for MemoryFlusher {
    fn default() -> Self {
        Self::new(true)
    }
}

/// Combined compaction + flush configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionWithFlush {
    /// Reserve tokens floor
    pub reserve_tokens_floor: usize,
    /// Memory flush configuration
    pub memory_flush: MemoryFlushConfig,
}

impl CompactionWithFlush {
    /// Create default configuration
    #[must_use]
    pub fn default_config() -> Self {
        Self {
            reserve_tokens_floor: 20000,
            memory_flush: MemoryFlushConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_flush_enabled() {
        let flusher = MemoryFlusher::new(true);

        // Should flush when crossing threshold
        let context_window = 100000;
        let reserve = 20000;
        let tokens = 80000; // Above threshold

        assert!(flusher.should_flush(tokens, context_window, reserve));
    }

    #[test]
    fn test_should_flush_disabled() {
        let mut flusher = MemoryFlusher::new(true);
        flusher.config.enabled = false;

        assert!(!flusher.should_flush(90000, 100000, 20000));
    }

    #[test]
    fn test_should_flush_not_writable() {
        let flusher = MemoryFlusher::new(false);

        assert!(!flusher.should_flush(90000, 100000, 20000));
    }

    #[test]
    fn test_should_flush_already_flushed() {
        let mut flusher = MemoryFlusher::new(true);
        flusher.state.flushed_this_cycle = true;

        assert!(!flusher.should_flush(90000, 100000, 20000));
    }

    #[test]
    fn test_is_no_reply() {
        assert!(MemoryFlusher::is_no_reply("NO_REPLY"));
        assert!(MemoryFlusher::is_no_reply("no_reply"));
        assert!(MemoryFlusher::is_no_reply("  NO_REPLY  "));
        assert!(!MemoryFlusher::is_no_reply("I have stored the memory"));
    }

    #[test]
    fn test_flush_messages() {
        let flusher = MemoryFlusher::new(true);
        let messages = flusher.create_flush_messages();

        assert_eq!(messages.len(), 2);
        assert!(messages[0].content.contains("compaction"));
        assert!(messages[1].content.contains("memory"));
    }

    #[test]
    fn test_mark_flushed() {
        let mut flusher = MemoryFlusher::new(true);

        assert_eq!(flusher.state.flush_count, 0);
        assert!(!flusher.state.flushed_this_cycle);

        flusher.mark_flushed();

        assert_eq!(flusher.state.flush_count, 1);
        assert!(flusher.state.flushed_this_cycle);
        assert!(flusher.state.last_flush_at.is_some());
    }

    #[test]
    fn test_reset_cycle() {
        let mut flusher = MemoryFlusher::new(true);
        flusher.mark_flushed();

        assert!(flusher.state.flushed_this_cycle);

        flusher.reset_cycle();

        assert!(!flusher.state.flushed_this_cycle);
    }

    #[test]
    fn test_status() {
        let flusher = MemoryFlusher::new(true);
        let status = flusher.status();

        assert!(status.contains("Memory flushes"));
        assert!(status.contains("Writable: true"));
    }
}
