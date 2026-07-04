//! Session management module
//!
//! Provides session storage with Peko JSONL format:
//! - File locking for concurrent access safety
//! - Unified session index (sessions.json + peers.json) for fast lookups
//! - Session key derivation for multi-user isolation
//! - Session overlays (base + channel/spawn layers)
//! - Atomic writes (tmp + rename) for durability
//!
//! # Module Structure
//!
//! - `directory`: Explicit session directory management (no side effects)
//! - `events`: Peko session event types (13 types per DATA_MODEL §5.3)
//! - `lock`: File locking with timeout and stale detection
//! - `index`: Unified session index (sessions.json + peers.json) management
//! - `key`: Session key derivation for scoping
//! - `jsonl`: JSONL storage format (Peko format)
//! - `types`: Core types (Subject, ChannelType, OverlayType)
//! - `overlay`: Session overlay trait and ChannelOverlay
//! - `spawn`: Spawn overlay for subagent isolation
//! - `base`: Base session (shared conversation context)
//! - `manager`: SessionManager for overlay lifecycle

pub mod context;
pub mod directory;
pub mod events;
pub mod inbox_registry;
mod index;
pub mod jsonl;
pub mod key;
pub mod lock;
pub mod lock_utils;
pub mod maintenance;
pub mod manager;
pub mod message;
pub mod message_conversion;
pub mod metadata;
pub mod metadata_controller;
pub mod overlay;
pub mod presentation;
pub mod recovery;
pub mod spawn;
pub mod subagent_key;
pub mod sync;
pub mod todos;
pub mod types;
pub mod unified;

// Context compaction (absorbed from src/compaction/ in issue #31b)
pub mod compaction;

// Re-export Session (replaces both BaseSession and SimpleSession)
pub use context::SessionContext;
pub use events::{
    generate_event_id, generate_message_id, generate_tool_call_id, A2aMessageType,
    A2aReceivedEvent, A2aSentEvent, EventEnvelope, HookTriggerEvent, HookType, MessageSource,
    SessionCreatedEvent, SessionEndReason, SessionEndedEvent, SessionEvent, SessionTrigger,
    SpawnRequestEvent, SpawnResultEvent, SystemEvent, ThinkingEvent, TokenUsage, ToolCallBlock,
    ToolCallEvent, ToolResultEvent,
};
pub use inbox_registry::{InboxRegistry, RunPermitGuard};
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
pub use types::{ChannelType, OverlayType, SpawnCleanupPolicy};

pub use overlay::{ChannelContext, ChannelOverlay, ChannelOverlayData, SessionOverlay};

pub use spawn::{SpawnOverlay, SpawnOverlayData, SpawnResult, SpawnStatus};

pub use manager::{
    OverlayRef, ResolutionStrategy, ResolvedSession, SessionCreateOptions, SessionHandle,
    SessionManager,
};

// Re-export recovery
pub use directory::SessionDirectory;
pub use maintenance::{maintain_agent, MaintenanceScheduler};
pub use recovery::{RecoveryReport, RecoveryState, SessionRecovery};
pub use sync::SyncSessionStorage;
pub use todos::{Todo, TodoStatus, TodoStorage};

// Re-export subagent key utilities
pub use subagent_key::{
    extract_agent_name, extract_subagent_uuid, format_display_key, generate_subagent_key,
    get_key_depth, is_subagent_key, parse_subagent_key,
};

/// Sanitize a string for use as a single filename component on every platform.
///
/// Session ids and todo session keys are canonically written with `:`
/// separators (e.g. `agent:test:cli:default`, `root:User(alice)`).
/// Unix file systems accept those characters, but Windows reserves
/// `< > : " / \ | ? *` (and control chars 0-31) in NTFS filenames, so any
/// code path that derives a filename from a session id crashes at
/// `fs::File::create` time with "filename, directory name, or volume
/// label syntax is incorrect" — surfacing through the lock helper as
/// "Lock acquisition timeout after 10000ms: Failed to create temp lock file".
///
/// This function rewrites only the characters Windows rejects (and only on
/// Windows; POSIX behavior is preserved bit-for-bit so existing on-disk
/// files remain reachable on Linux/macOS). The semantic session id stored
/// in memory and serialized into JSONL is unchanged — only the on-disk
/// filename is transformed.
#[must_use]
pub fn safe_filename_component(s: &str) -> String {
    if !cfg!(windows) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (ch as u32) < 0x20
        {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod safe_filename_tests {
    use super::*;

    #[test]
    fn safe_filename_is_noop_on_current_platform_when_input_clean() {
        // Cross-platform sanity check: a string with no Windows-reserved
        // chars must round-trip unchanged on every target.
        assert_eq!(
            safe_filename_component("clean-session-id"),
            "clean-session-id"
        );
    }

    #[test]
    #[cfg(windows)]
    fn safe_filename_output_is_filesafe_for_known_session_keys() {
        // Apply to the exact session-key forms used in the Windows-failing
        // tests and verify the rewritten form has no NTFS-reserved chars.
        let todo_key = "agent:test:cli:default";
        let root_key = "root:User(alice)";
        for input in [todo_key, root_key] {
            let out = safe_filename_component(input);
            for ch in out.chars() {
                assert!(
                    !matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                        && (ch as u32) >= 0x20,
                    "safe_filename_component({input:?}) produced unsafe char {ch:?}"
                );
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn safe_filename_replaces_colons_on_windows() {
        assert_eq!(
            safe_filename_component("agent:test:cli:default"),
            "agent-test-cli-default"
        );
    }

    #[test]
    #[cfg(windows)]
    fn safe_filename_keeps_parens_on_windows() {
        // `(` and `)` are legal in NTFS; only the truly-reserved chars are
        // rewritten. This guards against an over-broad sanitizer.
        assert_eq!(
            safe_filename_component("root:User(alice)"),
            "root-User(alice)"
        );
    }

    #[test]
    #[cfg(windows)]
    fn safe_filename_replaces_all_reserved_chars_on_windows() {
        assert_eq!(
            safe_filename_component("a<b>c:d\"e/f\\g|h?i*j"),
            "a-b-c-d-e-f-g-h-i-j"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Subject;

    #[test]
    fn test_peer_re_export() {
        let peer = Subject::User("test".to_string());
        assert_eq!(peer.subject_id(), "test");
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
        let peer = Subject::User("alice".to_string());
        let key = derive_base_session_key("test", &peer);
        assert!(key.contains("peer:user:alice"));
    }
}
