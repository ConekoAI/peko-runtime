//! Per-turn prompt context for the system-prompt renderer.
//!
//! `TurnPromptContext` carries the principal, session, iteration, and
//! control-surface state the [`PromptRenderer`](super::renderer::PromptRenderer)
//! consumes on every iteration. It is the single typed input the renderer
//! reads — no environment variables, no hidden state. The renderer rebuilds
//! the prompt fresh from this context every turn; the rebuilt prompt is the
//! only source of truth for `messages[0]`.
//!
//! ## Control surfaces
//!
//! Four long-horizon control surfaces are first-class fields:
//!
//! - [`TurnPromptContext::iteration_budget`] — emitted at `{{iteration_budget}}`
//! - [`TurnPromptContext::quota_tripped`] — emitted at `{{quota_tripped}}` (rising edge only)
//! - [`TurnPromptContext::soft_cancel_pending`] — emitted at `{{soft_cancel}}`
//! - [`TurnPromptContext::capability_diff`] — emitted at `{{capability_diff}}`
//!
//! Each is opt-in: a template that omits the placeholder simply drops the
//! section, because [`replace_placeholders`](super::placeholder::replace_placeholders)
//! with `remove_missing=true` strips unknown tokens.
//!
//! ## Capability diff tracking
//!
//! [`CapabilityDiffTracker`] lives on the [`AgenticLoop`](crate::AgenticLoop)
//! and observes the principal's capability snapshot each iteration. The
//! first render reports all grants as `granted` (baseline); subsequent
//! renders return `None` when nothing changed and a diff when it did.
//!
//! Phase 1 ships the tracker stub and plumbing. Phase 3 wires the four
//! control-surface placeholders to render real bodies from `ctx`.
//!
//! ## Capability diff types re-export
//!
//! `CapabilityChange`, `CapabilityChangeKind`, `CapabilityDiff`, and
//! `CapabilityDiffTracker` are owned by [`peko_engine::iteration_state`]
//! (Phase 9b.N.5a) but re-exported here so existing renderer / test
//! paths that import `crate::prompt::context::Capability*` continue
//! to compile unchanged. The loop itself still lives in root at
//! `src/engine/agentic_loop.rs` (Phase 9b.N.5b.4 has not lifted it);
//! once that happens the re-exports become vestigial.

use peko_extension_api::{ActiveExtensionSet, Capabilities};
use peko_provider_api::ToolDefinition;
use std::path::PathBuf;
use std::sync::Arc;

// Capability diff types live in `crate::iteration_state` (Phase 9b.N.5a).
// Re-export here so renderer + tests keep their existing import paths.
pub use crate::iteration_state::{
    CapabilityChange, CapabilityChangeKind, CapabilityDiff, CapabilityDiffTracker,
};

/// Iteration-budget state for the `{{iteration_budget}}` control surface.
#[derive(Debug, Clone, Copy)]
pub struct IterationBudgetState {
    /// Current iteration number (1-indexed; the loop increments at top).
    pub iteration: usize,
    /// Maximum iterations the loop will run.
    pub max_iterations: usize,
}

impl IterationBudgetState {
    /// Render the section body. Returns `None` when the template does
    /// not need a section this iteration (we always render when
    /// `iteration_budget` is requested, even at iteration 1).
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = vec![
            "## Iteration budget".to_string(),
            format!(
                "Iteration {} of {}. Plan remaining steps accordingly.",
                self.iteration, self.max_iterations
            ),
        ];
        if self.iteration >= self.max_iterations.saturating_sub(2) {
            lines.push("Approaching limit — wrap up.".to_string());
        }
        if self.iteration >= self.max_iterations {
            lines.push("Stop and finalize.".to_string());
        }
        lines.join("\n") + "\n"
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
    /// Agent name (for `{{agent_name}}`).
    pub agent_name: String,
    /// Agent prompt body template (Markdown with `{{placeholder}}` tokens).
    pub body: String,
    /// Per-agent capability snapshot (None ⇒ fail-closed empty set).
    pub capabilities: Option<Arc<Capabilities>>,
    /// Active extension IDs for the principal.
    pub active_extensions: Option<ActiveExtensionSet>,
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
    /// Iteration-budget state (`None` ⇒ `{{iteration_budget}}` not rendered).
    pub iteration_budget: Option<IterationBudgetState>,
    /// Quota-tripped rising-edge flag (`false` ⇒ `{{quota_tripped}}` not
    /// rendered). Set by `build_turn_context` on the iteration the
    /// principal's `QuotaMeter` first trips; cleared on subsequent
    /// iterations until quota rolls over and trips again. Single-shot so
    /// the banner doesn't spam the prompt every turn while the run
    /// stays tripped. Full quota state (counters, limits, window) is
    /// available via the `session` tool's `status` action — see
    /// `QuotaSnapshot`.
    pub quota_tripped: bool,
    /// Soft-cancel pending flag (`false` ⇒ `{{soft_cancel}}` not rendered).
    pub soft_cancel_pending: bool,
    /// Capability diff vs last observation (`None` ⇒ `{{capability_diff}}` not rendered).
    pub capability_diff: Option<CapabilityDiff>,

    /// Tool definitions resolved by the loop for this iteration. The
    /// renderer does NOT consume this field — tool catalogs travel
    /// exclusively on the wire as the `tools[]` JSON-schema array,
    /// built by the engine's `build_tool_definitions`. Kept on the
    /// typed context so hook handlers can introspect the visible set
    /// if they need to.
    pub tool_definitions: Vec<ToolDefinition>,
}

impl TurnPromptContext {
    /// Borrow the principal's capability grant strings (empty when unset).
    #[must_use]
    pub fn capability_strings(&self) -> Vec<String> {
        self.capabilities
            .as_ref()
            .map(|c| c.to_strings())
            .unwrap_or_default()
    }

    /// Borrow the active extension ID list.
    #[must_use]
    pub fn active_extension_vec(&self) -> Vec<String> {
        self.active_extensions
            .as_ref()
            .map(|a| a.to_vec())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_diff_first_observe_is_none() {
        let mut tracker = CapabilityDiffTracker::new();
        let caps = Capabilities::with_grants(["tool:Read"]);
        assert_eq!(tracker.observe(&caps), None);
    }

    #[test]
    fn capability_diff_unchanged_returns_none() {
        let mut tracker = CapabilityDiffTracker::new();
        let caps = Capabilities::with_grants(["tool:Read", "tool:Write"]);
        tracker.observe(&caps);
        assert_eq!(tracker.observe(&caps), None);
    }

    #[test]
    fn capability_diff_detects_grant() {
        let mut tracker = CapabilityDiffTracker::new();
        tracker.observe(&Capabilities::with_grants(["tool:Read"]));
        let diff = tracker
            .observe(&Capabilities::with_grants(["tool:Read", "tool:Write"]))
            .expect("grant should produce a diff");
        assert_eq!(diff.revoked.len(), 0);
        assert_eq!(diff.granted.len(), 1);
        assert_eq!(diff.granted[0].capability, "tool:Write");
        assert_eq!(diff.granted[0].kind, CapabilityChangeKind::Granted);
    }

    #[test]
    fn capability_diff_detects_revoke() {
        let mut tracker = CapabilityDiffTracker::new();
        tracker.observe(&Capabilities::with_grants(["tool:Read", "tool:Write"]));
        let diff = tracker
            .observe(&Capabilities::with_grants(["tool:Read"]))
            .expect("revoke should produce a diff");
        assert_eq!(diff.granted.len(), 0);
        assert_eq!(diff.revoked.len(), 1);
        assert_eq!(diff.revoked[0].capability, "tool:Write");
        assert_eq!(diff.revoked[0].kind, CapabilityChangeKind::Revoked);
    }

    #[test]
    fn capability_diff_render_empty_returns_empty_string() {
        let diff = CapabilityDiff::default();
        assert_eq!(diff.render(), "");
    }

    #[test]
    fn capability_diff_render_includes_grants_and_revokes() {
        let diff = CapabilityDiff {
            granted: vec![CapabilityChange::granted("tool:Write")],
            revoked: vec![CapabilityChange::revoked("tool:Bash")],
        };
        let rendered = diff.render();
        assert!(rendered.contains("## Capability changes since last turn"));
        assert!(rendered.contains("- tool:Write"));
        assert!(rendered.contains("- tool:Bash"));
    }

    #[test]
    fn iteration_budget_render_mentions_iteration_and_max() {
        let s = IterationBudgetState {
            iteration: 3,
            max_iterations: 10,
        };
        let rendered = s.render();
        assert!(rendered.contains("Iteration 3 of 10"));
        assert!(!rendered.contains("Approaching limit"));
    }

    #[test]
    fn iteration_budget_render_warns_when_close_to_limit() {
        // At iteration 9 of 10: `9 >= max(10) - 2 = 8` so "Approaching
        // limit" is appended, but `9 < 10` so "Stop and finalize" is
        // not yet appended (that lands on iteration 10).
        let s = IterationBudgetState {
            iteration: 9,
            max_iterations: 10,
        };
        let rendered = s.render();
        assert!(rendered.contains("Approaching limit"));
        assert!(!rendered.contains("Stop and finalize"));
    }

    #[test]
    fn iteration_budget_render_emits_stop_at_max() {
        let s = IterationBudgetState {
            iteration: 10,
            max_iterations: 10,
        };
        let rendered = s.render();
        assert!(rendered.contains("Approaching limit"));
        assert!(rendered.contains("Stop and finalize"));
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
