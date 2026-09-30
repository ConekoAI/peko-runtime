//! Placeholder replacement for system prompt templates
//!
//! The placeholder surface is **inline variables only** — identity,
//! workspace, channel, thinking level, clock. Section-shaped content
//! never rides a placeholder from the production renderer: sections
//! either render as `<runtime-context>` tail sections (change-detected)
//! or are appended to the frozen prefix as generated stable sections
//! when the template didn't place them. Templates may still *place* a
//! section via its placeholder token, but the token is not a required
//! contract.
//!
//! Unknown `{{...}}` tokens are stripped by
//! [`replace_placeholders`] with `remove_missing=true`, which is what
//! retires retired markers (e.g. `{{tools}}`, `{{quota_state}}`):
//! old templates keep rendering cleanly with no dead enum variants.

use std::collections::HashMap;

/// Available placeholders for system prompt templates
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placeholder {
    /// Skills section - {{skills}}
    Skills,
    /// Roles section - {{roles}} (ADR-064 renamed the catalog section).
    Roles,
    /// Runtime info (agent, host, OS, model, channel) - {{runtime}}
    Runtime,
    /// Sandbox status - {{sandbox}}
    Sandbox,
    /// Model aliases - {{`model_aliases`}}
    ModelAliases,
    /// Self-update section - {{`self_update`}}
    SelfUpdate,
    /// Timezone - {{timezone}}
    Timezone,
    /// Current date/time (local + UTC) - {{current_time}}
    ///
    /// Volatile, per-turn. Gives the model a wall clock so
    /// relative-time requests ("remind me in 2 minutes") can be
    /// resolved to absolute timestamps (2026-08-07 field test, N2a).
    CurrentTime,
    /// Role name inline - {{`role_name`}} (ADR-064: the name is the
    /// role the session was initiated from).
    RoleName,
    /// Deprecated legacy alias for [`Placeholder::RoleName`] — same
    /// value, legacy marker {{`agent_name`}}.
    AgentName,
    /// Workspace path inline - {{workspace}}
    Workspace,
    /// Channel inline - {{channel}}
    Channel,
    /// Thinking level inline - {{`thinking_level`}}
    ThinkingLevel,
    /// MCP server context section - {{mcp_context}}
    McpContext,
    /// Principal long-term memory from kb/MEMORY.md - {{memory}}
    Memory,
    /// Extension bootstrap context from per-turn SessionContextBuild hooks - {{session_context}}
    SessionContext,
    /// Iteration counter (no ceiling) - {{iteration_budget}}
    IterationBudget,
    /// Quota-tripped tripwire - {{quota_tripped}}
    ///
    /// Volatile, per-turn, but only on the rising edge: rendered as a
    /// short banner the iteration the principal's quota first trips;
    /// subsequent iterations while still tripped render to empty so the
    /// banner doesn't spam the prompt. Mirrors the `{{soft_cancel}}`
    /// single-shot pattern.
    QuotaTripped,
    /// Soft-cancel pending flag - {{soft_cancel}}
    SoftCancel,
    /// Capability-diff since last render - {{capability_diff}}
    CapabilityDiff,
}

impl Placeholder {
    /// Get the placeholder marker for this variant
    pub fn marker(&self) -> &'static str {
        match self {
            Self::Skills => "{{skills}}",
            Self::Roles => "{{roles}}",
            Self::Runtime => "{{runtime}}",
            Self::Sandbox => "{{sandbox}}",
            Self::ModelAliases => "{{model_aliases}}",
            Self::SelfUpdate => "{{self_update}}",
            Self::Timezone => "{{timezone}}",
            Self::CurrentTime => "{{current_time}}",
            Self::RoleName => "{{role_name}}",
            Self::AgentName => "{{agent_name}}",
            Self::Workspace => "{{workspace}}",
            Self::Channel => "{{channel}}",
            Self::ThinkingLevel => "{{thinking_level}}",
            Self::McpContext => "{{mcp_context}}",
            Self::Memory => "{{memory}}",
            Self::SessionContext => "{{session_context}}",
            Self::IterationBudget => "{{iteration_budget}}",
            Self::QuotaTripped => "{{quota_tripped}}",
            Self::SoftCancel => "{{soft_cancel}}",
            Self::CapabilityDiff => "{{capability_diff}}",
        }
    }
}

/// Replace placeholders in template content with provided values
///
/// Placeholders not found in `values` are left as-is or removed based on `remove_missing`.
pub fn replace_placeholders(
    template: &str,
    values: &HashMap<Placeholder, String>,
    remove_missing: bool,
) -> String {
    let mut result = template.to_string();

    for (placeholder, value) in values {
        result = result.replace(placeholder.marker(), value);
    }

    if remove_missing {
        // Remove any remaining unreplaced placeholders
        // Pattern: {{word_chars}}
        let re = regex::Regex::new(r"\{\{[a-z_]+\}\}").unwrap();
        result = re.replace_all(&result, "").to_string();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_placeholder_markers() {
        assert_eq!(Placeholder::Runtime.marker(), "{{runtime}}");
        assert_eq!(Placeholder::Memory.marker(), "{{memory}}");
        assert_eq!(Placeholder::SessionContext.marker(), "{{session_context}}");
        assert_eq!(
            Placeholder::IterationBudget.marker(),
            "{{iteration_budget}}"
        );
        assert_eq!(Placeholder::QuotaTripped.marker(), "{{quota_tripped}}");
        assert_eq!(Placeholder::SoftCancel.marker(), "{{soft_cancel}}");
        assert_eq!(Placeholder::CapabilityDiff.marker(), "{{capability_diff}}");
    }

    /// Retired markers (`{{tools}}`, `{{quota_state}}`) have no enum
    /// variant any more — but old templates containing them still
    /// render cleanly: with a value map lacking those keys and
    /// `remove_missing=true`, the unknown-token regex strips them.
    /// (Without `remove_missing`, unknown markers pass through
    /// untouched — the caller decides.)
    #[test]
    fn retired_markers_are_stripped_by_remove_missing() {
        let template = "body {{tools}} {{quota_state}} tail";
        let values = HashMap::new();
        let rendered = replace_placeholders(template, &values, true);
        assert_eq!(rendered, "body   tail");
    }

    #[test]
    fn test_replace_placeholders() {
        let template = "Hello {{agent_name}}, model: {{runtime}}";
        let mut values = HashMap::new();
        values.insert(Placeholder::AgentName, "test-agent".to_string());
        values.insert(Placeholder::Runtime, "## Runtime\nModel: m".to_string());

        let result = replace_placeholders(template, &values, false);
        assert_eq!(result, "Hello test-agent, model: ## Runtime\nModel: m");
    }

    #[test]
    fn test_replace_placeholders_remove_missing() {
        let template = "Hello {{agent_name}}, missing: {{unknown}}";
        let mut values = HashMap::new();
        values.insert(Placeholder::AgentName, "test-agent".to_string());

        let result = replace_placeholders(template, &values, true);
        assert_eq!(result, "Hello test-agent, missing: ");
    }
}
