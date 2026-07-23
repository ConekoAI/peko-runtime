//! Re-export shim. Canonical home is `peko_engine::StackedMeteredProvider`
//! (Phase 9b.N.5b.8).
//!
//! This file used to hold the wrapper directly. It was lifted into
//! `peko-engine` so the agentic loop can call it without taking a
//! `peko-engine → root` dep edge. The wrapper was refactored to wrap
//! `Arc<dyn ProviderView>` instead of `Arc<crate::providers::Provider>`
//! — the surface the loop actually reads. The dropped methods
//! (`chat_response`, `chat_response_with_system`, `chat`,
//! `chat_with_system`, `inner`) were root-only `Provider` methods
//! with one external caller (`src/session/compaction.rs:414`,
//! `BackgroundCompactor::summarize`), which was updated to use
//! `chat_with_tools` with an empty tool list.
pub use peko_engine::StackedMeteredProvider;
