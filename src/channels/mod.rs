//! Communication channels for agent I/O
//!
//! This module provides different channels for agents to communicate:
//! - CLI: Interactive terminal interface
//! - Discord: Discord Bot API integration
//!
//! Additional channels (HTTP, Telegram, Slack, Matrix, WhatsApp) will be
//! implemented as GatewayPlugin extensions.

pub mod cli;
pub mod discord;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Receiver;
use tokio::sync::oneshot;

/// Event stream returned by StatelessAgentService
/// 
/// This is the unified interface between service and presentation layers.
/// Channels consume this stream to produce appropriate output.
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

/// Output from channel processing
/// 
/// Contains the final result after processing all events.
/// Used by blocking channels to return collected output.
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

/// Streaming configuration for channels
///
/// Controls how streaming output is chunked and presented.
/// This lives at the channel layer (presentation), not the agent layer.
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

/// Channel trait for bidirectional communication
///
/// Implement this trait to create new communication channels.
/// Channels are used by agents to communicate.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Get the channel name
    fn name(&self) -> &str;

    /// Send a message through the channel
    async fn send(&mut self, message: &str) -> Result<()>;

    /// Receive a message from the channel
    /// Returns None if no message is available (non-blocking)
    async fn receive(&mut self) -> Result<Option<String>>;

    /// Get the streaming configuration for this channel
    ///
    /// Channels can override agent defaults based on their capabilities.
    fn streaming_config(&self) -> StreamingConfig {
        StreamingConfig::default()
    }

    /// Handle a streaming event receiver (legacy method)
    ///
    /// **DEPRECATED:** Use `process_stream` instead.
    /// This method will be removed in a future version.
    #[deprecated(since = "0.2.0", note = "Use process_stream instead")]
    async fn handle_stream(
        &mut self,
        mut event_rx: Receiver<crate::engine::AgenticEvent>,
    ) -> Result<()> {
        use crate::engine::AgenticEvent;

        while let Some(event) = event_rx.recv().await {
            match event {
                // New event type with clear semantics
                AgenticEvent::AssistantText {
                    text,
                    is_interstitial,
                    ..
                } => {
                    // Only send final answers in default impl
                    if !is_interstitial {
                        self.send(&text).await?;
                    }
                }
                // Deprecated: legacy event type (backward compatibility)
                #[allow(deprecated)]
                AgenticEvent::Assistant { text, is_final, .. } => {
                    if is_final {
                        self.send(&text).await?;
                    }
                    // Deltas are ignored in default impl
                }
                AgenticEvent::Lifecycle { phase, error, .. } => match phase {
                    crate::engine::LifecyclePhase::End => break,
                    crate::engine::LifecyclePhase::Error => {
                        if let Some(err) = error {
                            eprintln!("Stream error: {err}");
                        }
                        break;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    }

    /// Process an event stream and return collected output
    ///
    /// This is the unified interface for consuming agent execution events.
    /// Channels implement this to handle presentation:
    /// - CLI channels can stream to stdout or collect for blocking mode
    /// - HTTP channels convert to SSE
    /// - WebSocket channels convert to WS messages
    ///
    /// Default implementation collects events into ChannelOutput.
    async fn process_stream(
        &self,
        event_stream: EventStream,
    ) -> Result<ChannelOutput> {
        default_process_stream(event_stream).await
    }
}

/// Default event stream processing (shared implementation)
///
/// This is a helper function that channels can use to get the default
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
                output.usage.input = prompt_tokens as u64;
                output.usage.output = completion_tokens as u64;
                output.usage.total = total_tokens as u64;
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
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            completion
        ).await {
            Ok(Ok(Ok(()))) => {
                // Session persistence complete
            }
            Ok(Ok(Err(e))) => {
                // Log the error but don't fail - the execution itself succeeded
                tracing::warn!("Session persistence failed: {}", e);
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

// Re-exports for convenience
pub use cli::{CliChannel, CliMode};
pub use discord::{DiscordChannel, DiscordConfig};

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock channel for testing
    pub struct MockChannel {
        name: String,
        messages: Vec<String>,
    }

    impl MockChannel {
        pub fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                messages: Vec::new(),
            }
        }

        pub fn add_message(&mut self, msg: impl Into<String>) {
            self.messages.push(msg.into());
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            &self.name
        }

        async fn send(&mut self, _message: &str) -> Result<()> {
            Ok(())
        }

        async fn receive(&mut self) -> Result<Option<String>> {
            Ok(self.messages.pop())
        }
    }

    #[tokio::test]
    async fn test_mock_channel() {
        let mut channel = MockChannel::new("test");
        assert_eq!(channel.name(), "test");

        channel.add_message("hello");
        let msg = channel.receive().await.unwrap();
        assert_eq!(msg, Some("hello".to_string()));
    }
}
