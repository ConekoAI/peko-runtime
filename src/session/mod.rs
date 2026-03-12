//! Session management module
//!
//! Provides session storage with OpenClaw-compatible JSONL format:
//! - File locking for concurrent access safety
//! - Session index (sessions.json) for fast lookups
//! - Session key derivation for multi-user isolation
//! - Session overlays (base + channel/spawn layers)
//!
//! # Module Structure
//!
//! - `lock`: File locking with timeout and stale detection
//! - `index`: Session index (sessions.json) management
//! - `key`: Session key derivation for scoping
//! - `jsonl`: JSONL storage format (OpenClaw compatible)
//! - `types`: Core types (Peer, ChannelType, OverlayType)
//! - `overlay`: Session overlay trait and ChannelOverlay
//! - `spawn`: Spawn overlay for subagent isolation
//! - `base`: Base session (shared conversation context)
//! - `manager`: SessionManager for overlay lifecycle

pub mod base;
pub mod context;
pub mod index;
pub mod jsonl;
pub mod key;
pub mod lock;
pub mod manager;
pub mod overlay;
pub mod registry;
pub mod spawn;
pub mod subagent_key;
pub mod types;

// Re-export commonly used types from existing modules
pub use base::BaseSession;
pub use context::{SessionContext, SessionRouter};
pub use index::{IndexEntry, MaintenanceConfig, MaintenanceMode, MaintenanceReport, SessionIndex};
pub use jsonl::{SessionEntry, SessionStorage};
pub use key::{
    base_key_from_overlay, derive_base_session_key, derive_overlay_key, derive_session_key,
    parse_session_key, parse_session_key_v2, ChatType, ParsedSessionKeyV2, SessionScope,
};
pub use lock::FileLock;

// Re-export overlay architecture types
pub use types::{ChannelType, OverlayType, Peer, SpawnCleanupPolicy};

pub use overlay::{ChannelContext, ChannelOverlay, ChannelOverlayData, SessionOverlay};

pub use spawn::{SpawnOverlay, SpawnOverlayData, SpawnResult, SpawnStatus};

pub use manager::{HybridSession, OverlayRef, SessionManager};

// Re-export session registry for switching/branching
pub use registry::{PeerRegistryEntry, SessionInfo, SessionRegistry, SessionRegistryManager};

// Re-export subagent key utilities
pub use subagent_key::{
    extract_agent_name, extract_subagent_uuid, format_display_key, generate_subagent_key,
    generate_subagent_key_with_parent, get_key_depth, is_subagent_key, parse_hybrid_subagent_key,
    parse_subagent_key,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_re_export() {
        let peer = Peer::User("test".to_string());
        assert_eq!(peer.id(), "test");
    }

    #[test]
    fn test_channel_type_re_export() {
        assert_eq!(ChannelType::Discord.as_str(), "discord");
    }

    #[test]
    fn test_overlay_type_re_export() {
        assert!(OverlayType::Spawn.is_spawn());
    }

    #[test]
    fn test_spawn_cleanup_policy_re_export() {
        assert_eq!(SpawnCleanupPolicy::Keep.as_str(), "keep");
    }

    #[test]
    fn test_derive_base_session_key_re_export() {
        let peer = Peer::User("alice".to_string());
        let key = derive_base_session_key("test", &peer);
        assert!(key.contains("peer:user:alice"));
    }
}
