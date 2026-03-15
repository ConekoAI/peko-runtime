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

    /// Assistant text delta or block
    ///
    /// In streaming mode, text arrives as deltas.
    /// In block mode, text arrives as complete blocks.
    Assistant {
        /// Run identifier
        run_id: RunId,
        /// Text content (delta or complete)
        text: String,
        /// True if this is a delta (incremental)
        is_delta: bool,
        /// True if this is the final chunk
        is_final: bool,
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
}

impl AgenticEvent {
    /// Get the run ID for this event
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            AgenticEvent::Lifecycle { run_id, .. } => run_id,
            AgenticEvent::Assistant { run_id, .. } => run_id,
            AgenticEvent::Thinking { run_id, .. } => run_id,
            AgenticEvent::ToolCallDelta { run_id, .. } => run_id,
            AgenticEvent::ToolStart { run_id, .. } => run_id,
            AgenticEvent::ToolUpdate { run_id, .. } => run_id,
            AgenticEvent::ToolEnd { run_id, .. } => run_id,
            AgenticEvent::Status { run_id, .. } => run_id,
            AgenticEvent::Usage { run_id, .. } => run_id,
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

/// Event source trait for components that emit events
#[async_trait::async_trait]
pub trait EventSource: Send + Sync {
    /// Subscribe to events from this source
    ///
    /// Returns a channel receiver for events.
    fn subscribe(&self) -> tokio::sync::mpsc::Receiver<AgenticEvent>;

    /// Emit an event to all subscribers
    async fn emit(&self, event: AgenticEvent);
}

/// Event router for distributing events to multiple consumers
pub struct EventRouter {
    subscribers: Vec<tokio::sync::mpsc::Sender<AgenticEvent>>,
}

impl EventRouter {
    /// Create a new event router
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscribers: Vec::new(),
        }
    }

    /// Add a subscriber
    pub fn subscribe(&mut self) -> tokio::sync::mpsc::Receiver<AgenticEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        self.subscribers.push(tx);
        rx
    }

    /// Emit an event to all subscribers
    pub async fn emit(&self, event: AgenticEvent) {
        for subscriber in &self.subscribers {
            // Don't fail if a subscriber is closed
            let _ = subscriber.send(event.clone()).await;
        }
    }

    /// Emit an event, logging errors
    pub async fn emit_with_log(&self, event: AgenticEvent) {
        for subscriber in &self.subscribers {
            if let Err(e) = subscriber.send(event.clone()).await {
                tracing::warn!("Failed to send event to subscriber: {}", e);
            }
        }
    }
}

impl Default for EventRouter {
    fn default() -> Self {
        Self::new()
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

    #[tokio::test]
    async fn test_event_router() {
        let mut router = EventRouter::new();
        let mut rx = router.subscribe();

        let event = AgenticEvent::Status {
            run_id: "test-1".to_string(),
            message: "Thinking...".to_string(),
            typing: true,
        };

        router.emit(event.clone()).await;

        let received = rx.recv().await.unwrap();
        match received {
            AgenticEvent::Status { message, .. } => {
                assert_eq!(message, "Thinking...");
            }
            _ => panic!("Expected Status event"),
        }
    }
}
