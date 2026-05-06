//! Session management module
//!
//! Provides session storage with Pekobot JSONL format:
//! - File locking for concurrent access safety
//! - Unified session index (sessions.json + peers.json) for fast lookups
//! - Session key derivation for multi-user isolation
//! - Session overlays (base + channel/spawn layers)
//! - Atomic writes (tmp + rename) for durability
//!
//! # Module Structure
//!
//! - `directory`: Explicit session directory management (no side effects)
//! - `events`: Pekobot session event types (13 types per DATA_MODEL §5.3)
//! - `lock`: File locking with timeout and stale detection
//! - `index`: Unified session index (sessions.json + peers.json) management
//! - `key`: Session key derivation for scoping
//! - `jsonl`: JSONL storage format (Pekobot format)
//! - `types`: Core types (Peer, ChannelType, OverlayType)
//! - `overlay`: Session overlay trait and ChannelOverlay
//! - `spawn`: Spawn overlay for subagent isolation
//! - `base`: Base session (shared conversation context)
//! - `manager`: SessionManager for overlay lifecycle

pub mod context;
pub mod directory;
pub mod events;
pub mod message_conversion;
mod index;
pub mod jsonl;
pub mod key;
pub mod lock;
pub mod lock_utils;
pub mod manager;
pub mod message;
pub mod metadata;
pub mod metadata_controller;
pub mod overlay;
pub mod recovery;
pub mod spawn;
pub mod subagent_key;
pub mod sync;
pub mod types;
pub mod maintenance;
pub mod presentation;
pub mod unified;

// Re-export Session (replaces both BaseSession and SimpleSession)
pub use context::SessionContext;
pub use events::{
    generate_event_id, generate_message_id, generate_tool_call_id, A2aMessageType,
    A2aReceivedEvent, A2aSentEvent, EventEnvelope, HookTriggerEvent, HookType, MessageSource,
    SessionCreatedEvent, SessionEndReason, SessionEndedEvent, SessionEvent, SessionTrigger,
    SpawnRequestEvent, SpawnResultEvent, SystemEvent, ThinkingEvent, TokenUsage, ToolCallBlock,
    ToolCallEvent, ToolResultEvent,
};
pub use index::{MaintenanceConfig, MaintenanceReport, PeerIndex, PeerInfo, SessionEntry};
pub use jsonl::{NormalizedEntry, SessionStorage};
pub use key::{
    base_key_from_overlay, derive_base_session_key, derive_overlay_key, derive_session_key,
    parse_session_key, parse_session_key_v2, ChatType, ParsedSessionKeyV2, SessionScope,
};
pub use lock::FileLock;
pub use lock_utils::{
    try_read_lock, try_read_lock_default, try_write_lock, try_write_lock_default, LockError,
    DEFAULT_READ_TIMEOUT, DEFAULT_WRITE_TIMEOUT,
};
pub use message::{RoleMetadata, SessionMessage};
pub use metadata::{MetadataDiscrepancy, ReconciliationResult, SessionMetadata};
pub use metadata_controller::{ConsistencyStatus, MetadataController};
pub use unified::Session;

// Re-export overlay architecture types
pub use types::{ChannelType, OverlayType, Peer, SpawnCleanupPolicy};

pub use overlay::{ChannelContext, ChannelOverlay, ChannelOverlayData, SessionOverlay};

pub use spawn::{SpawnOverlay, SpawnOverlayData, SpawnResult, SpawnStatus};

pub use manager::{
    OverlayRef, ResolutionStrategy, ResolvedSession, SessionCreateOptions,
    SessionHandle, SessionManager,
};

// Re-export recovery
pub use directory::SessionDirectory;
pub use maintenance::{maintain_agent, MaintenanceScheduler};
pub use recovery::{RecoveryReport, RecoveryState, SessionRecovery};
pub use sync::SyncSessionStorage;

// Re-export subagent key utilities
pub use subagent_key::{
    extract_agent_name, extract_subagent_uuid, format_display_key, generate_subagent_key,
    get_key_depth, is_subagent_key, parse_subagent_key,
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
