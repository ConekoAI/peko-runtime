//! Stream types for agent execution output
//!
//! These types bridge the engine's event production with presentation-layer
//! consumers (CLI, HTTP, gateway extensions). They were migrated from
//! `src/channels/` as part of ADR-017 — channels are now external gateway
//! extensions, but the core stream types remain part of the engine.

use anyhow::Result;
use tokio::sync::mpsc::Receiver;
use tokio::sync::oneshot;

/// Event stream returned by `StatelessAgentService`
///
/// This is the unified interface between the engine and presentation layers.
/// Consumers (channels, gateways, CLI) read from this stream to produce
/// appropriate output.
///
/// The `completion` field provides a signal that ensures all session
/// persistence operations complete before the stream is considered done.
#[derive(Debug)]
pub struct EventStream {
    /// Receiver for agentic events
    pub receiver: Receiver<crate::engine::AgenticEvent>,
    /// Completion signal - resolves when all session writes are complete
    ///
    /// This eliminates the race condition where the consumer receives the End
    /// event before session persistence finishes.
    pub completion: oneshot::Receiver<anyhow::Result<()>>,
    /// Session ID for this execution
    pub session_id: String,
    /// Whether this is a new session
    pub is_new_session: bool,
}

/// Output from stream processing
///
/// Contains the final result after processing all events.
/// Used by blocking consumers to return collected output.
#[derive(Debug, Clone)]
pub struct ChannelOutput {
    /// Final text response
    pub final_text: String,
    /// Tool calls made during execution
    pub tool_calls: Vec<crate::agent::stateless_service::ToolCallInfo>,
    /// Token usage statistics
    pub usage: crate::providers::TokenUsage,
    /// Session ID
    pub session_id: String,
    /// Whether this was a new session
    pub is_new_session: bool,
    /// Whether execution succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
}

impl ChannelOutput {
    /// Create a new empty output
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            final_text: String::new(),
            tool_calls: Vec::new(),
            usage: crate::providers::TokenUsage::default(),
            session_id: session_id.into(),
            is_new_session: false,
            success: true,
            error: None,
        }
    }
}

/// Streaming configuration for presentation layers
///
/// Controls how streaming output is chunked and presented.
/// This lives at the presentation layer, not the agent layer.
#[derive(Debug, Clone)]
pub struct StreamingConfig {
    /// Enable streaming mode
    pub enabled: bool,
    /// Minimum characters before emitting a block
    pub min_chars: usize,
    /// Maximum characters per block
    pub max_chars: usize,
    /// Break preference: paragraph, sentence, whitespace, hard
    pub break_preference: crate::engine::chunker::BreakPreference,
    /// Show tool execution in real-time
    pub show_tools: bool,
    /// Show thinking/typing indicators
    pub show_status: bool,
    /// Coalesce small blocks (wait for idle before sending)
    pub coalesce: bool,
    /// Idle milliseconds before flushing coalesced blocks
    pub coalesce_idle_ms: u64,
    /// Human-like delay between blocks (min, max) in ms
    pub human_delay: Option<(u64, u64)>,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_chars: 100,
            max_chars: 2000,
            break_preference: crate::engine::chunker::BreakPreference::Sentence,
            show_tools: true,
            show_status: true,
            coalesce: false,
            coalesce_idle_ms: 500,
            human_delay: None,
        }
    }
}

/// Default event stream processing (shared implementation)
///
/// This is a helper function that consumers can use to get the default
/// behavior without duplicating the implementation.
///
/// This implementation awaits the completion signal to ensure session
/// persistence completes before returning, eliminating race conditions.
pub async fn default_process_stream(event_stream: EventStream) -> Result<ChannelOutput> {
    use crate::engine::{AgenticEvent, LifecyclePhase};

    let mut output = ChannelOutput::new(&event_stream.session_id);
    output.is_new_session = event_stream.is_new_session;

    let mut event_rx = event_stream.receiver;
    let completion = event_stream.completion;
    let mut end_received = false;

    while let Some(event) = event_rx.recv().await {
        match event {
            AgenticEvent::AssistantText {
                text,
                is_interstitial: false,
                ..
            } => {
                output.final_text.push_str(&text);
            }
            AgenticEvent::AssistantDelta { text, .. } => {
                output.final_text.push_str(&text);
            }
            AgenticEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => {
                output.usage.input = u64::from(prompt_tokens);
                output.usage.output = u64::from(completion_tokens);
                output.usage.total = u64::from(total_tokens);
            }
            AgenticEvent::Lifecycle { phase, error, .. } => {
                match phase {
                    LifecyclePhase::End => {
                        end_received = true;
                        // Don't break yet - wait for receiver to close
                    }
                    LifecyclePhase::Error => {
                        output.success = false;
                        output.error = error;
                        end_received = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Receiver closed - NOW wait for completion signal
    // This ensures session persistence is complete
    if end_received {
        match tokio::time::timeout(std::time::Duration::from_secs(30), completion).await {
            Ok(Ok(Ok(()))) => {
                // Session persistence complete
            }
            Ok(Ok(Err(e))) => {
                // Only log if the stream itself didn't already report an error.
                // Stream errors are communicated via LifecyclePhase::Error events.
                if output.success {
                    tracing::warn!("Session persistence failed: {}", e);
                }
            }
            Ok(Err(_recv_error)) => {
                // Sender dropped without sending - this is ok if execution completed
                tracing::warn!("Completion sender dropped without signal");
            }
            Err(_) => {
                tracing::warn!("Completion timeout - session persistence may be incomplete");
            }
        }
    }

    Ok(output)
}
