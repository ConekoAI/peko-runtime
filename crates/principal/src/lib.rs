//! `peko-principal` — Principal domain DTOs + memory + peer registry + factory contracts.
//!
//! Phase 14.c.1 of the post-migration cleanup lifts the pure-deps subset of
//! `src/principal/` into this workspace member. Phase 14.c.2a adds the
//! runtime-data helpers (`capability_evaluator`, `extension_store`) plus the
//! shared `runtime::OutputFormat` + `runtime::builtin_tools` const lists and
//! the `slash::PrincipalExtensionRow` re-type.
//!
//! ## Surface (8 lifted files + 1 module, ~1,560 lines)
//!
//! 14.c.1 (pure-deps subset):
//! - [`config`] — `PrincipalConfig` + governance/memory/routing/identity
//!   sub-configs; `AuditLevel`, `MemoryTier`, `TtlPolicy`,
//!   `ConsolidationConfig`, `DelegationGrant`, `PrincipalGovernanceConfig`,
//!   `PrincipalIdentityConfig`, `PrincipalIntentConfig`,
//!   `PrincipalMemoryConfig`, `PrincipalRoutingConfig`.
//! - [`peer`] — `Peer`, `PeerConfig`, `PeerError`, `PeerRegistry` (F20 per-peer quota).
//! - [`memory`] — `MemoryError`, `PrincipalMemory`, `SessionArtifact`.
//! - [`agent_prompt`] — `load_agent_prompt`, `AgentPrompt`,
//!   `AgentPromptFrontmatter`.
//!
//! 14.c.2a (pure-data helpers + shared types):
//! - [`capability_evaluator`] — `CapabilityEvaluator` (no state; pure logic).
//! - [`extension_store`] — `ExtensionCatalog`, `ExtensionCatalogItem`,
//!   `capability_kind_for_extension_type`.
//! - [`runtime::OutputFormat`] — human/JSON preference flag (lifted from
//!   `crate::common::types::OutputFormat`).
//! - [`runtime::builtin_tools`] — `GLOBAL_TOOL_NAMES`, `AGENT_SPECIFIC_TOOL_NAMES`,
//!   `all_tool_names()`, `is_builtin_tool()` (lifted from
//!   `crate::extensions::framework::adapters::builtin_tools`).
//! - [`slash::PrincipalExtensionRow`] — subset of the IPC wire
//!   `ExtensionSummary` for the principal slash dispatcher. Full
//!   `ExtensionSummary` stays in root's IPC layer; the daemon maps at the
//!   boundary.
//!
//! ## Why a leaf crate
//!
//! All lifted files depend only on `peko-extension-api`, `peko-subject`,
//! `peko-quota`, `peko-providers`, `peko-auth`, `peko-extension-host`, and
//! `peko-message`. No inbound deps from root's runtime-coupled principal
//! files (manager, context, agent_runner, routers, slash dispatcher impl) —
//! the reverse is true: root consumes `peko_principal::*` to build those
//! higher layers in 14.c.2b.
//!
//! ## What stayed in root and why
//!
//! - `src/principal/factory.rs` — `DefaultPrincipalRouterFactory` creates
//!   root's `RootRouter` (defined in `src/principal/routers/root.rs`),
//!   which stays in root for Phase 14.c.2b. The trait contract
//!   `PrincipalRouterFactory` could lift in 14.c.2b once the concrete
//!   `RootRouter` does, but the impl depends on a root-defined type
//!   and cannot move before that.
//! - `src/principal/manager.rs` + `src/principal/agent_runner.rs` — depend
//!   on root's `Agent` + `SubagentExecutor` machinery; lift with
//!   `peko-agents` extraction (Phase 14.d).
//!
//! ## Phase 14.c.2b preview
//!
//! - `src/principal/{router,routers,slash,context}.rs` — lift alongside the
//!   `Principal` struct with 2 new port traits (`RootAgentRunner` and
//!   `ExtensionCoreProvider`) so root's `agent_runner` and `ExtensionCore`
//!   singleton stay accessible through trait seams.

pub mod agent_prompt;
pub mod capability_evaluator;
pub mod config;
pub mod extension_store;
pub mod memory;
pub mod peer;
pub mod runtime;
pub mod slash;

// Re-exports that root's `src/principal/mod.rs` previously surfaced
// at `crate::principal::*`. After 14.c.1/14.c.2a these come from
// `peko_principal::*` directly; the root mod.rs still imports them
// where convenient, but the canonical path is here.
pub use agent_prompt::{load_agent_prompt, AgentPrompt, AgentPromptFrontmatter};
pub use capability_evaluator::CapabilityEvaluator;
pub use config::{
    AuditLevel, ConsolidationConfig, DelegationGrant, MemoryTier, PrincipalConfig,
    PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
    PrincipalMemoryConfig, PrincipalRoutingConfig, TtlPolicy,
};
pub use extension_store::{
    capability_kind_for_extension_type, ExtensionCatalog, ExtensionCatalogItem,
};
pub use memory::{MemoryError, PrincipalMemory, SessionArtifact};
pub use peer::{Peer, PeerConfig, PeerError, PeerRegistry};
pub use peko_quota::QuotaMeter;
pub use runtime::OutputFormat;
pub use slash::PrincipalExtensionRow;
