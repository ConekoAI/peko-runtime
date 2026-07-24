//! `peko-principal` — Principal domain DTOs + memory + peer registry + factory contracts.
//!
//! Phase 14.c.1 of the post-migration cleanup lifts the pure-deps subset of
//! `src/principal/` into this workspace member. The runtime-coupled subset
//! (`manager.rs`, `context.rs`, `extension_store.rs`, `agent_runner.rs`,
//! `capability_evaluator.rs`, `routers/*`, `slash/*`) stays in root for
//! Phase 14.c.2 because it depends on `ExtensionCore`, `PathResolver`,
//! `OutputFormat`, and the IPC `ExtensionSummary` wire shape.
//!
//! ## Surface (4 lifted files, ~1,296 lines)
//!
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
//! ## Why a leaf crate
//!
//! All lifted files depend only on `peko-extension-api`, `peko-subject`,
//! `peko-quota`, and `peko-providers`. No inbound deps from root's
//! runtime-coupled principal files (manager, context, etc.) — the
//! reverse is true: root consumes `peko_principal::*` to build those
//! higher layers in 14.c.2.
//!
//! ## What stayed in root and why
//!
//! - `src/principal/factory.rs` — `DefaultPrincipalRouterFactory` creates
//!   root's `RootRouter` (defined in `src/principal/routers/root.rs`),
//!   which stays in root for Phase 14.c.2. The trait contract
//!   `PrincipalRouterFactory` could lift in 14.c.2 once the concrete
//!   `RootRouter` does, but the impl depends on a root-defined type
//!   and cannot move before that.
//!
//! ## Phase 14.c.2 preview
//!
//! - `src/principal/manager.rs` — depends on `PathResolver`,
//!   `OutputFormat`, `AgentAdapter`, `ExtensionStore`. Needs
//!   `PrincipalManagerHost` port trait for the first two + a
//!   re-typed `ExtensionStore` access for the latter.
//! - `src/principal/context.rs` — depends on `ExtensionCore` global,
//!   `PathResolver`. Needs `PrincipalContextHost` port trait.
//! - `src/principal/extension_store.rs`, `agent_runner.rs`,
//!   `capability_evaluator.rs`, `routers/*`, `slash/*` — follow the
//!   same port-trait shape.

pub mod agent_prompt;
pub mod config;
pub mod memory;
pub mod peer;

// Re-exports that root's `src/principal/mod.rs` previously surfaced
// at `crate::principal::*`. After 14.c.1 these come from
// `peko_principal::*` directly; the root mod.rs still re-exports
// them where convenient, but the canonical path is here.
pub use agent_prompt::{load_agent_prompt, AgentPrompt, AgentPromptFrontmatter};
pub use config::{
    AuditLevel, ConsolidationConfig, DelegationGrant, MemoryTier, PrincipalConfig,
    PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
    PrincipalMemoryConfig, PrincipalRoutingConfig, TtlPolicy,
};
pub use memory::{MemoryError, PrincipalMemory, SessionArtifact};
pub use peer::{Peer, PeerConfig, PeerError, PeerRegistry};
pub use peko_quota::QuotaMeter;