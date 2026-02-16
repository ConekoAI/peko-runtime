//! Discord channel implementation
//! 
//! Uses Discord Bot API for sending messages and Gateway WebSocket for receiving.

use super::Channel;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::{debug, error, info, warn};

/// Discord channel configuration
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    /// Discord bot token
    pub bot_token: String,
    /// Optional guild ID to filter messages
    pub guild_id: Option<String>,
    /// Optional channel ID to filter messages
    pub channel_id: Option<String>,
    /// List of allowed user IDs (empty = allow all)
    pub allowed_users: Vec<String>,
}

impl DiscordConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let bot_token = std::env::var("DISCORD_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("DISCORD_BOT_TOKEN not set"))?;
        
        Ok(Self {
            bot_token,
            guild_id: std::env::var("DISCORD_GUILD_ID").ok(),
            channel_id: std::env::var("DISCORD_CHANNEL_ID").ok(),
            allowed_users: Vec::new(),
        })
    }
}

/// Discord channel for bot communication
pub struct DiscordChannel {
    config: DiscordConfig,
    client: reqwest::Client,
    message_queue: VecDeque<String>,
    last_channel_id: Option<String>,
}

impl DiscordChannel {
    /// Create new Discord channel
    pub fn new(config: DiscordConfig) -> Self {
        info!("Discord channel initialized");
        Self {
            config,
            client: reqwest::Client::new(),
            message_queue: VecDeque::new(),
            last_channel_id: None,
        }
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(DiscordConfig::from_env()?))
    }

    /// Check if user is allowed
    fn is_user_allowed(&self, user_id: &str) -> bool {
        if self.config.allowed_users.is_empty() {
            return true; // Allow all if no restrictions
        }
        self.config.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Send message to a specific channel
    async fn send_to_channel(&self, channel_id: &str, message: &str) -> Result<()> {
        let url = format!("https://discord.com/api/v10/channels/{}/messages", channel_id);
        let body = serde_json::json!({ "content": message });

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error = response.text().await.unwrap_or_default();
            error!("Discord API error: {} - {}", status, error);
            return Err(anyhow::anyhow!("Discord API error: {} - {}", status, error));
        }

        debug!("Sent message to Discord channel {}", channel_id);
        Ok(())
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn send(&mut self, message: &str) -> Result<()> {
        // Try to send to the last known channel, or the configured channel
        if let Some(channel_id) = self.last_channel_id.as_ref().or(self.config.channel_id.as_ref()) {
            self.send_to_channel(channel_id, message).await?;
        } else {
            warn!("No Discord channel ID available for sending");
            return Err(anyhow::anyhow!("No Discord channel ID configured"));
        }
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        // Check if we have queued messages
        if let Some(msg) = self.message_queue.pop_front() {
            return Ok(Some(msg));
        }

        // In a full implementation, this would:
        // 1. Connect to Discord Gateway WebSocket
        // 2. Listen for messages
        // 3. Filter by guild/channel/user
        // 4. Queue messages for processing
        
        // For now, return None (non-blocking)
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discord_channel_creation() {
        let config = DiscordConfig {
            bot_token: "test_token".to_string(),
            guild_id: None,
            channel_id: Some("123456".to_string()),
            allowed_users: vec![],
        };
        let channel = DiscordChannel::new(config);
        assert_eq!(channel.name(), "discord");
    }

    #[test]
    fn test_user_allowed() {
        let config = DiscordConfig {
            bot_token: "test".to_string(),
            guild_id: None,
            channel_id: None,
            allowed_users: vec!["user1".to_string(), "user2".to_string()],
        };
        let channel = DiscordChannel::new(config);
        assert!(channel.is_user_allowed("user1"));
        assert!(!channel.is_user_allowed("user3"));
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let config = DiscordConfig {
            bot_token: "test".to_string(),
            guild_id: None,
            channel_id: None,
            allowed_users: vec!["*".to_string()],
        };
        let channel = DiscordChannel::new(config);
        assert!(channel.is_user_allowed("any_user"));
    }
}
