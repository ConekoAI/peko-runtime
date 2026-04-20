//! System prompt builder with multi-section support
//!
//! Matches `OpenClaw`'s section-based prompt assembly

use crate::prompt::bootstrap::{default_workspace_dir, inject_bootstrap_files, BootstrapConfig};
use crate::prompt::placeholder::{replace_placeholders, Placeholder};
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
    bootstrap_config: BootstrapConfig,
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
    extension_core: Option<Arc<crate::extensions::ExtensionCore>>,
}

impl SystemPromptBuilder {
    pub fn new(agent_name: &str) -> Self {
        Self {
            mode: PromptMode::Full,
            bootstrap_config: BootstrapConfig::default(),
            tools: vec![],
            tool_definitions: vec![],
            agent_name: agent_name.to_string(),
            workspace: default_workspace_dir(),
            model: "default".to_string(),
            thinking_level: "medium".to_string(),
            has_gateway: true,
            model_aliases: vec![],
            sandbox_enabled: false,
            channel: "discord".to_string(),
            extension_core: None,
        }
    }

    /// Set the extension core for hook integration
    ///
    /// This enables extensions to inject content into prompt sections.
    pub fn with_extension_core(mut self, core: Arc<crate::extensions::ExtensionCore>) -> Self {
        self.extension_core = Some(core);
        self
    }

    pub fn with_mode(mut self, mode: PromptMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set tool definitions from unified registry (ADR-019 Phase 3)
    ///
    /// This allows building prompts with `ToolDefinition` instead of Arc<dyn Tool>,
    /// enabling dynamic tool updates without session restart.
    pub fn with_tool_definitions(mut self, definitions: Vec<ToolDefinition>) -> Self {
        self.tool_definitions = definitions;
        self
    }

    pub fn with_workspace(mut self, workspace: impl AsRef<std::path::Path>) -> Self {
        self.workspace = workspace.as_ref().to_path_buf();
        self.bootstrap_config.workspace_dir = self.workspace.clone();
        self
    }

    /// Set custom system files to inject (all treated as optional)
    ///
    /// If `files` is None, no files will be loaded (empty system prompt).
    /// Use `BootstrapConfig::with_default_files()` explicitly if you want defaults.
    pub fn with_system_files(mut self, files: Option<Vec<String>>) -> Self {
        self.bootstrap_config = BootstrapConfig::with_files(files, self.workspace.clone());
        self
    }

    /// Build the complete system prompt from templates with placeholder replacement
    pub fn build(self) -> String {
        if self.mode == PromptMode::None {
            return format!("You are {}.", self.agent_name);
        }

        let is_minimal = self.mode == PromptMode::Minimal;

        // 1. Load all bootstrap files (templates)
        let injected = inject_bootstrap_files(&self.bootstrap_config);

        // 2. Concatenate all template content (skip missing file placeholders)
        let mut template = String::new();
        for section in &injected.sections {
            // Skip "file not found" placeholder comments
            if section.content.starts_with("<!--") && section.content.contains("file not found") {
                continue;
            }
            if !template.is_empty() {
                template.push_str("\n\n");
            }
            template.push_str(&section.content);
        }

        // If no templates loaded, fall back to minimal default
        if template.trim().is_empty() {
            return format!("You are {}.", self.agent_name);
        }

        // 3. Build placeholder values
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

        // 4. Replace placeholders in template
        replace_placeholders(&template, &values, true)
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
                use crate::extensions::{HookInput, HookPoint};
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
        }

        lines.join("\n")
    }

    /// Build the Skills section via Extension Core hooks
    ///
    /// Uses the `ExtensionCore` hook system to inject skills content from registered
    /// skill extensions. This replaces the legacy `SkillsRegistry` approach.
    fn build_skills_section(&self) -> String {
        use crate::extensions::{HookInput, HookPoint};

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

                let result = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        core.invoke_hook_text(hook_point, HookInput::Unit).await
                    })
                });

                match result {
                    Some(skills_text) if !skills_text.is_empty() => {
                        return format!(
                            r"## Skills (mandatory)
Before replying: scan <available_skills> <description> entries.
- If exactly one skill clearly applies: read its SKILL.md at <location> with `read`, then follow it.
- If multiple could apply: choose the most specific one, then read/follow it.
- If none clearly apply: do not read any SKILL.md.
Constraints: never read more than one skill up front; only read after selecting.

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
        let tmp = TempDir::new().unwrap();

        // Create a template with placeholders
        let template = r"## Your Role
You are {{agent_name}}.

{{tools}}

## Safety
Be safe.

{{runtime}}";
        std::fs::write(tmp.path().join("SYSTEM.md"), template).unwrap();

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
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
        // When no templates exist, should fallback to minimal
        let builder = SystemPromptBuilder::new("test-agent").with_mode(PromptMode::Full);

        let prompt = builder.build();

        // Fallback to minimal when no templates
        assert_eq!(prompt, "You are test-agent.");
    }

    #[test]
    fn test_builder_with_skills_via_extension_core() {
        use crate::extensions::adapters::skill_adapter::{
            register_skills_with_core, DiscoveredSkill,
        };
        use crate::extensions::ExtensionManifest;
        use std::path::PathBuf;

        // Create a tokio runtime for async operations
        let rt = tokio::runtime::Runtime::new().unwrap();

        let tmp = TempDir::new().unwrap();
        let template = "{{skills}}";
        std::fs::write(tmp.path().join("SYSTEM.md"), template).unwrap();

        // Create ExtensionCore and register skills
        let core = crate::extensions::ExtensionCore::new();

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
        });

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Full)
            .with_extension_core(Arc::new(core));

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
        std::fs::write(tmp.path().join("SYSTEM.md"), template).unwrap();

        let builder = SystemPromptBuilder::new("my-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Full);

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
        std::fs::write(tmp.path().join("SYSTEM.md"), template).unwrap();

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Minimal);

        let prompt = builder.build();

        // Should still have basic sections
        assert!(prompt.contains("## Your Role"));
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("## Runtime"));
    }

    // test_reserved_params_in_tools_section removed in ADR-019 cleanup:
    // reserved parameter docs are no longer injected into the system prompt
    // because ExtensionCore handles execution control via hooks.
}
