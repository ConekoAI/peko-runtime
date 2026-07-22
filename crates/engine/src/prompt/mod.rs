//! Per-turn system prompt rendering — Phase 9b.N.5b.4 lift.
//!
//! [`PromptRenderer`] is the single source of truth for the system
//! prompt. It is invoked by `AgenticLoop::run_inner` at the top of
//! every iteration, fed a [`TurnPromptContext`], and returns the
//! freshly rendered Markdown body that becomes `messages[0]`.
//!
//! ## Module layout
//!
//! - [`renderer`] — `PromptRenderer` (the production hook-driven
//!   renderer; holds `Arc<dyn ToolFunnel>`).
//! - [`context`] — `TurnPromptContext` (the typed input the renderer
//!   reads) + `IterationBudgetState` / `QuotaStateView` control-surface
//!   types + re-exports of `CapabilityChange*` / `CapabilityDiffTracker`
//!   from `crate::iteration_state`.
//! - [`placeholder`] — `Placeholder` enum + `replace_placeholders`
//!   template substitution (the engine that combines hook outputs with
//!   the agent's Markdown body).
//! - [`builder`] — `SystemPromptBuilder` (test-only static renderer;
//!   no hook dispatch).
//! - [`memory`] — per-principal long-term memory (`MEMORY.md`) +
//!   shared directory context (`AGENTS.md`) helpers.

pub mod builder;
pub mod context;
pub mod memory;
pub mod placeholder;
pub mod renderer;
