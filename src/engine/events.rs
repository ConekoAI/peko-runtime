//! Agentic event system for streaming
//!
//! Events are emitted during agent execution to provide
//! real-time visibility into the agent's work.

use serde_json::Value;
use std::fmt;

/// Unique identifier for tool executions
pub type ToolId = String;

/// Unique identifier for tool calls (streaming construction)
pub type ToolCallId = String;

/// Unique identifier for agent runs
pub type RunId = String;

/// Lifecycle phases for agent execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    /// Run is starting
    Start,
    /// Run is in progress (model is thinking)
    Running,
    /// Run completed successfully
    End,
    /// Run failed with an error
    Error,
    /// Run was aborted
    Aborted,
}

impl fmt::Display for LifecyclePhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LifecyclePhase::Start => write!(f, "start"),
            LifecyclePhase::Running => write!(f, "running"),
            LifecyclePhase::End => write!(f, "end"),
            LifecyclePhase::Error => write!(f, "error"),
            LifecyclePhase::Aborted => write!(f, "aborted"),
        }
    }
}

/// Events emitted during agent execution
#[derive(Debug, Clone)]
pub enum AgenticEvent {
    /// Lifecycle event (start, running, end, error, aborted)
    Lifecycle {
        /// Run identifier
        run_id: RunId,
        /// Lifecycle phase
        phase: LifecyclePhase,
        /// Optional error message (for Error phase)
        error: Option<String>,
    },

    /// Assistant text content with clear semantics
    ///
    /// Represents a block of text from the assistant. Unlike the deprecated
    /// `Assistant` variant, this uses `is_interstitial` to clearly indicate
    /// whether this text appears before tool calls (interstitial) or is the
    /// final answer to the user.
    ///
    /// # Semantics
    /// - `is_interstitial: true` - Text explaining tool calls (e.g., "Let me search...")
    /// - `is_interstitial: false` - Final answer to the user's query
    ///
    /// # Example Flow
    /// ```text
    /// User: "What's the weather?"
    /// → AssistantText { text: "I'll check the weather...", is_interstitial: true }
    /// → ToolStart { name: "weather" }
    /// → ToolEnd { ... }
    /// → AssistantText { text: "It's sunny today.", is_interstitial: false }
    /// ```
    AssistantText {
        /// Run identifier
        run_id: RunId,
        /// Text content (complete block)
        text: String,
        /// Block sequence number for ordering within a run
        sequence: usize,
        /// True if this text precedes tool calls (explanatory text)
        /// False if this is the final answer
        is_interstitial: bool,
    },

    /// Thinking/reasoning block
    ///
    /// Models like Claude 3.7 Sonnet, o1, etc. emit thinking blocks
    /// that show the model's reasoning process.
    Thinking {
        /// Run identifier
        run_id: RunId,
        /// Thinking content (delta or complete)
        text: String,
        /// True if this is a delta (incremental)
        is_delta: bool,
        /// True if this is the final chunk
        is_final: bool,
        /// Optional signature for verifying thinking (Claude)
        signature: Option<String>,
    },

    /// Tool call streaming
    ///
    /// For providers that stream tool call construction (e.g., Claude, `OpenAI`)
    ToolCallDelta {
        /// Run identifier
        run_id: RunId,
        /// Tool call identifier
        tool_call_id: ToolCallId,
        /// Tool name (may be partial during streaming)
        name: Option<String>,
        /// Arguments delta (partial JSON)
        arguments_delta: Option<String>,
        /// True if this is the final delta
        is_final: bool,
    },

    /// Tool execution started
    ToolStart {
        /// Run identifier
        run_id: RunId,
        /// Tool execution identifier
        tool_id: ToolId,
        /// Tool name
        name: String,
        /// Tool parameters
        params: Value,
    },

    /// Tool execution progress update
    ///
    /// For long-running tools, periodic updates can be emitted.
    ToolUpdate {
        /// Run identifier
        run_id: RunId,
        /// Tool execution identifier
        tool_id: ToolId,
        /// Progress message or partial output
        output: String,
        /// Optional progress percentage (0-100)
        progress_percent: Option<u8>,
    },

    /// Tool execution completed
    ToolEnd {
        /// Run identifier
        run_id: RunId,
        /// Tool execution identifier
        tool_id: ToolId,
        /// Tool result
        result: Value,
        /// True if execution succeeded
        success: bool,
        /// Execution duration in milliseconds
        duration_ms: u64,
    },

    /// Status message for user visibility
    ///
    /// Used to show "Thinking...", "Running tool X...", etc.
    Status {
        /// Run identifier
        run_id: RunId,
        /// Status message
        message: String,
        /// Whether to show typing indicator
        typing: bool,
    },

    /// Token usage statistics
    ///
    /// Emitted at the end of a run (optional)
    Usage {
        /// Run identifier
        run_id: RunId,
        /// Prompt tokens consumed
        prompt_tokens: u32,
        /// Completion tokens generated
        completion_tokens: u32,
        /// Total tokens
        total_tokens: u32,
    },

    /// Text streaming delta (for true streaming mode)
    ///
    /// Unlike `AssistantText` (complete block), this is a raw delta
    /// that channels can render immediately or buffer.
    /// This is the primary event for token-by-token streaming.
    AssistantDelta {
        /// Run identifier
        run_id: RunId,
        /// Text delta (incremental content)
        text: String,
        /// Block sequence number for ordering within a run
        sequence: usize,
        /// True if this text precedes tool calls (interstitial)
        is_interstitial: bool,
    },

    /// Tool call streaming preview (for UI feedback)
    ///
    /// Emitted while tool call JSON is being accumulated.
    /// Channels can show "Running {name}..." with progress.
    ToolCallStreaming {
        /// Run identifier
        run_id: RunId,
        /// Tool call identifier
        tool_call_id: ToolCallId,
        /// Tool name (may be partial during streaming)
        name: Option<String>,
        /// Arguments preview (partial JSON)
        arguments_preview: String,
        /// Progress percentage (0-100, if determinable)
        progress: Option<u8>,
    },

    /// Flush request (internal signal)
    ///
    /// Signals that buffered content should be emitted immediately.
    /// Used when tool calls start (to end interstitial text)
    /// or when the stream ends.
    Flush {
        /// Run identifier
        run_id: RunId,
    },
}

impl AgenticEvent {
    /// Get the run ID for this event
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            AgenticEvent::Lifecycle { run_id, .. } => run_id,
            AgenticEvent::AssistantText { run_id, .. } => run_id,
            AgenticEvent::Thinking { run_id, .. } => run_id,
            AgenticEvent::ToolCallDelta { run_id, .. } => run_id,
            AgenticEvent::ToolStart { run_id, .. } => run_id,
            AgenticEvent::ToolUpdate { run_id, .. } => run_id,
            AgenticEvent::ToolEnd { run_id, .. } => run_id,
            AgenticEvent::Status { run_id, .. } => run_id,
            AgenticEvent::Usage { run_id, .. } => run_id,
            AgenticEvent::AssistantDelta { run_id, .. } => run_id,
            AgenticEvent::ToolCallStreaming { run_id, .. } => run_id,
            AgenticEvent::Flush { run_id, .. } => run_id,
        }
    }

    /// Returns true if this is a lifecycle end event
    #[must_use]
    pub fn is_end(&self) -> bool {
        matches!(
            self,
            AgenticEvent::Lifecycle {
                phase: LifecyclePhase::End,
                ..
            }
        )
    }

    /// Returns true if this is an error event
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            AgenticEvent::Lifecycle {
                phase: LifecyclePhase::Error,
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lifecycle_phase_display() {
        assert_eq!(LifecyclePhase::Start.to_string(), "start");
        assert_eq!(LifecyclePhase::Running.to_string(), "running");
        assert_eq!(LifecyclePhase::End.to_string(), "end");
    }

    #[test]
    fn test_event_is_end() {
        let end_event = AgenticEvent::Lifecycle {
            run_id: "test-1".to_string(),
            phase: LifecyclePhase::End,
            error: None,
        };
        assert!(end_event.is_end());
        assert!(!end_event.is_error());
    }

    #[test]
    fn test_assistant_delta_event() {
        let event = AgenticEvent::AssistantDelta {
            run_id: "run_123".to_string(),
            text: "Hello".to_string(),
            sequence: 1,
            is_interstitial: false,
        };

        assert_eq!(event.run_id(), "run_123");
        match event {
            AgenticEvent::AssistantDelta {
                text,
                sequence,
                is_interstitial,
                ..
            } => {
                assert_eq!(text, "Hello");
                assert_eq!(sequence, 1);
                assert!(!is_interstitial);
            }
            _ => panic!("Expected AssistantDelta event"),
        }
    }

    #[test]
    fn test_tool_call_streaming_event() {
        let event = AgenticEvent::ToolCallStreaming {
            run_id: "run_123".to_string(),
            tool_call_id: "tc_001".to_string(),
            name: Some("web_search".to_string()),
            arguments_preview: "{\"query\": \"rust\"".to_string(),
            progress: Some(50),
        };

        assert_eq!(event.run_id(), "run_123");
        match event {
            AgenticEvent::ToolCallStreaming {
                tool_call_id,
                name,
                arguments_preview,
                progress,
                ..
            } => {
                assert_eq!(tool_call_id, "tc_001");
                assert_eq!(name, Some("web_search".to_string()));
                assert!(arguments_preview.contains("query"));
                assert_eq!(progress, Some(50));
            }
            _ => panic!("Expected ToolCallStreaming event"),
        }
    }

    #[test]
    fn test_flush_event() {
        let event = AgenticEvent::Flush {
            run_id: "run_123".to_string(),
        };

        assert_eq!(event.run_id(), "run_123");
        assert!(matches!(event, AgenticEvent::Flush { .. }));
    }
}
