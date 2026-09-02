//! `EnvironmentSnapshot` — minimal runtime snapshot injected into
//! the conversation at the head of a mid-turn compaction.
//!
//! When peko fires a mid-turn compaction (PR 3), the resulting
//! summary is spliced above the last user message. The model has
//! already lost the original system prompt's environment context,
//! so we inject a tiny snapshot block right next to the summary
//! giving the model enough to re-orient:
//!
//! - The runtime environment (os/arch/shell)
//! - The capability allowlist (so the model knows what tools it can
//!   invoke after compaction finishes)
//!
//! # Why this shape
//!
//! codex's `WorldState` carries 16 sections (sandbox, plugins,
//! personality, network, etc.). Peko doesn't need that — the runtime
//! pins the model context limit once at run start (line ~1084 in
//! `agentic_loop.rs`), so the snapshot only needs the two sections
//! that genuinely change between compactions. Two sections, ~50
//! lines.
//!
//! # Wire format
//!
//! The snapshot serializes as a JSON object so hooks and audit
//! tooling can consume it without parsing markdown. The
//! human-readable form is rendered via [`Self::render_markdown`].

use serde::{Deserialize, Serialize};

/// Minimal two-section runtime snapshot for mid-turn compaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    /// Free-form runtime environment string. The convention is
    /// `"<os>, shell=<shell>"` but the field is a plain `String` so
    /// future providers can include arch / container info without a
    /// schema change.
    pub runtime_environment: String,
    /// Permission policy summary as a list of capability names
    /// (e.g. `["tool:read", "tool:bash", "tool:session"]`). The list
    /// is rendered verbatim — `Vec<String>` keeps the wire shape
    /// predictable for hooks that pattern-match on capability names.
    pub permission_policy_summary: Vec<String>,
}

impl EnvironmentSnapshot {
    /// Build an empty snapshot. Useful as a `Default::default()`
    /// stand-in for tests and for call sites that don't have agent
    /// context yet.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            runtime_environment: String::new(),
            permission_policy_summary: Vec::new(),
        }
    }

    /// Render the snapshot as a markdown block suitable for
    /// injection directly into a `ContentBlock::Text`. The block is
    /// always non-empty (the section titles are always present) so
    /// downstream consumers can match on `## Environment Snapshot`
    /// without conditional logic.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::from("## Environment Snapshot\n\n");

        out.push_str("- **Runtime**: ");
        if self.runtime_environment.is_empty() {
            out.push_str("(unknown)");
        } else {
            out.push_str(&self.runtime_environment);
        }
        out.push('\n');

        out.push_str("- **Capabilities**: ");
        if self.permission_policy_summary.is_empty() {
            out.push_str("(none)");
        } else {
            out.push_str(&self.permission_policy_summary.join(", "));
        }
        out.push('\n');

        out
    }

    /// Build an `LlmMessage` carrying the rendered snapshot as a
    /// single `Text` block. The role is `System` so it sits with the
    /// other system-prompt content and gets the same persistence
    /// treatment.
    #[must_use]
    pub fn to_system_message(&self) -> peko_message::LlmMessage {
        use peko_message::{ContentBlock, MessageRole};

        peko_message::LlmMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: self.render_markdown(),
            }],
            ..Default::default()
        }
    }
}

impl Default for EnvironmentSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_renders_unknown_sections() {
        let snap = EnvironmentSnapshot::empty();
        let md = snap.render_markdown();
        assert!(md.contains("## Environment Snapshot"));
        assert!(md.contains("Runtime"));
        assert!(md.contains("(unknown)"));
        assert!(md.contains("Capabilities"));
        assert!(md.contains("(none)"));
    }

    #[test]
    fn populated_snapshot_renders_concrete_values() {
        let snap = EnvironmentSnapshot {
            runtime_environment: "linux, shell=bash".to_string(),
            permission_policy_summary: vec![
                "tool:read".to_string(),
                "tool:bash".to_string(),
                "tool:session".to_string(),
            ],
        };
        let md = snap.render_markdown();
        assert!(md.contains("linux, shell=bash"));
        assert!(md.contains("tool:read"));
        assert!(md.contains("tool:bash"));
        assert!(md.contains("tool:session"));
    }

    #[test]
    fn render_markdown_is_always_non_empty() {
        // Even an empty snapshot produces a non-empty block — the
        // section titles are always present so consumers can
        // pattern-match without conditionals.
        let snap = EnvironmentSnapshot::empty();
        assert!(!snap.render_markdown().is_empty());
    }

    #[test]
    fn serde_round_trips() {
        let original = EnvironmentSnapshot {
            runtime_environment: "macos, shell=zsh".to_string(),
            permission_policy_summary: vec!["tool:read".to_string()],
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: EnvironmentSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn to_system_message_produces_single_text_block() {
        use peko_message::{ContentBlock, MessageRole};
        let snap = EnvironmentSnapshot {
            runtime_environment: "linux".to_string(),
            permission_policy_summary: vec!["tool:read".to_string()],
        };
        let msg = snap.to_system_message();
        assert_eq!(msg.role, MessageRole::System);
        assert_eq!(msg.content.len(), 1);
        let ContentBlock::Text { text } = &msg.content[0] else {
            panic!("expected single Text block")
        };
        assert!(text.contains("## Environment Snapshot"));
        assert!(text.contains("linux"));
    }

    #[test]
    fn default_matches_empty() {
        assert_eq!(EnvironmentSnapshot::default(), EnvironmentSnapshot::empty());
    }
}