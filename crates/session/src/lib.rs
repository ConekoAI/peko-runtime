//! `peko-session` — Peko session persistence (Phase 7).
//!
//! Phase 7.1 lands the scaffold + the compaction data types / trait
//! ports. Phase 7.2+ will move the remaining session modules from
//! root's `src/session/` into this crate.
//!
//! # Crate boundary
//!
//! - Allowed deps: `peko-message`, `peko-subject`, `peko-events`,
//!   `peko-quota`, `peko-extension-api`, `peko-provider-api`,
//!   `peko-tools-core`, `peko-providers`, `peko-fs-persistence`.
//! - Forbidden deps: `peko-engine`, `peko-agents`, `peko-extension-host`,
//!   root.
//!
//! # Compaction split
//!
//! - **Persistence** (this crate) — data types + `CompactorBackend`
//!   trait port + `BackgroundCompactorFactory` + eviction helper.
//!   Phase 7.2+ adds the `BackgroundCompactor` mpsc worker, the
//!   `Compactor` LLM summarization helper, `summary_format`,
//!   `turn_boundaries`, `cli`.
//! - **Orchestration** (`peko-engine::compaction_orchestrator`) —
//!   `CompactionOrchestrator` holds a `Box<dyn CompactorBackend>`
//!   supplied by the daemon.
//!
//! `peko-engine` re-exports the data types + trait port + eviction
//! helper from this crate so pre-Phase-7 import paths keep compiling.

// Convenience re-exports for test modules that do `use crate::*;`.
// Production callers should prefer the canonical paths
// (`peko_session::events::SessionEvent`, etc.).
pub use peko_message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};
pub use peko_subject::Subject;
pub use serde_json::{json, Value};
pub use std::sync::Arc;
pub use tokio::sync::RwLock;

pub use message_conversion::{entries_to_context_text, event_to_llm_message};

pub mod compaction;
// PathResolverLike now lives in peko-subject; re-export below.
pub mod types;

pub use peko_subject::PathResolverLike;
pub use types::{ChannelType, OverlayType, SpawnCleanupPolicy};

// Phase 7.4 lifted the rest of `src/session/` (events.rs, jsonl.rs,
// message.rs, message_conversion.rs, metadata.rs,
// metadata_controller.rs, key.rs, subagent_key.rs, index.rs,
// directory.rs, recovery.rs, sync.rs, overlay.rs, spawn.rs,
// todos.rs, todo_runtime_impl.rs, session_runtime_impl.rs,
// context.rs, test_config.rs, types.rs, unified.rs, manager.rs,
// inbox_registry.rs, lock_utils.rs, maintenance.rs) into this crate.
// Re-exports below preserve the historical `peko_session::X` paths.
pub mod context;
pub mod default_path_resolver;
pub use default_path_resolver::DefaultPathResolver;
pub mod directory;
pub mod events;
pub mod inbox_registry;
pub mod index;
pub mod jsonl;
pub mod key;
pub mod lock_utils;
pub mod maintenance;
pub mod manager;
pub mod message;
pub mod message_conversion;
pub mod metadata;
pub mod metadata_controller;
pub mod overlay;
pub mod recovery;
pub mod session_core;
pub mod session_core_impl;
pub mod session_info;
pub mod spawn;
pub mod subagent_key;
pub mod sync;
pub mod test_config;
pub mod todos;
pub mod unified;

pub use context::SessionContext;
pub use directory::SessionDirectory;
pub use events::{
    generate_event_id, generate_message_id, EventEnvelope, MessageSource, RoleMetadata,
    SessionCreatedEvent, SessionEndedEvent, SessionEvent, SessionMessage, SessionTrigger,
    SystemEvent,
};
pub use inbox_registry::{InboxFactory, InboxRegistry, RunPermitGuard};
pub use index::{SessionEntry, SessionIndex};
pub use jsonl::{NormalizedEntry, SessionStorage};
pub use key::{
    base_key_from_overlay, derive_base_session_key, derive_overlay_key, derive_session_key,
    discord_session_key, parse_session_key, parse_session_key_v2, safe_filename_component,
    sanitize_key_component, scope_from_key, ChatType, ParsedSessionKeyV2, SessionKeyContext,
    SessionKeyParts, SessionScope,
};
pub use lock_utils::into_anyhow;
pub use lock_utils::{
    try_read_lock, try_read_lock_default, try_write_lock, try_write_lock_default, LockError,
    DEFAULT_READ_TIMEOUT, DEFAULT_WRITE_TIMEOUT,
};
pub use maintenance::MaintenanceScheduler;
pub use manager::{OverlayRef, SessionCreateOptions, SessionHandle, SessionManager};
pub use metadata::{ReconciliationResult, SessionMetadata};
pub use metadata_controller::MetadataController;
pub use overlay::{ChannelContext, ChannelOverlay, ChannelOverlayData, SessionOverlay};
pub use recovery::{RecoveryReport, SessionRecovery};
pub use session_core::{SessionCore, SessionView};
pub use session_info::{
    BranchResult, HistoryEvent, HistoryQuery, HistoryResult, HistorySummary, SessionDetails,
    SessionInfo,
};
pub use spawn::{SpawnOverlay, SpawnOverlayData, SpawnResult, SpawnStatus};
pub use subagent_key::{
    extract_agent_name, extract_subagent_uuid, format_display_key, generate_subagent_key,
    get_key_depth, get_parent_key, is_subagent_key, parse_subagent_key,
};
pub use sync::SyncSessionStorage;
pub use test_config::{lock_timeout_ms, max_sessions, prune_duration, rotate_bytes};
pub use todos::{Todo, TodoStatus, TodoStorage};
pub use unified::Session;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_cleanup_policy_re_export() {
        assert_eq!(SpawnCleanupPolicy::Keep.as_str(), "keep");
    }

    #[test]
    fn test_channel_type_re_export() {
        assert_eq!(ChannelType::Discord.as_str(), "discord");
    }

    #[test]
    fn test_overlay_type_re_export() {
        assert!(OverlayType::Spawn.is_spawn());
    }
}

// ============================================================================
// ToolCall — Phase 7.4 lifted this struct from `peko-engine::agentic_loop`
// into `peko-session` so the session manager can hold tool-call
// metadata without depending on `peko-engine`. The struct exists purely
// for session-storage compatibility (its fields are a small subset of
// `peko_message::ContentBlock::ToolCall`). peko-engine re-exports this
// type under `peko_engine::ToolCall` for pre-Phase-7.4 callers; the
// engine-local definition is removed in Phase 16.
// ============================================================================

/// A tool call for session storage compatibility.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name.
    pub name: String,
    /// Tool parameters.
    pub parameters: serde_json::Value,
}
