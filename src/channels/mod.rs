//! Communication channels for agent I/O
//!
//! This module provides different channels for agents to communicate:
//! - CLI: Interactive terminal interface
//! - HTTP: Webhook-based HTTP server
//! - Telegram: Telegram Bot API integration
//! - Discord: Discord Bot API integration
//! - Slack: Slack Web API integration
//! - Matrix: Matrix Client-Server API integration
//! - `WhatsApp`: `WhatsApp` Business Cloud API integration

pub mod cli;
pub mod discord;
pub mod http;
pub mod matrix;
pub mod slack;
pub mod telegram;
pub mod whatsapp;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Receiver;

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

    /// Handle a streaming event receiver
    ///
    /// This is where chunking and presentation happens.
    /// The channel receives raw AgenticEvents and handles:
    /// - Block chunking based on channel config
    /// - Coalescing small blocks
    /// - Human-like delays between blocks
    /// - Platform-specific formatting
    ///
    /// Default implementation just forwards events without chunking.
    /// Override for custom streaming behavior.
    async fn handle_stream(
        &mut self,
        mut event_rx: Receiver<crate::engine::AgenticEvent>,
    ) -> Result<()> {
        use crate::engine::AgenticEvent;

        while let Some(event) = event_rx.recv().await {
            match event {
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
                            eprintln!("Stream error: {}", err);
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
}

// Re-exports for convenience
pub use cli::CliChannel;
pub use discord::{DiscordChannel, DiscordConfig};
pub use http::HttpChannel;
pub use matrix::{MatrixChannel, MatrixConfig};
pub use slack::{SlackChannel, SlackConfig};
pub use telegram::{TelegramChannel, TelegramConfig};
pub use whatsapp::{WhatsAppChannel, WhatsAppConfig};

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
