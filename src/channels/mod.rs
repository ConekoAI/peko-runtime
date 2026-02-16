//! Communication channels for agent I/O
//!
//! This module provides different channels for agents to communicate:
//! - CLI: Interactive terminal interface
//! - HTTP: Webhook-based HTTP server

pub mod cli;
pub mod http;

use anyhow::Result;
use async_trait::async_trait;

/// Channel trait for bidirectional communication
///
/// Implement this trait to create new communication channels.
/// Channels are used by agents to receive input and send output.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Get the channel name
    fn name(&self) -> &str;
    
    /// Send a message through the channel
    async fn send(&self, message: &str) -> Result<()>;
    
    /// Receive a message from the channel
    /// Returns None if no message is available (non-blocking)
    async fn receive(&mut self) -> Result<Option<String>>;
}

// Re-exports for convenience
pub use cli::CliChannel;
pub use http::HttpChannel;

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
        
        async fn send(&self, _message: &str) -> Result<()> {
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
