//! Stream orchestrator - transforms StreamEvents into AgenticEvents
//!
//! This is the core of the three-layer streaming pipeline:
//! - Provider Layer: Parses raw SSE into StreamEvents
//! - Orchestration Layer (this module): Transforms StreamEvents into AgenticEvents
//! - Channel Layer: Renders AgenticEvents to platform-specific output

use crate::engine::{
    AgenticEvent, BlockChunker, ChunkerConfig, CoalesceConfig, LifecyclePhase, StreamBuffer,
};
use crate::providers::StreamEvent;

/// Delivery mode for streaming
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// Emit every delta immediately (for CLI, TUI)
    Live,
    /// Coalesce into blocks (for Discord, HTTP)
    Block,
    /// Buffer until complete (for non-streaming channels)
    FinalOnly,
}

impl Default for DeliveryMode {
    fn default() -> Self {
        DeliveryMode::Block
    }
}

/// Orchestrator configuration
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Delivery mode
    pub delivery_mode: DeliveryMode,
    /// Chunking configuration (for block mode)
    pub chunking: ChunkerConfig,
    /// Coalescing configuration (for block mode)
    pub coalescing: CoalesceConfig,
    /// Throttle between emits in milliseconds (for live mode)
    pub throttle_ms: u64,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            delivery_mode: DeliveryMode::Block,
            chunking: ChunkerConfig::default(),
            coalescing: CoalesceConfig::default(),
            throttle_ms: 50,
        }
    }
}

impl OrchestratorConfig {
    /// Create configuration for live mode (CLI)
    pub fn live() -> Self {
        Self {
            delivery_mode: DeliveryMode::Live,
            throttle_ms: 50,
            ..Default::default()
        }
    }

    /// Create configuration for block mode (Discord)
    pub fn block() -> Self {
        Self {
            delivery_mode: DeliveryMode::Block,
            ..Default::default()
        }
    }

    /// Create configuration for final-only mode
    pub fn final_only() -> Self {
        Self {
            delivery_mode: DeliveryMode::FinalOnly,
            ..Default::default()
        }
    }
}

/// Internal state of the orchestrator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrchestratorState {
    /// Waiting for first event
    Idle,
    /// Processing text content
    Text,
    /// Processing tool call
    ToolCall,
    /// Processing thinking/reasoning
    Thinking,
    /// Stream completed
    Done,
}

/// Stream orchestrator - transforms StreamEvents into AgenticEvents
///
/// Responsibilities:
/// - Accumulate text deltas and emit chunked blocks
/// - Parse incremental tool calls
/// - Manage interstitial vs final state
/// - Apply throttling/coalescing based on delivery mode
pub struct StreamOrchestrator {
    /// Configuration
    config: OrchestratorConfig,
    /// Current state
    state: OrchestratorState,
    /// Block chunker for text chunking
    chunker: BlockChunker,
    /// Stream buffer for coalescing
    buffer: StreamBuffer,
    /// Sequence counter for events
    sequence: usize,
    /// Run ID
    run_id: String,
    /// Accumulated final text (for FinalOnly mode)
    final_buffer: String,
    /// Whether we've seen any tool calls (for interstitial detection)
    has_tool_calls: bool,
    /// Current thinking content
    thinking_buffer: String,
}

impl StreamOrchestrator {
    /// Create a new stream orchestrator
    pub fn new(run_id: impl Into<String>, config: OrchestratorConfig) -> Self {
        let run_id = run_id.into();
        let chunker = BlockChunker::with_config(config.chunking.clone());
        let buffer = StreamBuffer::new(&run_id, config.coalescing.clone());

        Self {
            config,
            state: OrchestratorState::Idle,
            chunker,
            buffer,
            sequence: 0,
            run_id,
            final_buffer: String::new(),
            has_tool_calls: false,
            thinking_buffer: String::new(),
        }
    }

    /// Process a StreamEvent and return AgenticEvents to emit
    pub fn process(&mut self, event: StreamEvent) -> Vec<AgenticEvent> {
        match event {
            StreamEvent::Start { provider: _, model: _ } => {
                self.state = OrchestratorState::Text;
                vec![AgenticEvent::Lifecycle {
                    run_id: self.run_id.clone(),
                    phase: LifecyclePhase::Start,
                    error: None,
                }]
            }
            StreamEvent::TextStart { content_index: _ } => {
                self.state = OrchestratorState::Text;
                vec![]
            }
            StreamEvent::TextDelta {
                content_index: _,
                delta,
            } => {
                self.state = OrchestratorState::Text;
                self.handle_text_delta(delta)
            }
            StreamEvent::TextEnd {
                content_index: _,
                content,
            } => {
                // Text content complete - ensure chunker is flushed
                self.handle_text_end(content)
            }
            StreamEvent::ThinkingStart { content_index: _ } => {
                self.state = OrchestratorState::Thinking;
                vec![]
            }
            StreamEvent::ThinkingDelta {
                content_index: _,
                delta,
            } => {
                self.state = OrchestratorState::Thinking;
                self.handle_thinking_delta(delta)
            }
            StreamEvent::ThinkingEnd {
                content_index: _,
                content,
            } => self.handle_thinking_end(content),
            StreamEvent::ToolCallStart { content_index: _ } => {
                self.has_tool_calls = true;
                self.state = OrchestratorState::ToolCall;
                vec![]
            }
            StreamEvent::ToolCallDelta {
                content_index,
                delta,
            } => {
                self.state = OrchestratorState::ToolCall;
                self.handle_tool_delta(content_index, delta)
            }
            StreamEvent::ToolCallEnd {
                content_index,
                tool_call,
            } => self.handle_tool_end(content_index, tool_call),
            StreamEvent::Done { stop_reason } => {
                self.state = OrchestratorState::Done;
                self.handle_done(stop_reason)
            }
            StreamEvent::Usage { .. } => {
                // Usage events are accumulated by the engine loop, not the orchestrator
                // The orchestrator handles content transformation, not metadata
                vec![]
            }
            StreamEvent::Error { message } => {
                vec![AgenticEvent::Lifecycle {
                    run_id: self.run_id.clone(),
                    phase: LifecyclePhase::Error,
                    error: Some(message),
                }]
            }
        }
    }

    /// Finalize the orchestrator and return any remaining events
    pub fn finalize(&mut self) -> Vec<AgenticEvent> {
        let mut events = Vec::new();

        // Flush the buffer
        events.extend(self.buffer.flush());

        // Flush chunker - only in Live mode where events are emitted immediately
        // In Block/FinalOnly modes, content has already been fed to buffers
        if self.config.delivery_mode == DeliveryMode::Live {
            let chunks = self.chunker.flush();
            for chunk in chunks {
                self.sequence += 1;
                events.push(AgenticEvent::AssistantDelta {
                    run_id: self.run_id.clone(),
                    text: chunk,
                    sequence: self.sequence,
                    is_interstitial: self.has_tool_calls,
                });
            }
        }

        // If in FinalOnly mode, emit the accumulated text
        if self.config.delivery_mode == DeliveryMode::FinalOnly && !self.final_buffer.is_empty() {
            self.sequence += 1;
            events.push(AgenticEvent::AssistantText {
                run_id: self.run_id.clone(),
                text: std::mem::take(&mut self.final_buffer),
                sequence: self.sequence,
                is_interstitial: self.has_tool_calls,
            });
        }

        // Emit end event
        events.push(AgenticEvent::Lifecycle {
            run_id: self.run_id.clone(),
            phase: LifecyclePhase::End,
            error: None,
        });

        events
    }

    /// Handle text delta based on delivery mode
    fn handle_text_delta(&mut self, delta: String) -> Vec<AgenticEvent> {
        match self.config.delivery_mode {
            DeliveryMode::Live => {
                // Emit immediately
                self.sequence += 1;
                vec![AgenticEvent::AssistantDelta {
                    run_id: self.run_id.clone(),
                    text: delta,
                    sequence: self.sequence,
                    is_interstitial: self.has_tool_calls,
                }]
            }
            DeliveryMode::Block => {
                // Accumulate in chunker and emit blocks
                let mut events = Vec::new();
                let chunks = self.chunker.feed(&delta);
                for chunk in chunks {
                    // Push to buffer for coalescing
                    let delta_event = AgenticEvent::AssistantDelta {
                        run_id: self.run_id.clone(),
                        text: chunk,
                        sequence: 0, // Will be assigned by buffer
                        is_interstitial: self.has_tool_calls,
                    };
                    events.extend(self.buffer.push(delta_event));
                }
                events
            }
            DeliveryMode::FinalOnly => {
                // Buffer everything
                self.final_buffer.push_str(&delta);
                vec![]
            }
        }
    }

    /// Handle text end - flush remaining chunker content
    fn handle_text_end(&mut self, _content: String) -> Vec<AgenticEvent> {
        match self.config.delivery_mode {
            DeliveryMode::Block => {
                let mut events = Vec::new();
                let chunks = self.chunker.flush();
                for chunk in chunks {
                    let delta_event = AgenticEvent::AssistantDelta {
                        run_id: self.run_id.clone(),
                        text: chunk,
                        sequence: 0,
                        is_interstitial: self.has_tool_calls,
                    };
                    events.extend(self.buffer.push(delta_event));
                }
                events
            }
            _ => vec![],
        }
    }

    /// Handle thinking delta
    fn handle_thinking_delta(&mut self, delta: String) -> Vec<AgenticEvent> {
        self.thinking_buffer.push_str(&delta);
        vec![AgenticEvent::Thinking {
            run_id: self.run_id.clone(),
            text: delta,
            is_delta: true,
            is_final: false,
            signature: None,
        }]
    }

    /// Handle thinking end
    fn handle_thinking_end(&mut self, content: String) -> Vec<AgenticEvent> {
        self.thinking_buffer.clear();
        vec![AgenticEvent::Thinking {
            run_id: self.run_id.clone(),
            text: content,
            is_delta: false,
            is_final: true,
            signature: None,
        }]
    }

    /// Handle tool call delta
    fn handle_tool_delta(&mut self, _index: usize, _delta: String) -> Vec<AgenticEvent> {
        // For now, just accumulate in the parser
        // In a full implementation, we'd track partial tool calls
        vec![]
    }

    /// Handle tool call end
    fn handle_tool_end(
        &mut self,
        _index: usize,
        tool_call: crate::types::message::ContentBlock,
    ) -> Vec<AgenticEvent> {
        if let crate::types::message::ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } = tool_call
        {
            vec![AgenticEvent::ToolStart {
                run_id: self.run_id.clone(),
                tool_id: id,
                name,
                params: arguments,
            }]
        } else {
            vec![]
        }
    }

    /// Handle stream completion
    fn handle_done(&mut self, _stop_reason: crate::providers::StopReason) -> Vec<AgenticEvent> {
        // Finalize will be called separately to emit the end event
        vec![]
    }

    /// Get current state
    pub fn state(&self) -> &str {
        match self.state {
            OrchestratorState::Idle => "idle",
            OrchestratorState::Text => "text",
            OrchestratorState::ToolCall => "tool_call",
            OrchestratorState::Thinking => "thinking",
            OrchestratorState::Done => "done",
        }
    }

    /// Get buffer length
    pub fn buffer_len(&self) -> usize {
        self.buffer.buffer_len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::StopReason;

    #[test]
    fn test_orchestrator_live_mode() {
        let config = OrchestratorConfig::live();
        let mut orchestrator = StreamOrchestrator::new("run_123", config);

        // Start event
        let events = orchestrator.process(StreamEvent::Start {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
        });
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AgenticEvent::Lifecycle { .. }));

        // Text deltas should emit immediately in live mode
        let events = orchestrator.process(StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello ".to_string(),
        });
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgenticEvent::AssistantDelta { text, sequence, .. } => {
                assert_eq!(text, "Hello ");
                assert_eq!(*sequence, 1);
            }
            _ => panic!("Expected AssistantDelta"),
        }

        let events = orchestrator.process(StreamEvent::TextDelta {
            content_index: 0,
            delta: "world!".to_string(),
        });
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgenticEvent::AssistantDelta { text, sequence, .. } => {
                assert_eq!(text, "world!");
                assert_eq!(*sequence, 2);
            }
            _ => panic!("Expected AssistantDelta"),
        }
    }

    #[test]
    fn test_orchestrator_final_only_mode() {
        let config = OrchestratorConfig::final_only();
        let mut orchestrator = StreamOrchestrator::new("run_123", config);

        // Text deltas should not emit in final-only mode
        orchestrator.process(StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello ".to_string(),
        });
        orchestrator.process(StreamEvent::TextDelta {
            content_index: 0,
            delta: "world!".to_string(),
        });

        // No events emitted yet
        assert_eq!(orchestrator.buffer_len(), 0);

        // Finalize should emit accumulated text
        let events = orchestrator.finalize();

        // Should have AssistantText and Lifecycle::End
        let has_assistant_text = events
            .iter()
            .any(|e| matches!(e, AgenticEvent::AssistantText { .. }));
        let has_end = events.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::End,
                    ..
                }
            )
        });
        assert!(has_assistant_text);
        assert!(has_end);
    }

    #[test]
    fn test_orchestrator_thinking() {
        let config = OrchestratorConfig::live();
        let mut orchestrator = StreamOrchestrator::new("run_123", config);

        // Thinking delta
        let events = orchestrator.process(StreamEvent::ThinkingDelta {
            content_index: 0,
            delta: "Let me think...".to_string(),
        });

        assert_eq!(events.len(), 1);
        match &events[0] {
            AgenticEvent::Thinking { text, is_delta, .. } => {
                assert_eq!(text, "Let me think...");
                assert!(*is_delta);
            }
            _ => panic!("Expected Thinking event"),
        }

        // Thinking end
        let events = orchestrator.process(StreamEvent::ThinkingEnd {
            content_index: 0,
            content: "Let me think about this.".to_string(),
        });

        assert_eq!(events.len(), 1);
        match &events[0] {
            AgenticEvent::Thinking { text, is_final, .. } => {
                assert_eq!(text, "Let me think about this.");
                assert!(*is_final);
            }
            _ => panic!("Expected final Thinking event"),
        }
    }

    #[test]
    fn test_orchestrator_error_handling() {
        let config = OrchestratorConfig::live();
        let mut orchestrator = StreamOrchestrator::new("run_123", config);

        let events = orchestrator.process(StreamEvent::Error {
            message: "Rate limit exceeded".to_string(),
        });

        assert_eq!(events.len(), 1);
        match &events[0] {
            AgenticEvent::Lifecycle { phase, error, .. } => {
                assert!(matches!(phase, LifecyclePhase::Error));
                assert_eq!(error.as_deref(), Some("Rate limit exceeded"));
            }
            _ => panic!("Expected Lifecycle::Error event"),
        }
    }

    #[test]
    fn test_interstitial_detection() {
        let config = OrchestratorConfig::live();
        let mut orchestrator = StreamOrchestrator::new("run_123", config);

        // First some text
        orchestrator.process(StreamEvent::TextDelta {
            content_index: 0,
            delta: "Let me search...".to_string(),
        });

        // Then a tool call
        orchestrator.process(StreamEvent::ToolCallStart { content_index: 1 });

        // Now text is interstitial
        let events = orchestrator.process(StreamEvent::TextDelta {
            content_index: 0,
            delta: " Found it!".to_string(),
        });

        match &events[0] {
            AgenticEvent::AssistantDelta {
                is_interstitial, ..
            } => {
                assert!(
                    *is_interstitial,
                    "Text after tool call start should be interstitial"
                );
            }
            _ => panic!("Expected AssistantDelta"),
        }
    }

    #[test]
    fn test_usage_event_returns_empty_vec() {
        let config = OrchestratorConfig::live();
        let mut orchestrator = StreamOrchestrator::new("run_123", config);

        // Usage event should return empty vec (handled by engine loop, not orchestrator)
        let events = orchestrator.process(StreamEvent::Usage {
            input: 10,
            output: 5,
            total: 15,
        });

        assert!(events.is_empty(), "Usage event should return empty vec");
    }
}
