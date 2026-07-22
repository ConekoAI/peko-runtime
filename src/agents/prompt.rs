//! System prompt generation and management — **re-export shim**.
//!
//! Production rendering lives in
//! [`PromptRenderer::render_for_iteration`], which is fed a
//! [`TurnPromptContext`] carrying the principal, session, iteration, and
//! control-surface state. There is no `SystemPromptService` any more —
//! the renderer is the single source of truth.
//!
//! [`SystemPromptBuilder`] survives as a **test-only** static renderer
//! (no hook dispatch) so the placeholder-replacement path can be
//! unit-tested without standing up an extension-core implementation of
//! the [`ToolFunnel`] trait port.
//!
//! [`PromptRenderer::render_for_iteration`]: peko_engine::PromptRenderer::render_for_iteration
//! [`SystemPromptBuilder`]: peko_engine::SystemPromptBuilder
//! [`ToolFunnel`]: peko_extension_host::ToolFunnel
//!
//! ## Phase 9b.N.5b.4 shim
//!
//! The real definitions live in `peko_engine::prompt::*` since
//! Phase 9b.N.5b.4 lifted the entire `src/agents/prompt/` directory
//! into `peko-engine`. This module is now a one-line re-export layer
//! so existing `crate::agents::prompt::*` import paths continue to
//! compile unchanged (per the per-phase backward-compat protocol).
//! Future phases may collapse these shims to nothing once all callers
//! migrate to `peko_engine::prompt::*` directly.

pub use peko_engine::prompt::{
    builder::{PromptMode, SystemPromptBuilder},
    context::{
        CapabilityChange, CapabilityChangeKind, CapabilityDiff, CapabilityDiffTracker,
        IterationBudgetState, QuotaStateView, TurnPromptContext,
    },
    memory::{
        directory_from_tool_params, discover_shared_context, load_principal_memory,
        PRINCIPAL_MEMORY_FILE, SHARED_CONTEXT_FILE,
    },
    placeholder::{replace_placeholders, Placeholder},
    renderer::PromptRenderer,
};

// Submodule re-exports preserve legacy path compatibility for callers
// that import `crate::agents::prompt::context::*`,
// `crate::agents::prompt::memory::*`, etc. Phase 9b.N.5b.4 moved the
// underlying modules into `peko_engine::prompt::*` but did NOT delete
// the legacy module path — the shim mirrors the old `src/agents/prompt/`
// directory layout so existing test sites + inline `use` statements
// continue to compile. (Each `pub use peko_engine::prompt::X as X` line
// below exposes the whole submodule under the same name; the inner
// `pub use` at the top covers the flat-import path.)
pub use peko_engine::prompt::builder;
pub use peko_engine::prompt::context;
pub use peko_engine::prompt::memory;
pub use peko_engine::prompt::placeholder;
pub use peko_engine::prompt::renderer;
