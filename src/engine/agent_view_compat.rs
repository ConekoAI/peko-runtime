//! Compatibility shim: implements `peko_engine::AgentView` for root's
//! `Agent` so the lifted `AgenticLoop` (Phase 9b.N.5b,
//! `crates/engine/src/agentic_loop.rs`) can read agent identity /
//! principal state without holding a direct borrow of root's
//! [`crate::agents::Agent`].
//!
//! # Trait port rationale
//!
//! `AgentView` (defined in `peko_engine::agent_view`) is a narrow 12-
//! method trait port:
//!
//! 1. `name()` / `identity_did()` — `Agent::name()` / DID field access.
//! 2. `has_llm_resolver()` — collapsed bool from `Agent::llm_resolver()`
//!    since the loop only does `Some(_) / None` matching.
//! 3. `principal_*()` — per-principal runtime state.
//! 4. `channel()` / `thinking_level()` / `sandbox_enabled()` /
//!    `model_aliases()` — config-derived scalars for prompt rendering.
//! 5. `config_enable_tool_search()` / `config_prompt_body()` — explicit
//!    accessors for the two `self.agent.config.*` field access sites
//!    in the loop. Reading the prompt body fresh each iteration is
//!    pinned by `loop_renders_fresh_prompt_body_each_iteration`.
//!
//! The impl lives here (not in `peko-engine`) because of the orphan
//! rule: `peko_engine::AgentView` is a foreign trait, and `Agent` is a
//! root-only type. The `impl AgentView for Agent` form is allowed
//! because `Agent` is local to root (see the orphan rule's "local
//! type before any uncovered type parameter" clause).
//!
//! Module location: rooted at `src/engine/agent_view_compat.rs` so
//! `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/extension_core_funnel_compat.rs` (Phase 9b.N.2),
//! `src/engine/session_view_compat.rs` (Phase 9b.N.3),
//! `src/engine/async_completion_compat.rs` (Phase 9b.N.1), and
//! `src/engine/compaction_backend_compat.rs` (Phase 9b.N.4) patterns.
//!
//! # Trait port lifetime
//!
//! The trait port mirrors the pattern established by Phase 9b.N.1
//! (`AsyncCompletionLike`), 9b.N.2 (`ToolFunnel`), 9b.N.3
//! (`SessionView`), and 9b.N.4 (`CompactorBackend`). It disappears
//! when a later phase lifts `Agent` itself into a `peko-agent` crate
//! (deferred — blocked by the `Identity` + `BuiltinToolAdapter` +
//! `KeyStorage` + `Subject` root-only couplings).

use crate::agents::Agent;
use peko_engine::AgentView;

impl AgentView for Agent {
    fn name(&self) -> &str {
        Agent::name(self)
    }

    fn identity_did(&self) -> &str {
        // `Agent::identity` is a private field; the public surface uses
        // `Agent::did()` which returns the same value. We avoid
        // exposing the `Identity` struct through the trait.
        Agent::did(self)
    }

    fn has_llm_resolver(&self) -> bool {
        Agent::llm_resolver(self).is_some()
    }

    fn principal_name(&self) -> Option<&str> {
        Agent::principal_name(self)
    }

    fn principal_capabilities(&self) -> Option<&peko_extension_api::Capabilities> {
        // `Agent::principal_capabilities` returns
        // `Option<&Arc<Capabilities>>` (the Arc lets the agent cache
        // a principal's capability snapshot across iterations); the
        // trait surface exposes `Option<&Capabilities>` so the engine
        // doesn't need to know about the Arc wrapper.
        Agent::principal_capabilities(self).map(|arc| &**arc)
    }

    fn principal_active_extensions(&self) -> Option<&peko_extension_api::ActiveExtensionSet> {
        Agent::principal_active_extensions(self)
    }

    fn channel(&self) -> Option<&str> {
        Agent::channel(self)
    }

    fn thinking_level(&self) -> Option<&str> {
        Agent::thinking_level(self)
    }

    fn sandbox_enabled(&self) -> bool {
        Agent::sandbox_enabled(self)
    }

    fn model_aliases(&self) -> &[String] {
        Agent::model_aliases(self)
    }

    fn config_enable_tool_search(&self) -> bool {
        self.config.enable_tool_search
    }

    fn config_prompt_body(&self) -> Option<String> {
        self.config.prompt.clone()
    }
}
