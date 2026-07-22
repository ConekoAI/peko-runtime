//! `AgentView` — the engine-facing surface of root's `Agent`.
//!
//! Phase 9b.N.5a introduced this trait port to break `agentic_loop.rs`'s
//! direct borrow of `Arc<crate::agents::Agent>`. The trait exposes ONLY
//! the methods/fields the loop actually reads — the full `Agent` type is
//! root-only (2524 lines + 16 root-only imports) and lifting it would
//! drag `BuiltinToolAdapter`, `KeyStorage`, `Identity`, `LlmResolver`,
//! `Subject`, etc. into the engine crate, all of which depend on root-only
//! types.
//!
//! This follows the same transient-scaffolding pattern as
//! [[workspace-phase9b-n4-compaction]]'s `CompactorBackend` trait. When
//! `Agent` itself eventually lifts into a `peko-agent` crate (deferred),
//! this trait disappears and the loop holds a direct `Arc<Agent>` again.
//!
//! # Why these methods
//!
//! Each method has a single real consumer today: the
//! `src/engine/agentic_loop.rs` field access / method call. Two are
//! field access (not method) sites, hence the `config_prompt_body()` and
//! `config_enable_tool_search()` accessors — the engine must not reach
//! into `agent.config.prompt` directly. `identity_did()` mirrors the
//! `self.agent.identity.did` direct field access at line 812.
//!
//! `has_llm_resolver()` collapses `Agent::llm_resolver()` to a bool
//! because the loop only does `Some(_) => ...` / `None => ...` — the
//! resolved resolver itself comes from the `Arc<Provider>` field on the
//! loop, not from the agent.
//!
//! Following [[prefer-concrete-over-speculative-abstraction]]: trait
//! stays narrow until a second consumer appears.

/// Narrow engine-facing view of root's `crate::agents::Agent`.
///
/// Implemented by `crate::agents::Agent` via
/// `src/engine/agent_view_compat.rs` (orphan-rule-friendly — Agent is
/// root-only, so the impl lives in root). The lifted
/// `src/engine/agentic_loop.rs` (Phase 9b.N.5b) holds a
/// `Box<dyn AgentView>` (or generic `A: AgentView`) instead of
/// `Arc<crate::agents::Agent>`.
pub trait AgentView: Send + Sync + 'static {
    /// Agent display name (for prompts + hook payloads).
    fn name(&self) -> &str;

    /// Agent DID — used as the session-key namespace on the shared
    /// `ExtensionCore` (issue #68) so concurrent agents don't clobber
    /// each other.
    fn identity_did(&self) -> &str;

    /// Whether the agent has a resolvable LLM provider configured.
    ///
    /// `agentic_loop.rs:845` does `match self.agent.llm_resolver() {
    /// Some(_) => ..., None => ... }` — the resolver itself isn't
    /// consumed, only its presence. We expose the bool directly so the
    /// trait doesn't need to depend on `crate::providers::LlmResolver`
    /// (root-only).
    fn has_llm_resolver(&self) -> bool;

    /// Resolved principal display name (None ⇒ system principal).
    fn principal_name(&self) -> Option<&str>;

    /// Per-principal capability snapshot (None ⇒ unscope).
    fn principal_capabilities(&self) -> Option<&peko_extension_api::Capabilities>;

    /// Active extension IDs for the principal (None ⇒ no extensions).
    fn principal_active_extensions(&self) -> Option<&peko_extension_api::ActiveExtensionSet>;

    /// Channel type (e.g. `"discord"`, `"cli"`). Defaults to `"cli"` when unset.
    fn channel(&self) -> Option<&str>;

    /// Thinking effort level (`"low" | "medium" | "high"`). Defaults
    /// to `"medium"` when unset.
    fn thinking_level(&self) -> Option<&str>;

    /// Whether the agent runs in sandboxed mode.
    fn sandbox_enabled(&self) -> bool;

    /// Resolved model aliases for placeholder substitution
    /// (`{{model_aliases}}`).
    fn model_aliases(&self) -> &[String];

    /// Whether to enable F35's `__tool_search` synthetic built-in.
    /// Field access at `agentic_loop.rs:1892` —
    /// `self.agent.config.enable_tool_search`.
    fn config_enable_tool_search(&self) -> bool;

    /// Agent prompt body template (Markdown with `{{placeholder}}` tokens).
    /// Field access at `agentic_loop.rs:1934` —
    /// `self.agent.config.prompt`. Read fresh each iteration; the
    /// `loop_renders_fresh_prompt_body_each_iteration` test pins this.
    fn config_prompt_body(&self) -> Option<String>;
}
