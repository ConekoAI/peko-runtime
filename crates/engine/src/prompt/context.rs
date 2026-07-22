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
//! - [`TurnPromptContext::quota_state`] — emitted at `{{quota_state}}`
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
use std::time::SystemTime;

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

/// Quota state for the `{{quota_state}}` control surface.
///
/// `None` ⇒ the principal is unquota'd and the section is omitted
/// entirely (the template's `remove_missing=true` semantics handle the
/// empty case too, but skipping avoids even computing the section).
#[derive(Debug, Clone)]
pub struct QuotaStateView {
    /// Live snapshot from `QuotaMeter::snapshot()`.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub request_count: u64,
    /// Window end timestamp (ISO 8601 when rendered).
    pub window_end: SystemTime,
    /// Configured limits (`None` ⇒ unlimited for that dimension).
    pub input_limit: Option<u64>,
    pub output_limit: Option<u64>,
    pub request_limit: Option<u64>,
}

impl QuotaStateView {
    /// Render the section body. Returns the empty string when the
    /// config has no limits (Phase 3 may revisit this).
    #[must_use]
    pub fn render(&self) -> String {
        if self.input_limit.is_none() && self.output_limit.is_none() && self.request_limit.is_none()
        {
            return String::new();
        }
        let pct = |used, limit: Option<u64>| -> String {
            match limit {
                Some(l) if l > 0 => {
                    let p = (used as f64 / l as f64) * 100.0;
                    format!("{}%", p.round() as u64)
                }
                _ => "—".to_string(),
            }
        };
        let mut lines = vec!["## Quota status (current window)".to_string()];
        lines.push(format!(
            "Input tokens:    {}/{} ({})",
            self.input_tokens,
            self.input_limit.map_or("—".to_string(), |n| n.to_string()),
            pct(self.input_tokens, self.input_limit)
        ));
        lines.push(format!(
            "Output tokens:   {}/{} ({})",
            self.output_tokens,
            self.output_limit.map_or("—".to_string(), |n| n.to_string()),
            pct(self.output_tokens, self.output_limit)
        ));
        lines.push(format!(
            "Requests:        {}/{} ({})",
            self.request_count,
            self.request_limit
                .map_or("—".to_string(), |n| n.to_string()),
            pct(self.request_count, self.request_limit)
        ));
        let reset = self
            .window_end
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| {
                chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| "unknown".to_string())
            })
            .unwrap_or_else(|_| "unknown".to_string());
        lines.push(format!("Window resets:   {reset}"));

        let tripped = [
            (self.input_limit, self.input_tokens),
            (self.output_limit, self.output_tokens),
            (self.request_limit, self.request_count),
        ]
        .iter()
        .any(|(l, u)| l.is_some_and(|lim| u >= &lim));
        if tripped {
            lines.push("Quota tripped — pause non-essential work.".to_string());
        }

        lines.join("\n") + "\n"
    }
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

    // ---- Control surfaces ----
    /// Iteration-budget state (`None` ⇒ `{{iteration_budget}}` not rendered).
    pub iteration_budget: Option<IterationBudgetState>,
    /// Quota snapshot (`None` ⇒ `{{quota_state}}` not rendered).
    pub quota_state: Option<QuotaStateView>,
    /// Soft-cancel pending flag (`false` ⇒ `{{soft_cancel}}` not rendered).
    pub soft_cancel_pending: bool,
    /// Capability diff vs last observation (`None` ⇒ `{{capability_diff}}` not rendered).
    pub capability_diff: Option<CapabilityDiff>,

    /// Tool definitions resolved by the loop for this iteration. The
    /// renderer does NOT re-fetch these — the loop has already
    /// consulted the capability allowlist and active extension set,
    /// so the renderer just threads them into the `{{tools}}` hook
    /// input for handler introspection. Tools themselves are
    /// advertised via the `tools` section hook, which the renderer
    /// invokes the same way as before.
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
    fn quota_state_render_unlimited_returns_empty() {
        let v = QuotaStateView {
            input_tokens: 0,
            output_tokens: 0,
            request_count: 0,
            window_end: SystemTime::UNIX_EPOCH,
            input_limit: None,
            output_limit: None,
            request_limit: None,
        };
        assert_eq!(v.render(), "");
    }

    #[test]
    fn quota_state_render_includes_pct() {
        let v = QuotaStateView {
            input_tokens: 50,
            output_tokens: 0,
            request_count: 0,
            window_end: SystemTime::UNIX_EPOCH,
            input_limit: Some(100),
            output_limit: None,
            request_limit: None,
        };
        let rendered = v.render();
        assert!(rendered.contains("50/100"));
        assert!(rendered.contains("50%"));
    }

    #[test]
    fn quota_state_render_trip_message_when_exceeded() {
        let v = QuotaStateView {
            input_tokens: 100,
            output_tokens: 0,
            request_count: 0,
            window_end: SystemTime::UNIX_EPOCH,
            input_limit: Some(100),
            output_limit: None,
            request_limit: None,
        };
        let rendered = v.render();
        assert!(rendered.contains("Quota tripped"));
    }
}
