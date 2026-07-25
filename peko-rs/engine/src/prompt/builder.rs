//! Test-only static system-prompt builder.
//!
//! ## Status
//!
//! As of the per-turn rebuild refactor, the production rendering path is
//! [`super::renderer::PromptRenderer::render_for_iteration`], which
//! renders the system prompt fresh every iteration from a
//! [`super::context::TurnPromptContext`]. The renderer dispatches all
//! hook-driven sections (`tools`, `skills`, `agents`, `mcp_context`,
//! `SessionContextBuild`) via [`ExtensionCore`] and threads the four
//! long-horizon control surfaces (`iteration_budget`, `quota_state`,
//! `soft_cancel`, `capability_diff`) into the body.
//!
//! This module survives as a **test-only** static renderer — a pure
//! function from `(body, memory, session_context, agent_name, ...)`
//! to a Markdown body with `{{placeholder}}` substitution and no hook
//! dispatch. Tests that exercise the placeholder-replacement path
//! without standing up an `ExtensionCore` (e.g. `memory_placeholder_*`,
//! `session_context_placeholder_*`) live here.
//!
//! Production callers should never use `SystemPromptBuilder`. If you
//! reach for it from a non-test path, you almost certainly want
//! [`PromptRenderer`](super::renderer::PromptRenderer) instead.

use super::placeholder::{replace_placeholders, Placeholder};
use chrono::Local;
use std::collections::HashMap;
use std::path::PathBuf;

/// Prompt mode - controls which sections are included.
///
/// Phase 1 keeps this enum for the existing test surface
/// (`PromptMode::{Full, Minimal, None}`). The renderer's hot path
/// ignores it; the static builder honors it for the test-only code.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PromptMode {
    /// All sections (default for main sessions)
    #[default]
    Full,
    /// Reduced sections (for sub-agents)
    Minimal,
    /// Base identity only
    None,
}

/// Static, hook-free system prompt builder.
///
/// See the [module-level docs](self) for why this survives as
/// test-only.
pub struct SystemPromptBuilder {
    mode: PromptMode,
    body: String,
    agent_name: String,
    workspace: PathBuf,
    model: String,
    thinking_level: String,
    has_gateway: bool,
    model_aliases: Vec<String>,
    sandbox_enabled: bool,
    channel: String,
    /// Per-principal long-term memory loaded from `<workspace>/MEMORY.md`.
    principal_memory: Option<String>,
    /// Bootstrap context returned by `SessionContextBuild` handlers.
    session_context: Option<String>,
}

impl SystemPromptBuilder {
    pub fn new(agent_name: &str) -> Self {
        Self {
            mode: PromptMode::Full,
            body: String::new(),
            agent_name: agent_name.to_string(),
            workspace: PathBuf::from("."),
            model: "default".to_string(),
            thinking_level: "medium".to_string(),
            has_gateway: true,
            model_aliases: vec![],
            sandbox_enabled: false,
            channel: "discord".to_string(),
            principal_memory: None,
            session_context: None,
        }
    }

    pub fn with_mode(mut self, mode: PromptMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_workspace(mut self, workspace: impl AsRef<std::path::Path>) -> Self {
        self.workspace = workspace.as_ref().to_path_buf();
        self
    }

    /// Set the agent's prompt body (Markdown with `{{placeholder}}` syntax).
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// Set the per-principal long-term memory loaded from `<workspace>/MEMORY.md`.
    pub fn with_principal_memory(mut self, memory: impl Into<String>) -> Self {
        self.principal_memory = Some(memory.into());
        self
    }

    /// Set bootstrap context returned by `SessionContextBuild` handlers.
    pub fn with_session_context(mut self, context: impl Into<String>) -> Self {
        self.session_context = Some(context.into());
        self
    }

    /// Build the complete system prompt from the body + section renderings.
    pub fn build(self) -> String {
        if self.mode == PromptMode::None {
            return format!("You are {}.", self.agent_name);
        }

        // Empty body → minimal identity fallback.
        if self.body.trim().is_empty() {
            return format!("You are {}.", self.agent_name);
        }

        let template = self.body.clone();
        let mut values = HashMap::new();

        // Inline placeholders
        values.insert(Placeholder::AgentName, self.agent_name.clone());
        values.insert(Placeholder::Workspace, self.workspace.display().to_string());
        values.insert(Placeholder::Channel, self.channel.clone());
        values.insert(Placeholder::ThinkingLevel, self.thinking_level.clone());
        values.insert(
            Placeholder::Timezone,
            Local::now().format("%:z").to_string(),
        );

        // Hook-driven sections render empty in the static builder; tests
        // exercise these by inspecting placeholder text directly, not
        // by checking section headers.
        values.insert(Placeholder::Tools, String::new());
        values.insert(Placeholder::Skills, String::new());
        values.insert(Placeholder::Agents, String::new());
        values.insert(Placeholder::Runtime, self.render_runtime_section());
        values.insert(Placeholder::Sandbox, self.render_sandbox_section());
        values.insert(
            Placeholder::ModelAliases,
            self.render_model_aliases_section(),
        );
        values.insert(Placeholder::SelfUpdate, self.render_self_update_section());
        values.insert(Placeholder::McpContext, String::new());
        values.insert(Placeholder::Memory, self.render_memory_section());
        values.insert(
            Placeholder::SessionContext,
            self.render_session_context_section(),
        );

        // Control surfaces — Phase 1 static builder always emits empty;
        // the renderer populates these from `TurnPromptContext`.
        values.insert(Placeholder::IterationBudget, String::new());
        values.insert(Placeholder::QuotaState, String::new());
        values.insert(Placeholder::SoftCancel, String::new());
        values.insert(Placeholder::CapabilityDiff, String::new());

        replace_placeholders(&template, &values, true)
    }

    fn render_runtime_section(&self) -> String {
        let mut lines: Vec<String> = vec![];
        lines.push("## Runtime".to_string());
        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown".to_string());
        lines.push(format!("Agent: {}", self.agent_name));
        lines.push(format!("Host: {hostname}"));
        lines.push(format!("OS: {}", std::env::consts::OS));
        lines.push(format!("Model: {}", self.model));
        lines.push(format!("Channel: {}", self.channel));
        lines.join("\n")
    }

    fn render_sandbox_section(&self) -> String {
        if self.sandbox_enabled {
            "## Sandbox\nSandbox: enabled\nTools run in isolated environment with restricted access.".to_string()
        } else {
            String::new()
        }
    }

    fn render_model_aliases_section(&self) -> String {
        if self.model_aliases.is_empty() {
            String::new()
        } else {
            let mut lines = vec!["## Model Aliases".to_string()];
            lines.push("Prefer aliases when specifying model overrides; full provider/model is also accepted.".to_string());
            for alias in &self.model_aliases {
                lines.push(format!("- {alias}"));
            }
            lines.join("\n")
        }
    }

    fn render_self_update_section(&self) -> String {
        let is_minimal = self.mode == PromptMode::Minimal;
        if self.has_gateway && !is_minimal {
            "## Self-Update\n\
            Get Updates (self-update) is ONLY allowed when the user explicitly asks for it.\n\
            Do not run config.apply or update.run unless the user explicitly requests an update or config change; if it's not explicit, ask first.\n\
            Actions: config.get, config.schema, config.apply (validate + write full config, then restart), update.run (update deps or git, then restart).\n\
            After restart, OpenClaw pings the last active session automatically.".to_string()
        } else {
            String::new()
        }
    }

    /// Render the principal long-term memory section.
    fn render_memory_section(&self) -> String {
        let Some(memory) = self.principal_memory.as_deref() else {
            return String::new();
        };
        let trimmed = memory.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        format!("## Your long-term memory (MEMORY.md)\n\n{trimmed}\n")
    }

    /// Render the session bootstrap context section.
    fn render_session_context_section(&self) -> String {
        let Some(context) = self.session_context.as_deref() else {
            return String::new();
        };
        let trimmed = context.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        format!("## Session context\n\n{trimmed}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_builder_basic() {
        let builder = SystemPromptBuilder::new("test-agent").with_mode(PromptMode::None);

        let prompt = builder.build();
        assert_eq!(prompt, "You are test-agent.");
    }

    #[test]
    fn test_builder_with_template() {
        let template = r"## Your Role
You are {{agent_name}}.

{{tools}}

## Safety
Be safe.

{{runtime}}";

        let builder = SystemPromptBuilder::new("test-agent")
            .with_body(template)
            .with_mode(PromptMode::Full);

        let prompt = builder.build();

        assert!(prompt.contains("You are test-agent."));
        // The static builder renders hook-driven sections (e.g.
        // `{{tools}}`) as empty: the production renderer is
        // responsible for dispatching hook handlers. The template
        // still places inline placeholders like `{{runtime}}` which
        // is rendered from local fields.
        assert!(prompt.contains("## Runtime"));
        assert!(prompt.contains("Agent: test-agent"));
        assert!(!prompt.contains("{{agent_name}}"));
        assert!(!prompt.contains("{{runtime}}"));
        // `{{tools}}` substitutes to empty (no hook dispatch here).
        assert!(!prompt.contains("{{tools}}"));
    }

    #[test]
    fn test_builder_no_template_fallback() {
        let builder = SystemPromptBuilder::new("test-agent").with_mode(PromptMode::Full);

        let prompt = builder.build();

        assert_eq!(prompt, "You are test-agent.");
    }

    #[test]
    fn test_placeholder_replacement_inline() {
        let tmp = TempDir::new().unwrap();
        let template = r"Agent: {{agent_name}}
Workspace: {{workspace}}
Channel: {{channel}}
Level: {{thinking_level}}";

        let builder = SystemPromptBuilder::new("my-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Full)
            .with_body(template);

        let prompt = builder.build();

        assert!(prompt.contains("Agent: my-agent"));
        assert!(prompt.contains("Workspace:"));
        assert!(prompt.contains("Channel: discord"));
        assert!(prompt.contains("Level: medium"));
    }

    #[test]
    fn test_minimal_mode_basic() {
        let tmp = TempDir::new().unwrap();
        let template = r"## Your Role
You are {{agent_name}}.

{{tools}}

{{runtime}}";

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Minimal)
            .with_body(template);

        let prompt = builder.build();

        assert!(prompt.contains("## Your Role"));
        // Hook-driven sections like `{{tools}}` are empty in the static
        // builder (no hook dispatch); `{{runtime}}` is rendered from
        // local fields and survives in Minimal mode.
        assert!(prompt.contains("## Runtime"));
        assert!(!prompt.contains("{{tools}}"));
        assert!(!prompt.contains("{{runtime}}"));
    }

    /// `{{memory}}` placeholder renders the loaded MEMORY.md content
    /// under the standard `## Your long-term memory (MEMORY.md)` header.
    #[test]
    fn memory_placeholder_renders_content_when_set() {
        let template = "You are {{agent_name}}.\n\n{{memory}}\n";

        let prompt = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Full)
            .with_body(template)
            .with_principal_memory("Prefer tabs over spaces.")
            .build();

        assert!(
            prompt.contains("## Your long-term memory (MEMORY.md)"),
            "expected the memory section header in: {prompt}"
        );
        assert!(
            prompt.contains("Prefer tabs over spaces."),
            "expected the memory body in: {prompt}"
        );
        assert!(!prompt.contains("{{memory}}"));
    }

    /// When the template omits `{{memory}}`, the builder must NOT
    /// append the memory section unconditionally.
    #[test]
    fn memory_placeholder_omitted_when_not_in_template() {
        let template = "You are {{agent_name}}.\n";

        let prompt = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Full)
            .with_body(template)
            .with_principal_memory("should not appear")
            .build();

        assert!(
            !prompt.contains("## Your long-term memory"),
            "memory section leaked into a template that did not opt in: {prompt}"
        );
        assert!(
            !prompt.contains("should not appear"),
            "memory body leaked into a template that did not opt in: {prompt}"
        );
    }

    /// When the template omits `{{session_context}}`, the builder must NOT
    /// append the bootstrap context unconditionally.
    #[test]
    fn session_context_placeholder_omitted_when_not_in_template() {
        let template = "You are {{agent_name}}.\n";

        let prompt = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Full)
            .with_body(template)
            .with_session_context("should not appear")
            .build();

        assert!(
            !prompt.contains("## Session context"),
            "session context section leaked into a template that did not opt in: {prompt}"
        );
        assert!(
            !prompt.contains("should not appear"),
            "session context body leaked into a template that did not opt in: {prompt}"
        );
    }

    /// `{{session_context}}` placeholder renders the bootstrap context
    /// under the standard `## Session context` header.
    #[test]
    fn session_context_placeholder_renders_content_when_set() {
        let template = "You are {{agent_name}}.\n\n{{session_context}}\n";

        let prompt = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Full)
            .with_body(template)
            .with_session_context("Always use the Superpowers skill pack.")
            .build();

        assert!(
            prompt.contains("## Session context"),
            "expected the session context section header in: {prompt}"
        );
        assert!(
            prompt.contains("Always use the Superpowers skill pack."),
            "expected the session context body in: {prompt}"
        );
        assert!(!prompt.contains("{{session_context}}"));
    }

    /// When `session_context` is `None`, `{{session_context}}` resolves to the
    /// empty string.
    #[test]
    fn session_context_placeholder_removed_when_context_unset() {
        let template = "You are {{agent_name}}.\n\n{{session_context}}\n";

        let prompt = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Full)
            .with_body(template)
            .build();

        assert!(
            !prompt.contains("{{session_context}}"),
            "unreplaced {{session_context}} marker leaked into: {prompt}"
        );
        assert!(
            !prompt.contains("## Session context"),
            "session context section rendered with no context: {prompt}"
        );
    }
}
