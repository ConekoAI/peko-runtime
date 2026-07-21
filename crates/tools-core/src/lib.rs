//! Tool execution API — the canonical home for `Tool` plus its
//! abort/cancellation, context, progress, and result primitives.
//!
//! Every extension built-in or external implements the [`Tool`] trait
//! defined here. Tool wiring (registration, capability gate, hook
//! dispatch) lives in `peko-extension-host`, not in this crate, so
//! `peko-tools-core` stays a domain types layer with no inbound
//! dependency on the framework host or any concrete extension
//! implementation.
//!
//! ## Module map
//!
//! - [`traits::Tool`] — the trait every tool implements.
//! - [`exec::ToolContext`], [`exec::AbortSignal`],
//!   [`exec::ToolProgressEvent`] — execution context, abort mechanism,
//!   and progress reporting.
//! - [`exec::ToolResult`], [`exec::ToolError`] — typed result / error.
//! - [`exec::ToolWithContext`], [`exec::ToolContextAdapter`] — adapter
//!   that bridges a raw `Tool` into the context-aware framework.
//! - [`interrupt::ToolInterruptNotice`] — structured cancel notice.
//! - [`context_source::ContextSource`] — unified context resolver.
//! - [`ToolExposure`] — F34 4-axis model for how a tool is exposed to
//!   the LLM (prompt section, native catalog, deferred via
//!   `__tool_search`, hidden).

pub mod context_source;
pub mod exec;
pub mod interrupt;
pub mod traits;

pub use context_source::{ContextResolver, ContextSource};
pub use exec::{
    bridge_from_cancellation_token, bridge_to_cancellation_token, AbortSignal,
    AbortSignalBridgeGuard, CancellationTokenBridgeGuard, ToolContext, ToolContextAdapter,
    ToolError, ToolProgressEvent, ToolResult, ToolWithContext,
};
pub use interrupt::ToolInterruptNotice;
pub use traits::Tool;

/// How a tool is exposed to the LLM (F34, audit section 3 row 4).
///
/// This enum moved into `peko-tools-core` from
/// `extensions::framework::types` so that the canonical home is the
/// tool API crate (where `Tool::exposure()` lives). The extensions
/// crate keeps a `pub use peko_tools_core::ToolExposure;`
/// re-export for backwards compatibility.
///
/// Pre-F34 peko had a binary on/off: a tool was either visible-and-callable
/// or gated by capability. F34 adds a 4-axis model so a tool author can
/// express intent without forcing the LLM (or the prompt section) into a
/// single binary choice.
///
/// Variants:
/// - [`ToolExposure::Direct`] — visible in both the prompt "Available
///   Tools" section AND the native LLM catalog. Callable by the model.
///   This is the default for every existing tool.
/// - [`ToolExposure::DirectModelOnly`] — visible in the native LLM
///   catalog (so the model can still see name + JSON Schema and call it)
///   but suppressed from the prose "Available Tools" prompt section.
///   Useful for tools whose schema is self-documenting (the model
///   doesn't need prose) or that would waste prompt tokens if duplicated.
/// - [`ToolExposure::Deferred`] — invisible to the model in the prompt
///   section and omitted from the initial native catalog. Discoverable
///   through the synthetic `__tool_search` stub (F35) which returns the
///   tool's full `ToolDefinition` so the model can call it by name on
///   the next iteration. Useful for tools that bloat the catalog when
///   the agent doesn't need them but might ask for one.
/// - [`ToolExposure::Hidden`] — invisible to the model in BOTH surfaces.
///   Still callable programmatically (e.g., from another tool's
///   `execute`) via the framework's internal `execute_from_hook` path,
///   but the model never sees or invokes it directly. Useful for
///   telemetry-only, audit-only, or sub-tool-of-other-tool entries.
///
/// The capability gate still applies on top of exposure — a
/// `DirectModelOnly` tool without the principal's `tool:<name>` grant
/// is still hidden from both surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolExposure {
    /// Visible in prompt section AND native catalog; callable. Default.
    #[default]
    Direct,
    /// Suppressed from prompt section; visible in native catalog; callable.
    DirectModelOnly,
    /// Hidden until `__tool_search` resolves it (F35). Discovered by query;
    /// not in the initial catalog.
    Deferred,
    /// Hidden from both surfaces; only callable programmatically.
    Hidden,
}

impl ToolExposure {
    /// True if the tool should appear in the prose "Available Tools"
    /// prompt section. `Direct` only.
    #[must_use]
    pub fn visible_in_prompt_section(self) -> bool {
        matches!(self, ToolExposure::Direct)
    }

    /// True if the tool should appear in the native LLM catalog
    /// (`list_tool_definitions_with_allowlist` output).
    /// `Direct` and `DirectModelOnly` qualify. `Deferred` and `Hidden`
    /// do NOT — `Deferred` is resolvable on demand via `__tool_search`
    /// (F35) and `Hidden` must stay invisible to the model.
    #[must_use]
    pub fn visible_in_native_catalog(self) -> bool {
        matches!(self, ToolExposure::Direct | ToolExposure::DirectModelOnly)
    }
}

use serde::{Deserialize, Serialize};
