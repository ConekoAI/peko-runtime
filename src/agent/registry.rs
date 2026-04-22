//! Local Registry — DELETED
//!
//! `LocalRegistry`, `AgentMetadata`, `CapabilityRecord`, `RegistryEvent`, and the
//! `Registry` trait were deleted in 2026-04-22 as dead code — nothing in the
//! codebase instantiated or used them.
//!
//! The active capability indexing is now in `crate::agent::context::CapabilityIndex`.
//! If a full agent registry is needed in the future, build it on
//! `crate::common::registry::SimpleRegistry` or `SharedRegistry`.

// Module kept as a placeholder to avoid breaking external importers.
// Remove this file and its mod.rs entry once confident no external code references it.
