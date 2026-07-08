//! System prompt builder with multi-section support
//!
//! The agent's system prompt is a single Markdown body (see
//! [`crate::agents::agent_config::AgentConfig::prompt`]).
//! Placeholders (`{{tools}}`, `{{skills}}`, `{{agents}}`,
//! `{{runtime}}`, etc.) are replaced at build time with rendered
//! sections. An empty body falls back to a one-line identity.

use crate::agents::prompt::placeholder::{replace_placeholders, Placeholder};
use crate::providers::ToolDefinition;
use crate::tools::Tool;
use chrono::Local;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Prompt mode - controls which sections are included
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

/// System prompt builder
pub struct SystemPromptBuilder {
    mode: PromptMode,
    body: String,
    tools: Vec<Arc<dyn Tool>>,
    /// Tool definitions from unified registry (ADR-019 Phase 3)
    tool_definitions: Vec<ToolDefinition>,
    agent_name: String,
    workspace: PathBuf,
    model: String,
    thinking_level: String,
    has_gateway: bool,
    model_aliases: Vec<String>,
    sandbox_enabled: bool,
    channel: String,
    /// Optional extension core for hook integration (Phase 1: Extension Architecture)
    extension_core: Option<Arc<crate::extensions::framework::ExtensionCore>>,
    /// Principal runtime id for extension-scoped prompt hooks (P2 audit).
    principal_id: Option<String>,
    /// Per-principal long-term memory loaded from `<workspace>/MEMORY.md`.
    /// Rendered into the system prompt at the `{{memory}}` placeholder
    /// when the template opts in; templates that omit `{{memory}}` get
    /// no section. The principal owns this file and may update it via
    /// `Write`.
    principal_memory: Option<String>,
    /// Bootstrap context returned by `HookPoint::SessionStart` handlers.
    /// Rendered at the `{{session_context}}` placeholder.
    session_context: Option<String>,
}

impl SystemPromptBuilder {
    pub fn new(agent_name: &str) -> Self {
        Self {
            mode: PromptMode::Full,
            body: String::new(),
            tools: vec![],
            tool_definitions: vec![],
            agent_name: agent_name.to_string(),
            workspace: PathBuf::from("."),
            model: "default".to_string(),
            thinking_level: "medium".to_string(),
            has_gateway: true,
            model_aliases: vec![],
            sandbox_enabled: false,
            channel: "discord".to_string(),
            extension_core: None,
            principal_id: None,
            principal_memory: None,
            session_context: None,
        }
    }

    /// Set the extension core for hook integration
    ///
    /// This enables extensions to inject content into prompt sections.
    pub fn with_extension_core(
        mut self,
        core: Arc<crate::extensions::framework::ExtensionCore>,
    ) -> Self {
        self.extension_core = Some(core);
        self
    }

    /// Set the principal id for extension-scoped prompt hooks.
    ///
    /// The `skills` section uses this to filter by the principal's
    /// enabled-skill allowlist at handle time.
    pub fn with_principal_id(mut self, principal_id: impl Into<String>) -> Self {
        self.principal_id = Some(principal_id.into());
        self
    }

    /// Set the per-principal long-term memory loaded from `<workspace>/MEMORY.md`.
    ///
    /// When set, the builder renders the memory at the `{{memory}}`
    /// placeholder in the prompt body. Templates that omit `{{memory}}`
    /// get no section. Pass `None` (the default) when the file does not
    /// exist.
    pub fn with_principal_memory(mut self, memory: impl Into<String>) -> Self {
        self.principal_memory = Some(memory.into());
        self
    }

    /// Set bootstrap context returned by `HookPoint::SessionStart` handlers.
    ///
    /// When set, the builder renders the context at the `{{session_context}}`
    /// placeholder. Templates that omit `{{session_context}}` get no section.
    pub fn with_session_context(mut self, context: impl Into<String>) -> Self {
        self.session_context = Some(context.into());
        self
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

    /// Build the complete system prompt from the body + section renderings
    pub fn build(self) -> String {
        if self.mode == PromptMode::None {
            return format!("You are {}.", self.agent_name);
        }

        let is_minimal = self.mode == PromptMode::Minimal;

        // 1. Resolve the template body. Empty body → minimal identity fallback.
        if self.body.trim().is_empty() {
            return format!("You are {}.", self.agent_name);
        }
        // Clone the body out of `self` so subsequent `&self` section
        // builder calls don't trip a partial-move error.
        let template = self.body.clone();

        // 2. Build placeholder values
        let mut values = HashMap::new();

        // Simple inline placeholders
        values.insert(Placeholder::AgentName, self.agent_name.clone());
        values.insert(Placeholder::Workspace, self.workspace.display().to_string());
        values.insert(Placeholder::Channel, self.channel.clone());
        values.insert(Placeholder::ThinkingLevel, self.thinking_level.clone());
        values.insert(
            Placeholder::Timezone,
            Local::now().format("%:z").to_string(),
        );

        // Complex section placeholders
        values.insert(Placeholder::Tools, self.build_tools_section());
        values.insert(Placeholder::Skills, self.build_skills_section());
        values.insert(Placeholder::Agents, self.build_agents_section());
        values.insert(Placeholder::Runtime, self.build_runtime_section());
        values.insert(Placeholder::Sandbox, self.build_sandbox_section());
        values.insert(
            Placeholder::ModelAliases,
            self.build_model_aliases_section(),
        );
        values.insert(
            Placeholder::SelfUpdate,
            self.build_self_update_section(is_minimal),
        );
        values.insert(Placeholder::McpContext, self.build_mcp_context_section());
        values.insert(Placeholder::Memory, self.build_memory_section());
        values.insert(
            Placeholder::SessionContext,
            self.build_session_context_section(),
        );

        // 3. Replace placeholders in template
        replace_placeholders(&template, &values, true)
    }

    /// Build the extension bootstrap context section from SessionStart hooks.
    ///
    /// Returns the trimmed context with a leading header when `session_context`
    /// is set and non-empty; otherwise an empty string so templates that omit
    /// `{{session_context}}` get no section.
    fn build_session_context_section(&self) -> String {
        let Some(context) = self.session_context.as_deref() else {
            return String::new();
        };
        let trimmed = context.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        format!("## Session context\n\n{trimmed}\n")
    }

    /// Build the principal long-term memory section from MEMORY.md.
    ///
    /// Returns the trimmed memory content with a leading header when
    /// `principal_memory` is set and non-empty; otherwise an empty
    /// string so templates that omit `{{memory}}` get no section.
    fn build_memory_section(&self) -> String {
        let Some(memory) = self.principal_memory.as_deref() else {
            return String::new();
        };
        let trimmed = memory.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        format!("## Your long-term memory (MEMORY.md)\n\n{trimmed}\n")
    }

    /// Build the MCP context section via Extension Core hooks
    fn build_mcp_context_section(&self) -> String {
        use crate::extensions::framework::{HookInput, HookPoint};

        if let Some(ref core) = self.extension_core {
            if let Ok(_handle) = tokio::runtime::Handle::try_current() {
                let hook_point = HookPoint::PromptSystemSection {
                    section: "mcp_context".to_string(),
                    priority: 100,
                };
                let core = core.clone();

                let result = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        core.invoke_hook_text(hook_point, HookInput::Unit).await
                    })
                });

                if let Some(text) = result {
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
        }

        String::new()
    }

    /// Build the Available Tools section
    fn build_tools_section(&self) -> String {
        let mut lines: Vec<String> = vec![];

        lines.push("## Available Tools".to_string());

        // Phase 1: Extension Architecture - Query ExtensionCore for tools via hooks
        // This picks up tools registered by BuiltinToolAdapter, MCPAdapter, etc.
        let mut has_extension_tools = false;
        if let Some(ref core) = self.extension_core {
            if let Ok(_handle) = tokio::runtime::Handle::try_current() {
                use crate::extensions::framework::{HookInput, HookPoint};
                let hook_point = HookPoint::PromptSystemSection {
                    section: "tools".to_string(),
                    priority: 100,
                };
                let core = core.clone();

                let result = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        core.invoke_hook_text(hook_point, HookInput::Unit).await
                    })
                });

                if let Some(tools_text) = result {
                    if !tools_text.is_empty() {
                        has_extension_tools = true;
                        lines.push(
                            "You have access to the following tools. Use them wisely.".to_string(),
                        );
                        lines.push(String::new());
                        lines.push(tools_text);
                        lines.push(String::new());
                    }
                }
            }
        }

        // ADR-019 Phase 3: Support both Arc<dyn Tool> and ToolDefinition
        // Prefer tool_definitions if available (dynamic updates), fall back to tools
        // Only used if ExtensionCore didn't provide tools
        let has_tool_defs = !self.tool_definitions.is_empty();
        let has_tools = !self.tools.is_empty();

        if !has_extension_tools {
            if !has_tool_defs && !has_tools {
                lines.push("No tools available.".to_string());
            } else {
                lines.push("You have access to the following tools. Use them wisely.".to_string());
                lines.push(String::new());

                // Use tool_definitions if available (from unified registry)
                if has_tool_defs {
                    for def in &self.tool_definitions {
                        lines.push(format!("### {}", def.name));
                        lines.push(String::new());
                        lines.push(def.description.clone());
                        lines.push(String::new());
                    }
                } else {
                    // Fall back to legacy Tool trait objects
                    for tool in &self.tools {
                        lines.push(format!("### {}", tool.name()));
                        lines.push(String::new());
                        lines.push(tool.description());
                        lines.push(String::new());
                    }
                }
            }
        }

        // Tool Use Guidelines (always add if there are tools)
        if has_extension_tools || has_tool_defs || has_tools {
            lines.push("### Tool Use Guidelines".to_string());
            lines.push(
                "- Think step by step. Use available tools when needed to accomplish tasks."
                    .to_string(),
            );
            lines.push(
                "- Multiple tools can be called in parallel if they are independent.".to_string(),
            );
            lines.push(
                "- When you have the final answer, provide it directly without tool calls."
                    .to_string(),
            );

            // Framework-level reserved parameters available on ALL tools
            lines.push(String::new());
            lines.push("### Tool Timeout and Async Execution".to_string());
            lines.push(
                "All tool calls have a constant 5-minute timeout. If a tool exceeds this timeout, the work is automatically detached to a background task and a receipt is returned.".to_string(),
            );
            lines.push(
                "To invoke a tool explicitly in the background, use the `task` tool with `action=\"spawn\"` and specify the target tool and parameters.".to_string(),
            );
            lines.push(
                "Use the `task` tool with `action=\"output\"` to retrieve the full result of a background task.".to_string(),
            );
            lines.push(String::new());
            lines.push("Example:".to_string());
            lines.push("```json".to_string());
            lines.push(
                r#"{"action": "spawn", "tool": "Agent", "params": {"prompt": "Analyze confidential data", "subagent_type": "researcher"}}"#.to_string(),
            );
            lines.push("```".to_string());
        }

        lines.join("\n")
    }

    /// Build the Skills section via Extension Core hooks
    ///
    /// Uses the `ExtensionCore` hook system to inject skills content from registered
    /// skill extensions. This replaces the legacy `SkillsRegistry` approach.
    fn build_skills_section(&self) -> String {
        use crate::extensions::framework::{HookInput, HookPoint};

        if let Some(ref core) = self.extension_core {
            // Try to invoke skills hooks via ExtensionCore
            // Note: This uses block_in_place + block_on as the builder is currently synchronous
            // but may be called from async contexts. Future phases will make the builder fully async.
            if let Ok(_handle) = tokio::runtime::Handle::try_current() {
                let hook_point = HookPoint::PromptSystemSection {
                    section: "skills".to_string(),
                    priority: 100,
                };
                let core = core.clone();

                let principal_id = self.principal_id.clone();
                let result = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        core.invoke_hook_text_with_principal(
                            hook_point,
                            HookInput::Unit,
                            principal_id.as_deref(),
                        )
                        .await
                    })
                });

                match result {
                    Some(skills_text) if !skills_text.is_empty() => {
                        return format!(
                            r"## Skills (mandatory)
Before replying: scan <available_skills> <description> entries.
- If exactly one skill clearly applies: invoke the `Skill` tool with `name` = the skill name, then follow the returned body.
- If multiple could apply: choose the most specific one, then invoke `Skill` with that name and follow the returned body.
- If none clearly apply: do not invoke any skill.
Constraints: never invoke more than one skill up front; only invoke after selecting.

<available_skills>
{skills_text}
</available_skills>"
                        );
                    }
                    _ => {}
                }
            }
        }

        String::new()
    }

    /// Build the Agents section via Extension Core hooks
    ///
    /// Uses the `ExtensionCore` hook system to inject agent content from registered
    /// agent extensions. This replaces any legacy agent catalog approach.
    fn build_agents_section(&self) -> String {
        use crate::extensions::framework::{HookInput, HookPoint};

        if let Some(ref core) = self.extension_core {
            if let Ok(_handle) = tokio::runtime::Handle::try_current() {
                let hook_point = HookPoint::PromptSystemSection {
                    section: "agents".to_string(),
                    priority: 100,
                };
                let core = core.clone();

                let principal_id = self.principal_id.clone();
                let result = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        core.invoke_hook_text_with_principal(
                            hook_point,
                            HookInput::Unit,
                            principal_id.as_deref(),
                        )
                        .await
                    })
                });

                match result {
                    Some(agents_text) if !agents_text.is_empty() => {
                        return format!(
                            r"## Available Agents
When delegating, choose the most appropriate agent from the list below. Each agent has a name you can pass to the `Agent` tool as `subagent_type`.

<available_agents>
{agents_text}
</available_agents>"
                        );
                    }
                    _ => {}
                }
            }
        }

        String::new()
    }

    /// Build the Runtime section
    fn build_runtime_section(&self) -> String {
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

    /// Build the Sandbox section (conditional)
    fn build_sandbox_section(&self) -> String {
        if self.sandbox_enabled {
            "## Sandbox\nSandbox: enabled\nTools run in isolated environment with restricted access.".to_string()
        } else {
            String::new()
        }
    }

    /// Build the Model Aliases section (conditional)
    fn build_model_aliases_section(&self) -> String {
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

    /// Build the Self-Update section (conditional)
    fn build_self_update_section(&self, is_minimal: bool) -> String {
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
        // Create a template with placeholders
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

        // Check placeholders were replaced
        assert!(prompt.contains("You are test-agent."));
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("## Runtime"));
        assert!(prompt.contains("Agent: test-agent"));

        // Original placeholders should be gone
        assert!(!prompt.contains("{{agent_name}}"));
        assert!(!prompt.contains("{{tools}}"));
    }

    #[test]
    fn test_builder_no_template_fallback() {
        // Empty body → fallback to minimal identity.
        let builder = SystemPromptBuilder::new("test-agent").with_mode(PromptMode::Full);

        let prompt = builder.build();

        assert_eq!(prompt, "You are test-agent.");
    }

    #[test]
    fn test_builder_with_skills_via_extension_core() {
        use crate::extensions::framework::ExtensionManifest;
        use crate::extensions::skill::{register_skills_with_core, DiscoveredSkill};
        use std::path::PathBuf;

        // Create a tokio runtime for async operations
        let rt = tokio::runtime::Runtime::new().unwrap();

        let tmp = TempDir::new().unwrap();
        let template = "{{skills}}";

        // Create ExtensionCore and register skills
        let core = crate::extensions::framework::ExtensionCore::new();

        // Create a test skill using the new Extension system
        let skill = DiscoveredSkill {
            manifest: ExtensionManifest::new(
                "docker",
                "skill",
                "docker",
                "Docker operations",
                "1.0.0",
                PathBuf::from("/tmp/skills/docker"),
            ),
            file_path: PathBuf::from("/tmp/skills/docker/SKILL.md"),
            base_dir: PathBuf::from("/tmp/skills/docker"),
        };

        // Register the skill with the ExtensionCore
        rt.block_on(async {
            register_skills_with_core(&core, vec![skill])
                .await
                .expect("Failed to register skills");

            // Enable the extension for the principal used by the builder.
            crate::principal::ExtensionStateRegistry::global()
                .register(
                    crate::principal::PrincipalId("test-builder".to_string()),
                    crate::principal::ExtensionState::new(
                        vec!["docker".to_string()],
                        tmp.path().to_path_buf(),
                    ),
                )
                .await;
        });

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Full)
            .with_body(template)
            .with_extension_core(Arc::new(core))
            .with_principal_id("test-builder");

        // Build needs to run in a tokio context because build_skills_section uses block_on
        let prompt = rt.block_on(async {
            // Use spawn_blocking to run the synchronous build in an async context
            tokio::task::spawn_blocking(move || builder.build())
                .await
                .unwrap()
        });

        // Should include skills section from ExtensionCore hooks
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("docker: Docker operations"));
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
        // Level defaults to "medium" since with_thinking_level was removed
        assert!(prompt.contains("Level: medium"));
    }

    #[test]
    fn test_minimal_mode_basic() {
        let tmp = TempDir::new().unwrap();
        // Template without conditional sections that minimal mode would skip
        let template = r"## Your Role
You are {{agent_name}}.

{{tools}}

{{runtime}}";

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Minimal)
            .with_body(template);

        let prompt = builder.build();

        // Should still have basic sections
        assert!(prompt.contains("## Your Role"));
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("## Runtime"));
    }

    // test_reserved_params_in_tools_section removed in ADR-019 cleanup:
    // reserved parameter docs are no longer injected into the system prompt
    // because ExtensionCore handles execution control via hooks.

    /// `{{memory}}` placeholder renders the loaded MEMORY.md content
    /// under the standard `## Your long-term memory (MEMORY.md)` header.
    /// This pins down the opt-in wiring so casual users (who use the
    /// default supervisor template) still see their memory without
    /// having to author a custom system prompt.
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
    /// append the memory section unconditionally — that's the whole
    /// point of making it a placeholder. Templates that don't opt in
    /// see no memory content at all, even when memory is loaded.
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
