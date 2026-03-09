//! `WhatsApp` channel implementation
//!
//! Uses `WhatsApp` Business Cloud API for sending messages.
//! Receiving messages requires webhook setup (simplified for now).

use super::Channel;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::VecDeque;
use tracing::{debug, error, info, warn};

/// `WhatsApp` channel configuration
#[derive(Debug, Clone)]
pub struct WhatsAppConfig {
    /// `WhatsApp` Business API access token
    pub access_token: String,
    /// `WhatsApp` phone number ID
    pub phone_number_id: String,
    /// Webhook verify token (for receiving)
    pub verify_token: String,
    /// List of allowed phone numbers (E.164 format: +1234567890)
    pub allowed_numbers: Vec<String>,
}

impl WhatsAppConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let access_token = std::env::var("WHATSAPP_ACCESS_TOKEN")
            .map_err(|_| anyhow::anyhow!("WHATSAPP_ACCESS_TOKEN not set"))?;
        let phone_number_id = std::env::var("WHATSAPP_PHONE_NUMBER_ID")
            .map_err(|_| anyhow::anyhow!("WHATSAPP_PHONE_NUMBER_ID not set"))?;
        let verify_token = std::env::var("WHATSAPP_VERIFY_TOKEN")
            .map_err(|_| anyhow::anyhow!("WHATSAPP_VERIFY_TOKEN not set"))?;

        Ok(Self {
            access_token,
            phone_number_id,
            verify_token,
            allowed_numbers: Vec::new(),
        })
    }
}

/// `WhatsApp` channel for bot communication
pub struct WhatsAppChannel {
    config: WhatsAppConfig,
    client: reqwest::Client,
    message_queue: VecDeque<String>,
    last_recipient: Option<String>,
}

impl WhatsAppChannel {
    /// Create new `WhatsApp` channel
    pub fn new(config: WhatsAppConfig) -> Self {
        info!("WhatsApp channel initialized");
        Self {
            config,
            client: reqwest::Client::new(),
            message_queue: VecDeque::new(),
            last_recipient: None,
        }
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(WhatsAppConfig::from_env()?))
    }

    /// Check if phone number is allowed
    fn is_number_allowed(&self, phone: &str) -> bool {
        if self.config.allowed_numbers.is_empty() {
            return true; // Allow all if no restrictions
        }
        // Normalize phone number
        let normalized = if phone.starts_with('+') {
            phone.to_string()
        } else {
            format!("+{phone}")
        };
        self.config
            .allowed_numbers
            .iter()
            .any(|n| n == "*" || n == &normalized)
    }

    /// Get the verify token for webhook verification
    #[must_use]
    pub fn verify_token(&self) -> &str {
        &self.config.verify_token
    }

    /// Send message to a specific phone number
    async fn send_to_number(&self, phone_number: &str, message: &str) -> Result<()> {
        // Normalize phone number
        let to = if phone_number.starts_with('+') {
            phone_number.to_string()
        } else {
            format!("+{phone_number}")
        };

        // Check if number is allowed
        if !self.is_number_allowed(&to) {
            return Err(anyhow::anyhow!("Phone number not in allowlist: {to}"));
        }

        let url = format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.config.phone_number_id
        );

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": to,
            "type": "text",
            "text": {
                "body": message
            }
        });

        let response = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.access_token),
            )
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error = response.text().await.unwrap_or_default();
            error!("WhatsApp API error: {} - {}", status, error);
            return Err(anyhow::anyhow!("WhatsApp API error: {status} - {error}"));
        }

        // Check for API-level errors
        let result: serde_json::Value = response.json().await?;
        if let Some(error) = result.get("error") {
            return Err(anyhow::anyhow!("WhatsApp API error: {error:?}"));
        }

        debug!("Sent message to WhatsApp number {}", to);
        Ok(())
    }

    /// Parse incoming webhook payload (simplified)
    ///
    /// In a full implementation, this would be called by your webhook handler
    pub fn parse_webhook_payload(&mut self, payload: &serde_json::Value) -> Vec<String> {
        let mut messages = Vec::new();

        // WhatsApp Cloud API webhook structure
        let Some(entries) = payload.get("entry").and_then(|e| e.as_array()) else {
            return messages;
        };

        for entry in entries {
            let Some(changes) = entry.get("changes").and_then(|c| c.as_array()) else {
                continue;
            };

            for change in changes {
                let Some(value) = change.get("value") else {
                    continue;
                };

                // Extract messages
                let Some(msgs) = value.get("messages").and_then(|m| m.as_array()) else {
                    continue;
                };

                for msg in msgs {
                    // Get sender
                    let Some(from) = msg.get("from").and_then(|f| f.as_str()) else {
                        continue;
                    };

                    // Check allowlist
                    if !self.is_number_allowed(from) {
                        continue;
                    }

                    // Store sender for replies
                    self.last_recipient = Some(from.to_string());

                    // Extract text content
                    if let Some(text) = msg
                        .get("text")
                        .and_then(|t| t.get("body"))
                        .and_then(|b| b.as_str())
                    {
                        messages.push(text.to_string());
                    }
                }
            }
        }

        messages
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &'static str {
        "whatsapp"
    }

    async fn send(&mut self, message: &str) -> Result<()> {
        if let Some(recipient) = self.last_recipient.as_ref() {
            self.send_to_number(recipient, message).await?;
        } else {
            warn!("No WhatsApp recipient available for sending");
            return Err(anyhow::anyhow!("No WhatsApp recipient configured"));
        }
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        // Return queued messages from webhook parsing
        // In production, this would be populated by your webhook handler
        Ok(self.message_queue.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_whatsapp_channel_creation() {
        let config = WhatsAppConfig {
            access_token: "test_token".to_string(),
            phone_number_id: "123456".to_string(),
            verify_token: "verify_123".to_string(),
            allowed_numbers: vec![],
        };
        let channel = WhatsAppChannel::new(config);
        assert_eq!(channel.name(), "whatsapp");
    }

    #[test]
    fn test_number_allowed() {
        let config = WhatsAppConfig {
            access_token: "test".to_string(),
            phone_number_id: "123".to_string(),
            verify_token: "verify".to_string(),
            allowed_numbers: vec!["+1234567890".to_string()],
        };
        let channel = WhatsAppChannel::new(config);
        assert!(channel.is_number_allowed("+1234567890"));
        assert!(channel.is_number_allowed("1234567890")); // Without +
        assert!(!channel.is_number_allowed("+9876543210"));
    }

    #[test]
    fn test_number_allowed_wildcard() {
        let config = WhatsAppConfig {
            access_token: "test".to_string(),
            phone_number_id: "123".to_string(),
            verify_token: "verify".to_string(),
            allowed_numbers: vec!["*".to_string()],
        };
        let channel = WhatsAppChannel::new(config);
        assert!(channel.is_number_allowed("+any_number"));
    }

    #[test]
    fn test_verify_token() {
        let config = WhatsAppConfig {
            access_token: "test".to_string(),
            phone_number_id: "123".to_string(),
            verify_token: "my_verify_token".to_string(),
            allowed_numbers: vec![],
        };
        let channel = WhatsAppChannel::new(config);
        assert_eq!(channel.verify_token(), "my_verify_token");
    }

    #[test]
    fn test_parse_webhook_payload() {
        let config = WhatsAppConfig {
            access_token: "test".to_string(),
            phone_number_id: "123".to_string(),
            verify_token: "verify".to_string(),
            allowed_numbers: vec!["*".to_string()],
        };
        let mut channel = WhatsAppChannel::new(config);

        let payload = serde_json::json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "+1234567890",
                            "text": {
                                "body": "Hello from WhatsApp"
                            }
                        }]
                    }
                }]
            }]
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0], "Hello from WhatsApp");
    }
}
