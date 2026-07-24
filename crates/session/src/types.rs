//! Core types for session overlay architecture
//!
//! This module provides the foundational types for the hybrid session model:
//! - `ChannelType`: Communication channel variants
//! - `OverlayType`: Classification of overlay kinds
//!
//! Session ownership identity uses `peko_subject::Subject`
//! (ADR-039). The former `Subject` type alias was removed in the
//! `refactor/peer-to-principal-rename` cleanup; callers should now
//! import `Subject` directly from `peko_subject`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Communication channel types
///
/// Each variant represents a different communication medium that
/// can have its own overlay with channel-specific state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ChannelType {
    /// Command line interface
    #[default]
    Cli,
    /// Discord messaging platform
    Discord,
    /// Telegram messaging platform
    Telegram,
    /// `WhatsApp` messaging platform
    WhatsApp,
    /// Slack messaging platform
    Slack,
    /// Generic web interface
    Web,
    /// HTTP API interface
    Http,
    /// Signal messaging platform
    Signal,
    /// Matrix messaging platform
    Matrix,
}

impl ChannelType {
    /// Get the channel type as a string slice
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            ChannelType::Cli => "cli",
            ChannelType::Discord => "discord",
            ChannelType::Telegram => "telegram",
            ChannelType::WhatsApp => "whatsapp",
            ChannelType::Slack => "slack",
            ChannelType::Web => "web",
            ChannelType::Http => "http",
            ChannelType::Signal => "signal",
            ChannelType::Matrix => "matrix",
        }
    }

    /// Parse a channel type from a string
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "cli" => Some(ChannelType::Cli),
            "discord" => Some(ChannelType::Discord),
            "telegram" => Some(ChannelType::Telegram),
            "whatsapp" => Some(ChannelType::WhatsApp),
            "slack" => Some(ChannelType::Slack),
            "web" => Some(ChannelType::Web),
            "http" => Some(ChannelType::Http),
            "signal" => Some(ChannelType::Signal),
            "matrix" => Some(ChannelType::Matrix),
            _ => None,
        }
    }

    /// Check if this channel type supports rich formatting
    #[must_use]
    pub const fn supports_rich_formatting(&self) -> bool {
        matches!(
            self,
            ChannelType::Discord | ChannelType::Slack | ChannelType::Web
        )
    }

    /// Check if this channel type supports threaded conversations
    #[must_use]
    pub const fn supports_threads(&self) -> bool {
        matches!(
            self,
            ChannelType::Discord | ChannelType::Slack | ChannelType::Telegram
        )
    }
}

impl fmt::Display for ChannelType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Types of session overlays
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlayType {
    /// Channel-specific overlay
    Channel(ChannelType),
    /// Spawn/subagent overlay
    Spawn,
}

impl OverlayType {
    /// Get the overlay type as a string
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            OverlayType::Channel(_) => "channel",
            OverlayType::Spawn => "spawn",
        }
    }

    /// Check if this is a channel overlay
    #[must_use]
    pub const fn is_channel(&self) -> bool {
        matches!(self, OverlayType::Channel(_))
    }

    /// Check if this is a spawn overlay
    #[must_use]
    pub const fn is_spawn(&self) -> bool {
        matches!(self, OverlayType::Spawn)
    }

    /// Get the channel type if this is a channel overlay
    #[must_use]
    pub const fn channel_type(&self) -> Option<ChannelType> {
        match self {
            OverlayType::Channel(ct) => Some(*ct),
            OverlayType::Spawn => None,
        }
    }
}

impl fmt::Display for OverlayType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OverlayType::Channel(ct) => write!(f, "channel:{ct}"),
            OverlayType::Spawn => write!(f, "spawn"),
        }
    }
}

/// Cleanup policy for spawn overlays.
///
/// Phase 7 promoted this enum from
/// `peko_extension_host::subagent::SpawnCleanupPolicy` (where
/// Phase 8 commit 2 had moved it) back into `peko-session` — the
/// canonical home is the session overlay architecture, not the
/// framework async-execution payload. The host crate re-exports
/// from here (in a Phase 8 follow-up) so framework code keeps
/// compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpawnCleanupPolicy {
    /// Keep the spawn overlay after the subagent finishes.
    #[default]
    Keep,
    /// Delete the spawn overlay after the subagent finishes.
    Delete,
}

impl SpawnCleanupPolicy {
    /// String form used in serialized config / CLI args.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            SpawnCleanupPolicy::Keep => "keep",
            SpawnCleanupPolicy::Delete => "delete",
        }
    }

    /// Parse from a string (case-insensitive). Inverse of [`as_str`].
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "keep" => Some(SpawnCleanupPolicy::Keep),
            "delete" => Some(SpawnCleanupPolicy::Delete),
            _ => None,
        }
    }

    /// Whether the spawn overlay should persist across subagent
    /// completion. `Keep` overlays stay queryable for follow-up
    /// turns; `Delete` overlays are dropped as soon as the subagent
    /// returns.
    #[must_use]
    pub const fn should_persist(&self) -> bool {
        matches!(self, SpawnCleanupPolicy::Keep)
    }
}

impl fmt::Display for SpawnCleanupPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_type_as_str() {
        assert_eq!(ChannelType::Cli.as_str(), "cli");
        assert_eq!(ChannelType::Discord.as_str(), "discord");
        assert_eq!(ChannelType::Telegram.as_str(), "telegram");
    }

    #[test]
    fn test_channel_type_from_str() {
        assert_eq!(ChannelType::from_str("cli"), Some(ChannelType::Cli));
        assert_eq!(ChannelType::from_str("CLI"), Some(ChannelType::Cli));
        assert_eq!(ChannelType::from_str("discord"), Some(ChannelType::Discord));
        assert_eq!(ChannelType::from_str("unknown"), None);
    }

    #[test]
    fn test_channel_type_capabilities() {
        assert!(!ChannelType::Cli.supports_rich_formatting());
        assert!(ChannelType::Discord.supports_rich_formatting());

        assert!(!ChannelType::Cli.supports_threads());
        assert!(ChannelType::Discord.supports_threads());
    }

    #[test]
    fn test_channel_type_display() {
        assert_eq!(format!("{}", ChannelType::Discord), "discord");
    }

    #[test]
    fn test_overlay_type() {
        let ct = OverlayType::Channel(ChannelType::Discord);
        assert!(ct.is_channel());
        assert!(!ct.is_spawn());
        assert_eq!(ct.channel_type(), Some(ChannelType::Discord));
        assert_eq!(ct.as_str(), "channel");

        let spawn = OverlayType::Spawn;
        assert!(!spawn.is_channel());
        assert!(spawn.is_spawn());
        assert_eq!(spawn.channel_type(), None);
        assert_eq!(spawn.as_str(), "spawn");
    }

    #[test]
    fn test_spawn_cleanup_policy() {
        assert_eq!(SpawnCleanupPolicy::Keep.as_str(), "keep");
        assert_eq!(SpawnCleanupPolicy::Delete.as_str(), "delete");

        assert_eq!(
            SpawnCleanupPolicy::from_str("keep"),
            Some(SpawnCleanupPolicy::Keep)
        );
        assert_eq!(
            SpawnCleanupPolicy::from_str("DELETE"),
            Some(SpawnCleanupPolicy::Delete)
        );
        assert_eq!(SpawnCleanupPolicy::from_str("unknown"), None);

        assert!(SpawnCleanupPolicy::Keep.should_persist());
        assert!(!SpawnCleanupPolicy::Delete.should_persist());

        // Test default
        let default: SpawnCleanupPolicy = Default::default();
        assert_eq!(default, SpawnCleanupPolicy::Keep);
    }
}
