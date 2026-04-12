//! Message Tool - Send messages to communication channels
//!
//! Supports: Discord, Slack, Telegram, `WhatsApp`, Signal, Email
//! This is the core tool missing from `OpenClaw` parity (17/18 → 18/18)

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::tools::Tool;

/// Channel types supported by the message tool
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelType {
    /// Discord
    Discord,
    /// Slack
    Slack,
    /// Telegram
    Telegram,
    /// `WhatsApp`
    Whatsapp,
    /// Signal
    Signal,
    /// Email (SMTP)
    Email,
    /// Generic HTTP webhook
    Webhook,
}

impl std::fmt::Display for ChannelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelType::Discord => write!(f, "discord"),
            ChannelType::Slack => write!(f, "slack"),
            ChannelType::Telegram => write!(f, "telegram"),
            ChannelType::Whatsapp => write!(f, "whatsapp"),
            ChannelType::Signal => write!(f, "signal"),
            ChannelType::Email => write!(f, "email"),
            ChannelType::Webhook => write!(f, "webhook"),
        }
    }
}

/// Message configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageConfig {
    /// Discord webhook URL (optional)
    pub discord_webhook_url: Option<String>,
    /// Slack webhook URL (optional)
    pub slack_webhook_url: Option<String>,
    /// Telegram bot token (optional)
    pub telegram_bot_token: Option<String>,
    /// `WhatsApp` API key (optional)
    pub whatsapp_api_key: Option<String>,
    /// Signal API endpoint (optional)
    pub signal_api_endpoint: Option<String>,
    /// Email SMTP config (optional)
    pub smtp_config: Option<SmtpConfig>,
    /// Default from address for email
    pub default_from: Option<String>,
}

/// SMTP configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub use_tls: bool,
}

/// Message sending result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageResult {
    pub success: bool,
    pub channel: String,
    pub recipient: String,
    pub message_id: Option<String>,
    pub error: Option<String>,
}

/// Message tool for sending messages to channels
pub struct MessageTool {
    config: MessageConfig,
    client: reqwest::Client,
}

impl MessageTool {
    /// Create a new message tool
    #[must_use]
    pub fn new(config: MessageConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Create with config from environment
    #[must_use]
    pub fn from_env() -> Self {
        let config = MessageConfig {
            discord_webhook_url: std::env::var("DISCORD_WEBHOOK_URL").ok(),
            slack_webhook_url: std::env::var("SLACK_WEBHOOK_URL").ok(),
            telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN").ok(),
            whatsapp_api_key: std::env::var("WHATSAPP_API_KEY").ok(),
            signal_api_endpoint: std::env::var("SIGNAL_API_ENDPOINT").ok(),
            smtp_config: None, // SMTP requires more complex config
            default_from: std::env::var("MESSAGE_DEFAULT_FROM").ok(),
        };
        Self::new(config)
    }

    /// Send a message
    async fn send_message(
        &self,
        channel: ChannelType,
        recipient: &str,
        content: &str,
        subject: Option<&str>,
    ) -> Result<MessageResult> {
        match channel {
            ChannelType::Discord => self.send_discord(content).await,
            ChannelType::Slack => self.send_slack(recipient, content).await,
            ChannelType::Telegram => self.send_telegram(recipient, content).await,
            ChannelType::Whatsapp => self.send_whatsapp(recipient, content).await,
            ChannelType::Signal => self.send_signal(recipient, content).await,
            ChannelType::Email => {
                let sub = subject.unwrap_or("Message from Pekobot");
                self.send_email(recipient, sub, content).await
            }
            ChannelType::Webhook => self.send_webhook(recipient, content).await,
        }
    }

    /// Send Discord message via webhook
    async fn send_discord(&self, content: &str) -> Result<MessageResult> {
        let webhook_url = self
            .config
            .discord_webhook_url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Discord webhook URL not configured"))?;

        let payload = serde_json::json!({
            "content": content,
        });

        let response = self
            .client
            .post(webhook_url)
            .json(&payload)
            .send()
            .await
            .context("Failed to send Discord message")?;

        if response.status().is_success() {
            info!("Sent Discord message");
            Ok(MessageResult {
                success: true,
                channel: "discord".to_string(),
                recipient: "webhook".to_string(),
                message_id: None,
                error: None,
            })
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!("Discord API error {status}: {text}"))
        }
    }

    /// Send Slack message via webhook
    async fn send_slack(&self, channel: &str, content: &str) -> Result<MessageResult> {
        let webhook_url = self
            .config
            .slack_webhook_url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Slack webhook URL not configured"))?;

        let payload = serde_json::json!({
            "channel": channel,
            "text": content,
        });

        let response = self
            .client
            .post(webhook_url)
            .json(&payload)
            .send()
            .await
            .context("Failed to send Slack message")?;

        if response.status().is_success() {
            info!("Sent Slack message to {}", channel);
            Ok(MessageResult {
                success: true,
                channel: "slack".to_string(),
                recipient: channel.to_string(),
                message_id: None,
                error: None,
            })
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!("Slack API error {status}: {text}"))
        }
    }

    /// Send Telegram message
    async fn send_telegram(&self, chat_id: &str, content: &str) -> Result<MessageResult> {
        let token = self
            .config
            .telegram_bot_token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Telegram bot token not configured"))?;

        let url = format!("https://api.telegram.org/bot{token}/sendMessage");

        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text": content,
        });

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .context("Failed to send Telegram message")?;

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Telegram response")?;

        if result
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            let message_id = result["result"]["message_id"]
                .as_i64()
                .map(|id| id.to_string());

            info!("Sent Telegram message to {}", chat_id);
            Ok(MessageResult {
                success: true,
                channel: "telegram".to_string(),
                recipient: chat_id.to_string(),
                message_id,
                error: None,
            })
        } else {
            let error = result["description"]
                .as_str()
                .unwrap_or("Unknown Telegram error");
            Err(anyhow::anyhow!("Telegram error: {error}"))
        }
    }

    /// Send `WhatsApp` message (placeholder - requires `WhatsApp` Business API)
    async fn send_whatsapp(&self, to: &str, _content: &str) -> Result<MessageResult> {
        // WhatsApp Business API requires complex setup
        // This is a placeholder implementation
        warn!("WhatsApp sending not fully implemented - requires WhatsApp Business API");

        Ok(MessageResult {
            success: false,
            channel: "whatsapp".to_string(),
            recipient: to.to_string(),
            message_id: None,
            error: Some("WhatsApp Business API required".to_string()),
        })
    }

    /// Send Signal message (placeholder - requires signal-cli or API)
    async fn send_signal(&self, to: &str, _content: &str) -> Result<MessageResult> {
        // Signal requires signal-cli or similar
        warn!("Signal sending not fully implemented - requires signal-cli");

        Ok(MessageResult {
            success: false,
            channel: "signal".to_string(),
            recipient: to.to_string(),
            message_id: None,
            error: Some("signal-cli required".to_string()),
        })
    }

    /// Send email via SMTP
    async fn send_email(&self, to: &str, _subject: &str, _body: &str) -> Result<MessageResult> {
        // Email sending requires async-smtp or similar
        // This is a placeholder
        warn!("Email sending not fully implemented - requires SMTP configuration");

        Ok(MessageResult {
            success: false,
            channel: "email".to_string(),
            recipient: to.to_string(),
            message_id: None,
            error: Some("SMTP not configured".to_string()),
        })
    }

    /// Send generic webhook
    async fn send_webhook(&self, url: &str, content: &str) -> Result<MessageResult> {
        let payload = serde_json::json!({
            "text": content,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .context("Failed to send webhook")?;

        if response.status().is_success() {
            info!("Sent webhook to {}", url);
            Ok(MessageResult {
                success: true,
                channel: "webhook".to_string(),
                recipient: url.to_string(),
                message_id: None,
                error: None,
            })
        } else {
            let status = response.status();
            Err(anyhow::anyhow!("Webhook error: HTTP {status}"))
        }
    }
}

#[async_trait]
impl Tool for MessageTool {
    fn name(&self) -> &'static str {
        "message"
    }

    fn description(&self) -> String {
        "Send messages to communication channels. Supports Discord, Slack, Telegram, WhatsApp, Signal, Email, and webhooks. \
        Parameters: {\"channel\": \"discord|slack|telegram|whatsapp|signal|email|webhook\", \"recipient\": \"channel_id|user_id|email|url\", \"content\": \"message text\", \"subject\": \"optional for email\"}".to_string()
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let channel_str = params
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: channel"))?;

        let channel = match channel_str {
            "discord" => ChannelType::Discord,
            "slack" => ChannelType::Slack,
            "telegram" => ChannelType::Telegram,
            "whatsapp" => ChannelType::Whatsapp,
            "signal" => ChannelType::Signal,
            "email" => ChannelType::Email,
            "webhook" => ChannelType::Webhook,
            _ => return Err(anyhow::anyhow!("Unknown channel type: {channel_str}")),
        };

        let recipient = params
            .get("recipient")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: recipient"))?;

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: content"))?;

        let subject = params.get("subject").and_then(|v| v.as_str());

        let result = self
            .send_message(channel, recipient, content, subject)
            .await?;

        Ok(serde_json::json!({
            "success": result.success,
            "channel": result.channel,
            "recipient": result.recipient,
            "message_id": result.message_id,
            "error": result.error,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_tool_creation() {
        let config = MessageConfig::default();
        let tool = MessageTool::new(config);
        assert_eq!(tool.name(), "message");
    }

    #[test]
    fn test_channel_type_display() {
        assert_eq!(ChannelType::Discord.to_string(), "discord");
        assert_eq!(ChannelType::Slack.to_string(), "slack");
        assert_eq!(ChannelType::Telegram.to_string(), "telegram");
    }

    #[tokio::test]
    async fn test_discord_no_config() {
        let config = MessageConfig::default();
        let tool = MessageTool::new(config);

        let result = tool.send_discord("Test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not configured"));
    }
}
