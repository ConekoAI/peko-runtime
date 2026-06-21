//! Session overlay trait and implementations
//!
//! Provides the `SessionOverlay` trait and concrete implementations:
//! - `ChannelOverlay`: Channel-specific state storage
//! - `ChannelContext`: Interface for channel-specific data

use crate::auth::principal::Principal;
use super::types::{ChannelType, OverlayType};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Trait for session overlays
///
/// Overlays provide context-specific state that layers on top of
/// a base session. They enable:
/// - Channel-specific settings (e.g., Discord guild IDs)
/// - Spawn isolation for subagent tasks
#[async_trait]
pub trait SessionOverlay: Send + Sync {
    /// Get the overlay type
    fn overlay_type(&self) -> OverlayType;

    /// Get the overlay ID (unique within the base session)
    fn overlay_id(&self) -> &str;

    /// Whether this overlay should be persisted
    fn persist(&self) -> bool;

    /// Serialize to JSON
    fn to_json(&self) -> Value;

    /// Get parent base session key
    fn base_session_key(&self) -> &str;

    /// Get the peer this overlay belongs to
    fn peer(&self) -> &Principal;

    /// Get creation timestamp
    fn created_at(&self) -> DateTime<Utc>;
}

/// Channel-specific context interface
///
/// Implemented by channel overlays to provide channel-specific data access.
pub trait ChannelContext: Send + Sync {
    /// Get the channel type
    fn channel_type(&self) -> ChannelType;

    /// Get the channel ID
    fn channel_id(&self) -> &str;

    /// Get a state value by key
    fn get_state(&self, key: &str) -> Option<&Value>;

    /// Set a state value
    fn set_state(&mut self, key: &str, value: Value);
}

/// Channel overlay with channel-specific state
///
/// Each communication channel (CLI, Discord, etc.) gets its own overlay
/// that stores channel-specific context while sharing the base session's
/// conversation history.
#[derive(Debug, Clone)]
pub struct ChannelOverlay {
    /// Unique overlay ID
    pub overlay_id: String,
    /// Parent base session key
    pub base_session_key: String,
    /// The peer this overlay belongs to
    pub peer: Principal,
    /// Type of channel
    pub channel_type: ChannelType,
    /// Channel-specific identifier (e.g., guild ID for Discord)
    pub channel_id: String,
    /// Channel-specific state storage
    pub state: HashMap<String, Value>,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Last accessed timestamp
    pub last_accessed: DateTime<Utc>,
}

impl ChannelOverlay {
    /// Create a new channel overlay
    ///
    /// # Arguments
    /// * `base_session_key` - The parent base session key
    /// * `peer` - The peer this overlay belongs to
    /// * `channel_type` - The type of communication channel
    /// * `channel_id` - The channel-specific identifier
    pub fn new(
        base_session_key: impl Into<String>,
        peer: Principal,
        channel_type: ChannelType,
        channel_id: impl Into<String>,
    ) -> Self {
        let base = base_session_key.into();
        let channel_id_str = channel_id.into();

        // Generate overlay ID from channel type and ID
        let overlay_id = format!("{}:{}", channel_type.as_str(), channel_id_str);

        let now = Utc::now();

        Self {
            overlay_id,
            base_session_key: base,
            peer,
            channel_type,
            channel_id: channel_id_str,
            state: HashMap::new(),
            created_at: now,
            last_accessed: now,
        }
    }

    /// Update the last accessed timestamp
    pub fn touch(&mut self) {
        self.last_accessed = Utc::now();
    }

    /// Get a state value by key
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.state.get(key)
    }

    /// Set a state value
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.state.insert(key.into(), value.into());
        self.touch();
    }

    /// Remove a state value
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let result = self.state.remove(key);
        if result.is_some() {
            self.touch();
        }
        result
    }

    /// Check if a key exists in state
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.state.contains_key(key)
    }

    /// Get state as JSON object
    #[must_use]
    pub fn state_json(&self) -> Value {
        serde_json::to_value(&self.state).unwrap_or_else(|_| Value::Object(Default::default()))
    }

    /// Create from stored data (for deserialization)
    pub fn from_stored(
        base_session_key: impl Into<String>,
        peer: Principal,
        channel_type: ChannelType,
        channel_id: impl Into<String>,
        state: HashMap<String, Value>,
        created_at: DateTime<Utc>,
    ) -> Self {
        let channel_id_str = channel_id.into();
        let overlay_id = format!("{}:{}", channel_type.as_str(), channel_id_str);

        Self {
            overlay_id,
            base_session_key: base_session_key.into(),
            peer,
            channel_type,
            channel_id: channel_id_str,
            state,
            created_at,
            last_accessed: Utc::now(),
        }
    }
}

#[async_trait]
impl SessionOverlay for ChannelOverlay {
    fn overlay_type(&self) -> OverlayType {
        OverlayType::Channel(self.channel_type)
    }

    fn overlay_id(&self) -> &str {
        &self.overlay_id
    }

    fn persist(&self) -> bool {
        // Channel overlays are persisted by default
        true
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "type": "channel",
            "overlay_id": self.overlay_id,
            "base_session_key": self.base_session_key,
            "peer": self.peer,
            "channel_type": self.channel_type,
            "channel_id": self.channel_id,
            "state": self.state,
            "created_at": self.created_at,
            "last_accessed": self.last_accessed,
        })
    }

    fn base_session_key(&self) -> &str {
        &self.base_session_key
    }

    fn peer(&self) -> &Principal {
        &self.peer
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl ChannelContext for ChannelOverlay {
    fn channel_type(&self) -> ChannelType {
        self.channel_type
    }

    fn channel_id(&self) -> &str {
        &self.channel_id
    }

    fn get_state(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }

    fn set_state(&mut self, key: &str, value: Value) {
        self.set(key, value);
    }
}

/// Serializable representation of a channel overlay for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelOverlayData {
    pub overlay_id: String,
    pub base_session_key: String,
    pub peer: Principal,
    pub channel_type: ChannelType,
    pub channel_id: String,
    pub state: HashMap<String, Value>,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
}

impl From<ChannelOverlay> for ChannelOverlayData {
    fn from(overlay: ChannelOverlay) -> Self {
        Self {
            overlay_id: overlay.overlay_id,
            base_session_key: overlay.base_session_key,
            peer: overlay.peer,
            channel_type: overlay.channel_type,
            channel_id: overlay.channel_id,
            state: overlay.state,
            created_at: overlay.created_at,
            last_accessed: overlay.last_accessed,
        }
    }
}

impl From<ChannelOverlayData> for ChannelOverlay {
    fn from(data: ChannelOverlayData) -> Self {
        Self {
            overlay_id: data.overlay_id,
            base_session_key: data.base_session_key,
            peer: data.peer,
            channel_type: data.channel_type,
            channel_id: data.channel_id,
            state: data.state,
            created_at: data.created_at,
            last_accessed: data.last_accessed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_overlay_new() {
        let overlay = ChannelOverlay::new(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Discord,
            "guild123",
        );

        assert_eq!(overlay.overlay_id, "discord:guild123");
        assert_eq!(overlay.base_session_key, "agent:test:peer:user:alice");
        assert_eq!(overlay.channel_type, ChannelType::Discord);
        assert_eq!(overlay.channel_id, "guild123");
        assert!(overlay.state.is_empty());
        assert!(overlay.persist());
    }

    #[test]
    fn test_channel_overlay_state() {
        let mut overlay = ChannelOverlay::new(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Discord,
            "guild123",
        );

        // Set state
        overlay.set("guild_id", "123456");
        overlay.set("user_nickname", "Ali");

        assert_eq!(
            overlay.get("guild_id"),
            Some(&Value::String("123456".to_string()))
        );
        assert_eq!(
            overlay.get("user_nickname"),
            Some(&Value::String("Ali".to_string()))
        );
        assert_eq!(overlay.get("missing"), None);

        // Check contains
        assert!(overlay.contains("guild_id"));
        assert!(!overlay.contains("missing"));

        // Remove
        let removed = overlay.remove("guild_id");
        assert_eq!(removed, Some(Value::String("123456".to_string())));
        assert!(!overlay.contains("guild_id"));
    }

    #[test]
    fn test_channel_overlay_overlay_type() {
        let overlay = ChannelOverlay::new(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Cli,
            "default",
        );

        let overlay_type = overlay.overlay_type();
        assert!(overlay_type.is_channel());
        assert!(!overlay_type.is_spawn());
        assert_eq!(overlay_type.channel_type(), Some(ChannelType::Cli));
    }

    #[test]
    fn test_channel_overlay_to_json() {
        let mut overlay = ChannelOverlay::new(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Discord,
            "guild123",
        );
        overlay.set("key1", "value1");

        let json = overlay.to_json();

        assert_eq!(json["type"], "channel");
        assert_eq!(json["overlay_id"], "discord:guild123");
        assert_eq!(json["base_session_key"], "agent:test:peer:user:alice");
        // ChannelType serializes as enum variant name by default
        assert!(json["channel_type"].as_str().is_some());
        assert_eq!(json["channel_id"], "guild123");
        assert!(json["state"]["key1"].is_string());
    }

    #[test]
    fn test_channel_overlay_serialization() {
        let mut overlay = ChannelOverlay::new(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Discord,
            "guild123",
        );
        overlay.set("test_key", "test_value");

        // Convert to data struct and serialize
        let data: ChannelOverlayData = overlay.clone().into();
        let json = serde_json::to_string(&data).unwrap();

        // Deserialize back
        let data2: ChannelOverlayData = serde_json::from_str(&json).unwrap();
        let overlay2: ChannelOverlay = data2.into();

        assert_eq!(overlay.overlay_id, overlay2.overlay_id);
        assert_eq!(overlay.channel_type, overlay2.channel_type);
        assert_eq!(overlay.state, overlay2.state);
    }

    #[test]
    fn test_channel_context_trait() {
        let mut overlay = ChannelOverlay::new(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Discord,
            "guild123",
        );

        // As ChannelContext
        let ctx: &dyn ChannelContext = &overlay;
        assert_eq!(ctx.channel_type(), ChannelType::Discord);
        assert_eq!(ctx.channel_id(), "guild123");

        // Set via trait
        ChannelContext::set_state(
            &mut overlay,
            "via_trait",
            Value::String("value".to_string()),
        );
        assert_eq!(
            ChannelContext::get_state(&overlay, "via_trait"),
            Some(&Value::String("value".to_string()))
        );
    }

    #[test]
    fn test_touch_updates_last_accessed() {
        let mut overlay = ChannelOverlay::new(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Cli,
            "default",
        );

        let before = overlay.last_accessed;

        // Small delay to ensure timestamp difference
        std::thread::sleep(std::time::Duration::from_millis(10));

        overlay.touch();

        assert!(overlay.last_accessed > before);
    }

    #[test]
    fn test_from_stored() {
        let mut state = HashMap::new();
        state.insert("key".to_string(), Value::String("value".to_string()));

        let created = Utc::now();

        let overlay = ChannelOverlay::from_stored(
            "agent:test:peer:user:alice",
            Principal::User("alice".to_string()),
            ChannelType::Discord,
            "guild123",
            state,
            created,
        );

        assert_eq!(overlay.overlay_id, "discord:guild123");
        assert_eq!(overlay.created_at, created);
        assert_eq!(
            overlay.get("key"),
            Some(&Value::String("value".to_string()))
        );
    }
}
