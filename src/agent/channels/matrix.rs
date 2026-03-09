//! Matrix channel implementation
//!
//! Uses Matrix Client-Server API for communication.

use super::Channel;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::VecDeque;
use tracing::{debug, error, info};

/// Matrix channel configuration
#[derive(Debug, Clone)]
pub struct MatrixConfig {
    /// Matrix homeserver URL (e.g., "<https://matrix.org>")
    pub homeserver: String,
    /// Access token for authentication
    pub access_token: String,
    /// Room ID to join (e.g., "!room:matrix.org")
    pub room_id: String,
    /// List of allowed user IDs (empty = allow all)
    pub allowed_users: Vec<String>,
}

impl MatrixConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let homeserver = std::env::var("MATRIX_HOMESERVER")
            .map_err(|_| anyhow::anyhow!("MATRIX_HOMESERVER not set"))?;
        let access_token = std::env::var("MATRIX_ACCESS_TOKEN")
            .map_err(|_| anyhow::anyhow!("MATRIX_ACCESS_TOKEN not set"))?;
        let room_id = std::env::var("MATRIX_ROOM_ID")
            .map_err(|_| anyhow::anyhow!("MATRIX_ROOM_ID not set"))?;

        Ok(Self {
            homeserver,
            access_token,
            room_id,
            allowed_users: Vec::new(),
        })
    }
}

/// Matrix channel for bot communication
pub struct MatrixChannel {
    config: MatrixConfig,
    client: reqwest::Client,
    message_queue: VecDeque<String>,
    bot_user_id: Option<String>,
    next_batch: Option<String>,
}

/// Matrix sync response
#[derive(Debug, Deserialize)]
struct SyncResponse {
    next_batch: String,
    #[serde(default)]
    rooms: Rooms,
}

#[derive(Debug, Deserialize, Default)]
struct Rooms {
    #[serde(default)]
    join: std::collections::HashMap<String, JoinedRoom>,
}

#[derive(Debug, Deserialize)]
struct JoinedRoom {
    #[serde(default)]
    timeline: Timeline,
}

#[derive(Debug, Deserialize, Default)]
struct Timeline {
    #[serde(default)]
    events: Vec<TimelineEvent>,
}

#[derive(Debug, Deserialize)]
struct TimelineEvent {
    #[serde(rename = "type")]
    event_type: String,
    sender: String,
    #[serde(default)]
    content: EventContent,
}

#[derive(Debug, Deserialize, Default)]
struct EventContent {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    msgtype: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WhoAmIResponse {
    user_id: String,
}

impl MatrixChannel {
    /// Create new Matrix channel
    pub fn new(config: MatrixConfig) -> Self {
        let homeserver = config.homeserver.trim_end_matches('/').to_string();

        info!("Matrix channel initialized for {}", homeserver);

        Self {
            config: MatrixConfig {
                homeserver,
                ..config
            },
            client: reqwest::Client::new(),
            message_queue: VecDeque::new(),
            bot_user_id: None,
            next_batch: None,
        }
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(MatrixConfig::from_env()?))
    }

    /// Check if user is allowed
    fn is_user_allowed(&self, user_id: &str) -> bool {
        if self.config.allowed_users.is_empty() {
            return true; // Allow all if no restrictions
        }
        self.config
            .allowed_users
            .iter()
            .any(|u| u == "*" || u == user_id)
    }

    /// Get bot's own user ID
    async fn fetch_bot_user_id(&mut self) -> Result<Option<String>> {
        let url = format!(
            "{}/_matrix/client/v3/account/whoami",
            self.config.homeserver
        );

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.access_token),
            )
            .send()
            .await?;

        if !response.status().is_success() {
            error!("Failed to get Matrix user ID: {}", response.status());
            return Ok(None);
        }

        let result: WhoAmIResponse = response.json().await?;
        self.bot_user_id = Some(result.user_id.clone());
        Ok(Some(result.user_id))
    }

    /// Send message to the configured room
    async fn send_message(&self, message: &str) -> Result<()> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.config.homeserver,
            urlencoding::encode(&self.config.room_id),
            uuid::Uuid::new_v4()
        );

        let body = serde_json::json!({
            "msgtype": "m.text",
            "body": message
        });

        let response = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.access_token),
            )
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await.unwrap_or_default();
            error!("Matrix API error: {}", error);
            return Err(anyhow::anyhow!("Matrix API error: {error}"));
        }

        debug!("Sent message to Matrix room {}", self.config.room_id);
        Ok(())
    }

    /// Poll for new messages via sync
    async fn sync_messages(&mut self) -> Result<Vec<String>> {
        let mut url = format!(
            "{}/_matrix/client/v3/sync?timeout=30000",
            self.config.homeserver
        );

        if let Some(next_batch) = &self.next_batch {
            url.push_str(&format!("&since={next_batch}"));
        }

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.access_token),
            )
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Matrix sync error: {error}"));
        }

        let sync: SyncResponse = response.json().await?;
        self.next_batch = Some(sync.next_batch);

        let mut messages = Vec::new();
        let bot_id = self.bot_user_id.clone().unwrap_or_default();

        // Check our room for new messages
        if let Some(room) = sync.rooms.join.get(&self.config.room_id) {
            for event in &room.timeline.events {
                // Only process text messages
                if event.event_type != "m.room.message" {
                    continue;
                }

                // Skip bot's own messages
                if event.sender == bot_id {
                    continue;
                }

                // Check if user is allowed
                if !self.is_user_allowed(&event.sender) {
                    continue;
                }

                // Extract message body
                if let Some(body) = &event.content.body {
                    if event.content.msgtype.as_deref() == Some("m.text") {
                        messages.push(body.clone());
                    }
                }
            }
        }

        Ok(messages)
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &'static str {
        "matrix"
    }

    async fn send(&mut self, message: &str) -> Result<()> {
        self.send_message(message).await
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
        match self.sync_messages().await {
            Ok(messages) => {
                for msg in messages {
                    self.message_queue.push_back(msg);
                }
            }
            Err(e) => {
                error!("Failed to sync Matrix messages: {}", e);
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
    fn test_matrix_channel_creation() {
        let config = MatrixConfig {
            homeserver: "https://matrix.org".to_string(),
            access_token: "test_token".to_string(),
            room_id: "!test:matrix.org".to_string(),
            allowed_users: vec![],
        };
        let channel = MatrixChannel::new(config);
        assert_eq!(channel.name(), "matrix");
        assert_eq!(channel.config.homeserver, "https://matrix.org");
    }

    #[test]
    fn test_homeserver_trailing_slash_removed() {
        let config = MatrixConfig {
            homeserver: "https://matrix.org/".to_string(),
            access_token: "test".to_string(),
            room_id: "!test:matrix.org".to_string(),
            allowed_users: vec![],
        };
        let channel = MatrixChannel::new(config);
        assert_eq!(channel.config.homeserver, "https://matrix.org");
    }

    #[test]
    fn test_user_allowed() {
        let config = MatrixConfig {
            homeserver: "https://matrix.org".to_string(),
            access_token: "test".to_string(),
            room_id: "!test:matrix.org".to_string(),
            allowed_users: vec!["@alice:matrix.org".to_string()],
        };
        let channel = MatrixChannel::new(config);
        assert!(channel.is_user_allowed("@alice:matrix.org"));
        assert!(!channel.is_user_allowed("@bob:matrix.org"));
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let config = MatrixConfig {
            homeserver: "https://matrix.org".to_string(),
            access_token: "test".to_string(),
            room_id: "!test:matrix.org".to_string(),
            allowed_users: vec!["*".to_string()],
        };
        let channel = MatrixChannel::new(config);
        assert!(channel.is_user_allowed("@anyone:matrix.org"));
    }
}
