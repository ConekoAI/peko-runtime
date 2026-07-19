//! System prompt generation and management
//!
//! The system prompt is reconstructed fresh from current principal state
//! every turn. Production rendering lives in
//! [`renderer::PromptRenderer::render_for_iteration`], which is fed a
//! [`context::TurnPromptContext`] carrying the principal, session,
//! iteration, and control-surface state. There is no `SystemPromptService`
//! any more — the renderer is the single source of truth.
//!
//! [`builder::SystemPromptBuilder`] survives as a **test-only** static
//! renderer (no hook dispatch) so the placeholder-replacement path can
//! be unit-tested without standing up an [`ExtensionCore`].
//!
//! [`ExtensionCore`]: crate::extensions::framework::ExtensionCore

pub mod builder;
pub mod context;
pub mod memory;
pub mod placeholder;
pub mod renderer;

pub use builder::{PromptMode, SystemPromptBuilder};
pub use context::{
    CapabilityChange, CapabilityChangeKind, CapabilityDiff, CapabilityDiffTracker,
    IterationBudgetState, QuotaStateView, TurnPromptContext,
};
pub use memory::{
    directory_from_tool_params, discover_shared_context, load_principal_memory,
    PRINCIPAL_MEMORY_FILE, SHARED_CONTEXT_FILE,
};
pub use renderer::PromptRenderer;
