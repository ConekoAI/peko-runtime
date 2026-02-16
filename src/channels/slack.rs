//! Slack channel implementation
//!
//! Uses Slack Web API for sending messages and polling for receiving.

use super::Channel;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::VecDeque;
use tracing::{debug, error, info, warn};

/// Slack channel configuration
#[derive(Debug, Clone)]
pub struct SlackConfig {
    /// Slack bot token (starts with xoxb-)
    pub bot_token: String,
    /// Optional channel ID to send/receive from
    pub channel_id: Option<String>,
    /// List of allowed user IDs (empty = allow all)
    pub allowed_users: Vec<String>,
}

impl SlackConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("SLACK_BOT_TOKEN not set"))?;
        
        Ok(Self {
            bot_token,
            channel_id: std::env::var("SLACK_CHANNEL_ID").ok(),
            allowed_users: Vec::new(),
        })
    }
}

/// Slack channel for bot communication
pub struct SlackChannel {
    config: SlackConfig,
    client: reqwest::Client,
    message_queue: VecDeque<String>,
    bot_user_id: Option<String>,
}

impl SlackChannel {
    /// Create new Slack channel
    pub fn new(config: SlackConfig) -> Self {
        info!("Slack channel initialized");
        Self {
            config,
            client: reqwest::Client::new(),
            message_queue: VecDeque::new(),
            bot_user_id: None,
        }
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(SlackConfig::from_env()?))
    }

    /// Check if user is allowed
    fn is_user_allowed(&self, user_id: &str) -> bool {
        if self.config.allowed_users.is_empty() {
            return true; // Allow all if no restrictions
        }
        self.config.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Get bot's own user ID
    async fn fetch_bot_user_id(&mut self) -> Result<Option<String>> {
        let resp: serde_json::Value = self
            .client
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.config.bot_token)
            .send()
            .await?
            .json()
            .await?;

        if let Some(error) = resp.get("error") {
            error!("Slack auth error: {:?}", error);
            return Ok(None);
        }

        let user_id = resp
            .get("user_id")
            .and_then(|u| u.as_str())
            .map(String::from);

        self.bot_user_id = user_id.clone();
        Ok(user_id)
    }

    /// Send message to a specific channel
    async fn send_to_channel(&self, channel_id: &str, message: &str) -> Result<()> {
        let body = serde_json::json!({
            "channel": channel_id,
            "text": message
        });

        let response = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.config.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error = response.text().await.unwrap_or_default();
            error!("Slack API error: {} - {}", status, error);
            return Err(anyhow::anyhow!("Slack API error: {} - {}", status, error));
        }

        // Check Slack's own error response
        let result: serde_json::Value = response.json().await?;
        if let Some(ok) = result.get("ok").and_then(|v| v.as_bool()) {
            if !ok {
                let error = result.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
                return Err(anyhow::anyhow!("Slack API error: {}", error));
            }
        }

        debug!("Sent message to Slack channel {}", channel_id);
        Ok(())
    }

    /// Poll for new messages
    async fn poll_messages(&mut self) -> Result<Vec<String>> {
        let channel_id = match &self.config.channel_id {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };

        let response = self
            .client
            .get("https://slack.com/api/conversations.history")
            .bearer_auth(&self.config.bot_token)
            .query(&[("channel", channel_id.as_str()), ("limit", "10")])
            .send()
            .await?;

        let result: serde_json::Value = response.json().await?;
        
        if let Some(ok) = result.get("ok").and_then(|v| v.as_bool()) {
            if !ok {
                let error = result.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
                return Err(anyhow::anyhow!("Slack API error: {}", error));
            }
        }

        let mut messages = Vec::new();
        let bot_id = self.bot_user_id.clone().unwrap_or_default();

        if let Some(msgs) = result.get("messages").and_then(|m| m.as_array()) {
            for msg in msgs {
                // Skip bot's own messages
                if let Some(user) = msg.get("user").and_then(|u| u.as_str()) {
                    if user == bot_id {
                        continue;
                    }
                    
                    // Check if user is allowed
                    if !self.is_user_allowed(user) {
                        continue;
                    }
                }

                if let Some(text) = msg.get("text").and_then(|t| t.as_str()) {
                    messages.push(text.to_string());
                }
            }
        }

        Ok(messages)
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&mut self, message: &str) -> Result<()> {
        if let Some(channel_id) = &self.config.channel_id {
            self.send_to_channel(channel_id, message).await?;
        } else {
            warn!("No Slack channel ID configured for sending");
            return Err(anyhow::anyhow!("No Slack channel ID configured"));
        }
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        // Check if we have queued messages
        if let Some(msg) = self.message_queue.pop_front() {
            return Ok(Some(msg));
        }

        // Fetch bot user ID if not already known
        if self.bot_user_id.is_none() {
            let _ = self.fetch_bot_user_id().await;
        }

        // Poll for new messages
        match self.poll_messages().await {
            Ok(messages) => {
                for msg in messages {
                    self.message_queue.push_back(msg);
                }
            }
            Err(e) => {
                error!("Failed to poll Slack messages: {}", e);
            }
        }

        // Return first queued message if any
        Ok(self.message_queue.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slack_channel_creation() {
        let config = SlackConfig {
            bot_token: "xoxb-test-token".to_string(),
            channel_id: Some("C123456".to_string()),
            allowed_users: vec![],
        };
        let channel = SlackChannel::new(config);
        assert_eq!(channel.name(), "slack");
    }

    #[test]
    fn test_user_allowed() {
        let config = SlackConfig {
            bot_token: "xoxb-test".to_string(),
            channel_id: None,
            allowed_users: vec!["U123".to_string(), "U456".to_string()],
        };
        let channel = SlackChannel::new(config);
        assert!(channel.is_user_allowed("U123"));
        assert!(!channel.is_user_allowed("U789"));
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let config = SlackConfig {
            bot_token: "xoxb-test".to_string(),
            channel_id: None,
            allowed_users: vec!["*".to_string()],
        };
        let channel = SlackChannel::new(config);
        assert!(channel.is_user_allowed("any_user"));
    }
}
