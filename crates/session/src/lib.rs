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

pub mod compaction;
pub mod types;

pub use types::{ChannelType, OverlayType, SpawnCleanupPolicy};

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
