//! Per-iteration state for the agentic loop.
//!
//! Phase 9b.N.5a lifted `CapabilityDiffTracker` from
//! `src/agents/prompt/context.rs` into `peko-engine` because the
//! tracker lives on the [`AgenticLoop`](crate) and observes the
//! principal's capability snapshot each iteration. The companion
//! types ([`CapabilityChange`], [`CapabilityChangeKind`],
//! [`CapabilityDiff`]) come along because the tracker returns them
//! to the renderer. The renderer in `src/agents/prompt/` keeps its
//! own copy via the orphan rule-friendly `pub use` re-export from
//! `src/engine/mod.rs` — both crates define the same shape; the
//! root side remains the source of truth until `PromptRenderer`
//! itself lifts.
//!
//! The loop holds the [`CapabilityDiffTracker`]; the renderer reads
//! the returned diff each iteration. The tracker is the only state
//! that's loop-owned — `IterationBudgetState` + `QuotaStateView` are
//! constructed per-iteration from the loop's read-only fields, so
//! they stay in `src/agents/prompt/context.rs` until that file lifts.

use peko_extension_api::Capabilities;

/// A single capability change between two consecutive renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityChange {
    /// The capability string (e.g. `tool:Bash`).
    pub capability: String,
    /// Whether the change was a grant or a revoke.
    pub kind: CapabilityChangeKind,
}

impl CapabilityChange {
    /// Construct a `Granted` change for `cap`.
    #[must_use]
    pub fn granted(cap: &str) -> Self {
        Self {
            capability: cap.to_string(),
            kind: CapabilityChangeKind::Granted,
        }
    }

    /// Construct a `Revoked` change for `cap`.
    #[must_use]
    pub fn revoked(cap: &str) -> Self {
        Self {
            capability: cap.to_string(),
            kind: CapabilityChangeKind::Revoked,
        }
    }
}

/// Whether a capability was newly granted or newly revoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityChangeKind {
    Granted,
    Revoked,
}

/// Diff between two consecutive capability snapshots.
///
/// Returned by [`CapabilityDiffTracker::observe`]. When the principal's
/// grants are unchanged, this is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityDiff {
    pub granted: Vec<CapabilityChange>,
    pub revoked: Vec<CapabilityChange>,
}

impl CapabilityDiff {
    /// True when no changes are recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty() && self.revoked.is_empty()
    }

    /// Render the diff as a Markdown section. Returns the empty string
    /// when the diff is empty so templates that opt into
    /// `{{capability_diff}}` get no section when there are no changes.
    #[must_use]
    pub fn render(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut lines = vec!["## Capability changes since last turn".to_string()];
        if !self.granted.is_empty() {
            lines.push(String::new());
            lines.push("Granted:".to_string());
            for c in &self.granted {
                lines.push(format!("- {}", c.capability));
            }
        }
        if !self.revoked.is_empty() {
            lines.push(String::new());
            lines.push("Revoked:".to_string());
            for c in &self.revoked {
                lines.push(format!("- {}", c.capability));
            }
        }
        lines.join("\n") + "\n"
    }
}

/// Tracks capability diffs across iterations of one agentic loop.
///
/// The tracker stores the last-observed grant set as a sorted `Vec<String>`
/// and computes a [`CapabilityDiff`] when the next snapshot differs. The
/// first observation is always "no diff" — establishing the baseline so
/// subsequent iterations can diff against it.
///
/// Owned by [`crate::engine::AgenticLoop`] (root). Lifted here in Phase
/// 9b.N.5a because the loop is moving into `peko-engine` in 9b.N.5b.
#[derive(Debug, Default)]
pub struct CapabilityDiffTracker {
    last_snapshot: Option<Vec<String>>,
}

impl CapabilityDiffTracker {
    /// Create a tracker with no prior observation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe the current capability snapshot and return the diff
    /// against the last observation, if any.
    ///
    /// - First call: returns `None` (baseline).
    /// - Subsequent calls: returns `Some(diff)` when the set changed,
    ///   `None` otherwise.
    pub fn observe(&mut self, current: &Capabilities) -> Option<CapabilityDiff> {
        let mut current_sorted: Vec<String> = current.to_strings();
        current_sorted.sort();

        let diff = match &self.last_snapshot {
            None => None,
            Some(prev) => {
                if prev == &current_sorted {
                    None
                } else {
                    let prev_set: std::collections::HashSet<&str> =
                        prev.iter().map(String::as_str).collect();
                    let cur_set: std::collections::HashSet<&str> =
                        current_sorted.iter().map(String::as_str).collect();
                    let granted: Vec<CapabilityChange> = current_sorted
                        .iter()
                        .filter(|c| !prev_set.contains(c.as_str()))
                        .map(|c| CapabilityChange::granted(c))
                        .collect();
                    let revoked: Vec<CapabilityChange> = prev
                        .iter()
                        .filter(|c| !cur_set.contains(c.as_str()))
                        .map(|c| CapabilityChange::revoked(c))
                        .collect();
                    Some(CapabilityDiff { granted, revoked })
                }
            }
        };

        self.last_snapshot = Some(current_sorted);
        diff
    }
}
