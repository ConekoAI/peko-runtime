//! Per-turn prompt rendering context and control banners.

use peko_provider_api::ToolDefinition;
use std::path::PathBuf;

/// Iteration counter state for the `{{iteration_budget}}` control surface.
///
/// This is a bare counter, not a budget: the loop has no iteration
/// ceiling and runs until natural completion (the model stops calling
/// tools), abort/interrupt, quota trip, or provider error. The section
/// doubles as the always-on heartbeat line of the runtime-context tail.
#[derive(Debug, Clone, Copy)]
pub struct IterationBudgetState {
    /// Current iteration number (1-indexed; the loop increments at top).
    pub iteration: usize,
}

impl IterationBudgetState {
    /// Render the section body (section header + counter line). Always
    /// rendered when `iteration_budget` is requested, even at iteration 1.
    #[must_use]
    pub fn render(&self) -> String {
        format!("## Iteration budget\nIteration {}.\n", self.iteration)
    }
}

/// Render the `{{quota_tripped}}` banner. Empty string when the trip
/// indicator is `false` (the template's `remove_missing=true` strips
/// the marker); otherwise the short advisory the LLM should see at the
/// moment the principal's quota first trips.
///
/// Mirrors `render_soft_cancel_section` (see `renderer.rs`) — both
/// single-shot banners render to empty unless their flag is set.
#[must_use]
pub fn render_quota_tripped_section() -> String {
    "## Quota tripped\n\
     The principal's quota was just tripped. Wrap up non-essential work,\
     finish the current step cleanly, and do not start a new tool round.\n"
        .to_string()
}

/// The single typed input the renderer reads each iteration.
///
/// Built by [`AgenticLoop::run_inner`](crate::AgenticLoop)
/// at the top of every iteration and consumed exactly once by
/// [`PromptRenderer::render_for_iteration`](super::renderer::PromptRenderer::render_for_iteration).
///
/// Cheap to construct (mostly `Arc` clones). Holds no `&'static` references.
#[derive(Clone)]
pub struct TurnPromptContext {
    /// Principal runtime id (for hook dispatch).
    pub principal_id: String,
    /// Real session id for this run — stamped onto the
    /// `SessionSnapshot` the `SessionContextBuild` hook receives.
    pub session_id: String,
    /// Role name (for `{{role_name}}`; `{{agent_name}}` is a legacy
    /// alias rendered with the same value).
    pub role_name: String,
    /// Agent prompt body template (Markdown with `{{placeholder}}` tokens).
    pub body: String,
    /// Per-principal long-term memory loaded from `<workspace>/MEMORY.md`.
    /// Rendered into the system prompt at the `{{memory}}` placeholder.
    pub principal_memory: Option<String>,
    /// ADR-052 D5 (T2): the nearest `AGENTS.md` discovered above the
    /// run's focus directory, as `(label, content)` where the label is
    /// the file's path. Rendered as the `## Project instructions`
    /// runtime-context tail section (explicitly labeled as
    /// environment-provided, below principal instructions in
    /// authority). `None` when no `AGENTS.md` is in scope — the change
    /// tracker then retracts the section.
    pub project_instructions: Option<(String, String)>,
    /// Workspace path (for `{{workspace}}`).
    pub workspace: PathBuf,
    /// Resolved model id for the LLM call this iteration (for `{{runtime}}`).
    pub resolved_model: String,
    /// Channel that triggered the LLM call (for `{{channel}}`).
    pub channel: String,
    /// Thinking level (for `{{thinking_level}}`).
    pub thinking_level: String,
    /// Sandbox status (for `{{sandbox}}`).
    pub sandbox_enabled: bool,
    /// Configured model aliases (for `{{model_aliases}}`).
    pub model_aliases: Vec<String>,
    /// Whether the daemon has a gateway attached (gates `{{self_update}}`).
    pub has_gateway: bool,

    /// Peer-conversation DM channel id (peer-ingress turns only),
    /// rendered into the `{{session_context}}` section as the channel
    /// that reaches the user. `None` for non-conversation runs.
    pub conversation_channel: Option<String>,
    /// Peer-conversation peer subject in wire form (`user:alice`,
    /// `principal:did:…`; peer-ingress turns only), rendered into the
    /// `{{session_context}}` section. `None` for non-conversation
    /// runs.
    pub conversation_peer: Option<String>,

    // ---- Control surfaces ----
    /// Iteration counter state (`None` ⇒ `{{iteration_budget}}` not rendered).
    pub iteration_budget: Option<IterationBudgetState>,
    /// Quota-tripped rising-edge flag (`false` ⇒ `{{quota_tripped}}` not
    /// rendered). Set by `build_turn_context` on the iteration the
    /// principal's `QuotaMeter` first trips; cleared on subsequent
    /// iterations until quota rolls over and trips again. Single-shot so
    /// the banner doesn't spam the prompt every turn while the run
    /// stays tripped. Full quota state (counters, limits, window) is
    /// available via the `Session` tool's `status` action — see
    /// `QuotaSnapshot`.
    pub quota_tripped: bool,
    /// Soft-cancel pending flag (`false` ⇒ `{{soft_cancel}}` not rendered).
    pub soft_cancel_pending: bool,

    /// Tool definitions resolved by the loop for this iteration. The
    /// renderer does NOT consume this field — tool catalogs travel
    /// exclusively on the wire as the `tools[]` JSON-schema array,
    /// built by the engine's `build_tool_definitions`. Kept on the
    /// typed context so hook handlers can introspect the visible set
    /// if they need to.
    pub tool_definitions: Vec<ToolDefinition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iteration_budget_render_is_bare_counter() {
        let s = IterationBudgetState { iteration: 3 };
        let rendered = s.render();
        assert!(rendered.contains("## Iteration budget"));
        assert!(rendered.contains("Iteration 3."));
        // No ceiling language — the loop has no iteration cap.
        assert!(!rendered.contains(" of "));
        assert!(!rendered.contains("Approaching limit"));
        assert!(!rendered.contains("Stop and finalize"));
    }

    #[test]
    fn quota_tripped_section_renders_banner() {
        // The trip banner is the rising-edge payload the renderer
        // substitutes into `{{quota_tripped}}`. Mirrors the soft-cancel
        // pattern: empty when the flag is false, advisory when true.
        let rendered = render_quota_tripped_section();
        assert!(rendered.contains("## Quota tripped"));
        assert!(rendered.contains("non-essential"));
    }
}
