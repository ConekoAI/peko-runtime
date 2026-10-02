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
//!
//! Registered tools remain on the native wire catalog after compaction;
//! the snapshot carries only runtime context (ADR-066 P6).
//!
//! # Wire format
//!
//! The snapshot serializes as a JSON object so hooks and audit
//! tooling can consume it without parsing markdown. The
//! human-readable form is rendered via [`Self::render_markdown`].

use serde::{Deserialize, Serialize};

/// Minimal runtime snapshot for mid-turn compaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    /// Free-form runtime environment string. The convention is
    /// `"<os>, shell=<shell>"` but the field is a plain `String` so
    /// future providers can include arch / container info without a
    /// schema change.
    pub runtime_environment: String,
}

impl EnvironmentSnapshot {
    /// Build an empty snapshot. Useful as a `Default::default()`
    /// stand-in for tests and for call sites that don't have agent
    /// context yet.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            runtime_environment: String::new(),
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
    }

    #[test]
    fn populated_snapshot_renders_concrete_values() {
        let snap = EnvironmentSnapshot {
            runtime_environment: "linux, shell=bash".to_string(),
        };
        let md = snap.render_markdown();
        assert!(md.contains("linux, shell=bash"));
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
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: EnvironmentSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn legacy_allowlist_is_dropped_without_losing_runtime_context() {
        let snapshot: EnvironmentSnapshot = serde_json::from_value(serde_json::json!({
            "runtime_environment": "linux, shell=bash",
            "permission_policy_summary": ["tool:Read", "tool:Bash"]
        }))
        .unwrap();
        assert_eq!(snapshot.runtime_environment, "linux, shell=bash");
        assert!(!snapshot.render_markdown().contains("Capabilities"));
        assert!(serde_json::to_value(snapshot)
            .unwrap()
            .get("permission_policy_summary")
            .is_none());
    }

    #[test]
    fn to_system_message_produces_single_text_block() {
        use peko_message::{ContentBlock, MessageRole};
        let snap = EnvironmentSnapshot {
            runtime_environment: "linux".to_string(),
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
