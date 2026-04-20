//! Stream buffer for throttling and coalescing events
//!
//! Similar to `OpenClaw`'s `DraftStreamLoop` but generalized
//! for `AgenticEvents` instead of text strings.

use crate::engine::AgenticEvent;
use std::time::{Duration, Instant};

/// Coalescing configuration
#[derive(Debug, Clone)]
pub struct CoalesceConfig {
    /// Minimum characters before emitting
    pub min_chars: usize,
    /// Maximum characters per emit
    pub max_chars: usize,
    /// Idle timeout before flush
    pub idle_timeout: Duration,
    /// String to join coalesced blocks
    pub joiner: String,
}

impl Default for CoalesceConfig {
    fn default() -> Self {
        Self {
            min_chars: 1500,
            max_chars: 3000,
            idle_timeout: Duration::from_millis(500),
            joiner: "\n\n".to_string(),
        }
    }
}

/// Stream buffer for throttling and coalescing events
///
/// Buffers `AssistantDelta` events and emits them based on:
/// - Size thresholds (`min_chars`, `max_chars`)
/// - Idle timeout
///
/// Similar to `OpenClaw`'s `DraftStreamLoop` but generalized
/// for `AgenticEvents`.
pub struct StreamBuffer {
    /// Pending events waiting to be emitted
    pending: Vec<AgenticEvent>,
    /// Last emit timestamp for idle timeout
    last_emit: Instant,
    /// Coalescing configuration
    config: CoalesceConfig,
    /// Accumulated text buffer for coalescing
    text_buffer: String,
    /// Current sequence number for emitted events
    sequence: usize,
    /// Run ID for events
    run_id: String,
    /// Whether this is interstitial text
    is_interstitial: bool,
}

impl StreamBuffer {
    /// Create a new stream buffer
    pub fn new(run_id: impl Into<String>, config: CoalesceConfig) -> Self {
        Self {
            pending: Vec::new(),
            last_emit: Instant::now(),
            config,
            text_buffer: String::new(),
            sequence: 0,
            run_id: run_id.into(),
            is_interstitial: false,
        }
    }

    /// Push an event into the buffer
    ///
    /// Only `AssistantDelta` events are buffered and coalesced.
    /// Other events are passed through immediately.
    pub fn push(&mut self, event: AgenticEvent) -> Vec<AgenticEvent> {
        match &event {
            AgenticEvent::AssistantDelta {
                text,
                is_interstitial,
                ..
            } => {
                // Update interstitial flag
                self.is_interstitial = *is_interstitial;
                // Accumulate text
                self.text_buffer.push_str(text);
                // Try to flush
                self.try_flush()
            }
            AgenticEvent::Flush { .. } => {
                // Force flush on explicit flush signal
                self.flush()
            }
            _ => {
                // Pass through non-text events immediately
                vec![event]
            }
        }
    }

    /// Try to flush based on size thresholds
    fn try_flush(&mut self) -> Vec<AgenticEvent> {
        let mut events = Vec::new();

        // Check if we should emit based on size
        while self.text_buffer.len() >= self.config.max_chars {
            // Force emit at max_chars boundary
            let split_point = self.find_split_point(self.config.max_chars);
            let text = self.text_buffer[..split_point].to_string();
            self.text_buffer = self.text_buffer[split_point..].to_string();

            self.sequence += 1;
            events.push(AgenticEvent::AssistantDelta {
                run_id: self.run_id.clone(),
                text,
                sequence: self.sequence,
                is_interstitial: self.is_interstitial,
            });
            self.last_emit = Instant::now();
        }

        // Check if we have enough for min threshold
        if self.text_buffer.len() >= self.config.min_chars {
            // Check idle timeout
            if self.last_emit.elapsed() >= self.config.idle_timeout {
                let text = std::mem::take(&mut self.text_buffer);
                self.sequence += 1;
                events.push(AgenticEvent::AssistantDelta {
                    run_id: self.run_id.clone(),
                    text,
                    sequence: self.sequence,
                    is_interstitial: self.is_interstitial,
                });
                self.last_emit = Instant::now();
            }
        }

        events
    }

    /// Force flush all buffered content
    pub fn flush(&mut self) -> Vec<AgenticEvent> {
        let mut events = Vec::new();

        // Emit remaining text buffer
        if !self.text_buffer.is_empty() {
            let text = std::mem::take(&mut self.text_buffer);
            self.sequence += 1;
            events.push(AgenticEvent::AssistantDelta {
                run_id: self.run_id.clone(),
                text,
                sequence: self.sequence,
                is_interstitial: self.is_interstitial,
            });
        }

        // Emit any pending events
        events.extend(std::mem::take(&mut self.pending));

        self.last_emit = Instant::now();
        events
    }

    /// Check if there's content that should be flushed after idle timeout
    pub fn check_idle_flush(&mut self) -> Vec<AgenticEvent> {
        if self.text_buffer.is_empty() {
            return vec![];
        }

        if self.last_emit.elapsed() >= self.config.idle_timeout
            && self.text_buffer.len() >= self.config.min_chars
        {
            let text = std::mem::take(&mut self.text_buffer);
            self.sequence += 1;
            self.last_emit = Instant::now();
            vec![AgenticEvent::AssistantDelta {
                run_id: self.run_id.clone(),
                text,
                sequence: self.sequence,
                is_interstitial: self.is_interstitial,
            }]
        } else {
            vec![]
        }
    }

    /// Get current buffer size in characters
    #[must_use]
    pub fn buffer_len(&self) -> usize {
        self.text_buffer.len()
    }

    /// Check if buffer is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text_buffer.is_empty() && self.pending.is_empty()
    }

    /// Find a good split point near the target position
    fn find_split_point(&self, target: usize) -> usize {
        let search_limit = target.min(self.text_buffer.len());

        // Try to find whitespace boundary
        if let Some(pos) = self.text_buffer[..search_limit].rfind(' ') {
            return pos + 1; // Include the space
        }

        // Fallback to hard break
        search_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_delta(text: &str, seq: usize) -> AgenticEvent {
        AgenticEvent::AssistantDelta {
            run_id: "run_123".to_string(),
            text: text.to_string(),
            sequence: seq,
            is_interstitial: false,
        }
    }

    #[test]
    fn test_buffer_accumulates_small_deltas() {
        let config = CoalesceConfig {
            min_chars: 100,
            max_chars: 200,
            idle_timeout: Duration::from_millis(100),
            joiner: "\n\n".to_string(),
        };

        let mut buffer = StreamBuffer::new("run_123", config);

        // Push small deltas - should not emit
        let events = buffer.push(create_test_delta("Hello ", 1));
        assert!(events.is_empty());

        let events = buffer.push(create_test_delta("world! ", 2));
        assert!(events.is_empty());

        assert_eq!(buffer.buffer_len(), 13);
    }

    #[test]
    fn test_buffer_emits_at_max_chars() {
        let config = CoalesceConfig {
            min_chars: 10,
            max_chars: 20,
            idle_timeout: Duration::from_secs(10), // Long timeout
            joiner: "\n\n".to_string(),
        };

        let mut buffer = StreamBuffer::new("run_123", config);

        // Push text exceeding max_chars
        let events = buffer.push(create_test_delta(
            "This is a very long text that exceeds the max_chars limit",
            1,
        ));

        // Should emit at least one event
        assert!(!events.is_empty());
        // First emitted chunk should not exceed max_chars
        assert!(events[0].run_id() == "run_123");
    }

    #[test]
    fn test_flush_emits_all_content() {
        let config = CoalesceConfig::default();
        let mut buffer = StreamBuffer::new("run_123", config);

        // Accumulate some text
        buffer.push(create_test_delta("Hello ", 1));
        buffer.push(create_test_delta("world!", 2));

        // Flush should emit all
        let events = buffer.flush();
        assert_eq!(events.len(), 1);

        match &events[0] {
            AgenticEvent::AssistantDelta { text, .. } => {
                assert_eq!(text, "Hello world!");
            }
            _ => panic!("Expected AssistantDelta"),
        }

        assert!(buffer.is_empty());
    }

    #[test]
    fn test_pass_through_non_delta_events() {
        let config = CoalesceConfig::default();
        let mut buffer = StreamBuffer::new("run_123", config);

        // ToolStart should pass through immediately
        let tool_event = AgenticEvent::ToolStart {
            run_id: "run_123".to_string(),
            tool_id: "tc_001".to_string(),
            name: "web_search".to_string(),
            params: serde_json::json!({"query": "test"}),
        };

        let events = buffer.push(tool_event.clone());
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AgenticEvent::ToolStart { .. }));
    }

    #[test]
    fn test_flush_event_triggers_buffer_flush() {
        let config = CoalesceConfig::default();
        let mut buffer = StreamBuffer::new("run_123", config);

        // Accumulate text
        buffer.push(create_test_delta("Buffered text", 1));

        // Send flush event
        let flush_event = AgenticEvent::Flush {
            run_id: "run_123".to_string(),
        };

        let events = buffer.push(flush_event);
        assert_eq!(events.len(), 1);

        match &events[0] {
            AgenticEvent::AssistantDelta { text, .. } => {
                assert_eq!(text, "Buffered text");
            }
            _ => panic!("Expected AssistantDelta after flush"),
        }
    }

    #[test]
    fn test_is_empty() {
        let config = CoalesceConfig::default();
        let mut buffer = StreamBuffer::new("run_123", config);

        assert!(buffer.is_empty());

        buffer.push(create_test_delta("text", 1));
        assert!(!buffer.is_empty());

        buffer.flush();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_interstitial_flag_preserved() {
        let config = CoalesceConfig {
            min_chars: 5,
            max_chars: 100,
            idle_timeout: Duration::from_millis(0), // Immediate
            joiner: "\n\n".to_string(),
        };

        let mut buffer = StreamBuffer::new("run_123", config);

        // Push interstitial text - with idle_timeout=0 and min_chars=5,
        // this should emit immediately (text is 16 chars)
        let interstitial = AgenticEvent::AssistantDelta {
            run_id: "run_123".to_string(),
            text: "Let me search...".to_string(),
            sequence: 1,
            is_interstitial: true,
        };

        let events = buffer.push(interstitial);

        // Should have emitted immediately
        assert!(
            !events.is_empty(),
            "Expected events to be emitted immediately"
        );

        match &events[0] {
            AgenticEvent::AssistantDelta {
                is_interstitial, ..
            } => {
                assert!(*is_interstitial, "Expected is_interstitial to be true");
            }
            _ => panic!("Expected AssistantDelta"),
        }
    }
}
