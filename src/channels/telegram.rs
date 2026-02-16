//! Telegram channel implementation

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use crate::channels::Channel;

/// Telegram channel configuration
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    /// Bot token from @BotFather
    pub bot_token: String,
    /// Allowed chat IDs (empty = allow all)
    pub allowed_chats: Vec<i64>,
    /// Poll interval in seconds
    pub poll_interval_secs: u64,
}

impl TelegramConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("TELEGRAM_BOT_TOKEN not set"))?;
        
        Ok(Self {
            bot_token,
            allowed_chats: vec![],
            poll_interval_secs: 2,
        })
    }
}

/// Telegram channel for bot integration
pub struct TelegramChannel {
    config: TelegramConfig,
    client: reqwest::Client,
    message_tx: mpsc::Sender<String>,
    message_rx: mpsc::Receiver<String>,
    chat_id_tx: mpsc::Sender<i64>,
    chat_id_rx: Mutex<mpsc::Receiver<i64>>,
    last_update_id: Option<i64>,
}

impl TelegramChannel {
    /// Create a new Telegram channel
    pub fn new(config: TelegramConfig) -> Self {
        let (message_tx, message_rx) = mpsc::channel(100);
        let (chat_id_tx, chat_id_rx) = mpsc::channel(100);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            message_tx,
            message_rx,
            chat_id_tx,
            chat_id_rx: Mutex::new(chat_id_rx),
            last_update_id: None,
        }
    }

    /// Start polling for updates
    pub async fn start_polling(mut self: Arc<Self>) -> anyhow::Result<()> {
        info!("Starting Telegram polling");

        loop {
            match self.poll_updates().await {
                Ok(()) => {}
                Err(e) => {
                    error!("Polling error: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(
                self.config.poll_interval_secs
            )).await;
        }
    }

    /// Poll for updates from Telegram
    async fn poll_updates(&self
    ) -> anyhow::Result<()> {
        let url = format!(
            "https://api.telegram.org/bot{}/getUpdates",
            self.config.bot_token
        );

        let mut params = serde_json::Map::new();
        params.insert("limit".to_string(), json!(100));
        
        if let Some(offset) = self.last_update_id {
            params.insert("offset".to_string(), json!(offset + 1));
        }

        let response = self.client
            .post(&url)
            .json(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow::anyhow!("Telegram API error: {}", error_text));
        }

        let updates: TelegramResponse<Vec<Update>> = response.json().await?;
        
        if !updates.ok {
            return Err(anyhow::anyhow!("Telegram API returned error"));
        }

        for update in updates.result {
            self.process_update(update).await?;
        }

        Ok(())
    }

    /// Process a single update
    async fn process_update(
        &self,
        update: Update
    ) -> anyhow::Result<()> {
        let update_id = update.update_id;
        
        let message = match update.message {
            Some(m) => m,
            None => return Ok(()),
        };

        let chat_id = message.chat.id;

        // Check allowed chats
        if !self.config.allowed_chats.is_empty() 
            && !self.config.allowed_chats.contains(&chat_id) {
            warn!("Ignoring message from unauthorized chat: {}", chat_id);
            return Ok(());
        }

        // Get text content
        let text = message.text.unwrap_or_default();
        if text.is_empty() {
            return Ok(());
        }

        // Store chat ID for replies
        let _ = self.chat_id_tx.send(chat_id).await;
        
        // Send message content
        self.message_tx.send(text).await?;
        
        Ok(())
    }

    /// Send a message to Telegram
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
    ) -> anyhow::Result<()> {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.config.bot_token
        );

        let params = json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown"
        });

        let response = self.client
            .post(&url)
            .json(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            return Err(anyhow::anyhow!("Failed to send message: {}", error));
        }

        Ok(())
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn send(&mut self, message: &str) -> anyhow::Result<()> {
        // Try to get the last chat ID for replies
        let mut chat_id_rx = self.chat_id_rx.lock().await;
        if let Ok(chat_id) = chat_id_rx.try_recv() {
            self.send_message(chat_id, message).await?;
        } else {
            warn!("No chat ID available for reply");
        }
        Ok(())
    }

    async fn receive(&mut self) -> anyhow::Result<Option<String>> {
        match self.message_rx.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("Channel error: {}", e)),
        }
    }
}

// Telegram API types

#[derive(Debug, serde::Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: T,
}

#[derive(Debug, serde::Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Debug, serde::Deserialize)]
struct Message {
    message_id: i64,
    from: Option<User>,
    chat: Chat,
    text: Option<String>,
    date: i64,
}

#[derive(Debug, serde::Deserialize)]
struct User {
    id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
    title: Option<String>,
    username: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telegram_channel_creation() {
        let config = TelegramConfig {
            bot_token: "test_token".to_string(),
            allowed_chats: vec![],
            poll_interval_secs: 2,
        };

        let channel = TelegramChannel::new(config);
        assert_eq!(channel.name(), "telegram");
    }

    #[tokio::test]
    #[ignore]
    async fn test_send_message() {
        let config = TelegramConfig::from_env().unwrap();
        let channel = TelegramChannel::new(config);

        // This would send a real message - use a test chat
        // channel.send_message(TEST_CHAT_ID, "Test message").await.unwrap();
    }
}
