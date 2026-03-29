//! System prompt builder with multi-section support
//!
//! Matches `OpenClaw`'s section-based prompt assembly

use crate::prompt::bootstrap::{default_workspace_dir, inject_bootstrap_files, BootstrapConfig};
use crate::prompt::placeholder::{Placeholder, replace_placeholders};
use crate::skills::{build_skills_prompt, Skill};
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

impl PromptMode {
    /// Parse from string
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "minimal" => Self::Minimal,
            "none" => Self::None,
            _ => Self::Full,
        }
    }
}

/// System prompt builder
pub struct SystemPromptBuilder {
    mode: PromptMode,
    bootstrap_config: BootstrapConfig,
    tools: Vec<Arc<dyn Tool>>,
    agent_name: String,
    workspace: PathBuf,
    model: String,
    thinking_level: String,
    has_gateway: bool,
    model_aliases: Vec<String>,
    sandbox_enabled: bool,
    channel: String,
    capabilities: Vec<String>,
    skills: Vec<Skill>,
}

impl SystemPromptBuilder {
    pub fn new(agent_name: &str) -> Self {
        Self {
            mode: PromptMode::Full,
            bootstrap_config: BootstrapConfig::default(),
            tools: vec![],
            agent_name: agent_name.to_string(),
            workspace: default_workspace_dir(),
            model: "default".to_string(),
            thinking_level: "medium".to_string(),
            has_gateway: true,
            model_aliases: vec![],
            sandbox_enabled: false,
            channel: "discord".to_string(),
            capabilities: vec![],
            skills: vec![],
        }
    }

    pub fn with_mode(mut self, mode: PromptMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_workspace(mut self, workspace: impl AsRef<std::path::Path>) -> Self {
        self.workspace = workspace.as_ref().to_path_buf();
        self.bootstrap_config.workspace_dir = self.workspace.clone();
        self
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    pub fn with_thinking_level(mut self, level: &str) -> Self {
        self.thinking_level = level.to_string();
        self
    }

    pub fn with_channel(mut self, channel: &str) -> Self {
        self.channel = channel.to_string();
        self
    }

    pub fn with_sandbox(mut self, enabled: bool) -> Self {
        self.sandbox_enabled = enabled;
        self
    }

    pub fn with_model_aliases(mut self, aliases: Vec<String>) -> Self {
        self.model_aliases = aliases;
        self
    }

    pub fn with_skills(mut self, skills: Vec<Skill>) -> Self {
        self.skills = skills;
        self
    }

    /// Set custom bootstrap files to inject (all treated as optional)
    /// 
    /// If `files` is None or empty, uses the default bootstrap file list.
    pub fn with_bootstrap_files(mut self, files: Option<Vec<String>>) -> Self {
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
        values.insert(Placeholder::Timezone, Local::now().format("%:z").to_string());
        
        // Complex section placeholders
        values.insert(Placeholder::Tools, self.build_tools_section());
        values.insert(Placeholder::Skills, self.build_skills_section());
        values.insert(Placeholder::Runtime, self.build_runtime_section());
        values.insert(Placeholder::Sandbox, self.build_sandbox_section());
        values.insert(Placeholder::ModelAliases, self.build_model_aliases_section());
        values.insert(Placeholder::SelfUpdate, self.build_self_update_section(is_minimal));
        
        // 4. Replace placeholders in template
        replace_placeholders(&template, &values, true)
    }
    
    /// Build the Available Tools section
    fn build_tools_section(&self) -> String {
        let mut lines: Vec<String> = vec![];
        
        lines.push("## Available Tools".to_string());
        if self.tools.is_empty() {
            lines.push("No tools available.".to_string());
        } else {
            lines.push("You have access to the following tools. Use them wisely.".to_string());
            lines.push(String::new());
            
            for tool in &self.tools {
                lines.push(format!("### {}", tool.name()));
                lines.push(String::new());
                lines.push(tool.llm_description());
                lines.push(String::new());
            }
            
            lines.push("### Tool Use Guidelines".to_string());
            lines.push("- Think step by step. Use available tools when needed to accomplish tasks.".to_string());
            lines.push("- Multiple tools can be called in parallel if they are independent.".to_string());
            lines.push("- When you have the final answer, provide it directly without tool calls.".to_string());
        }
        
        lines.join("\n")
    }
    
    /// Build the Skills section
    fn build_skills_section(&self) -> String {
        let skill_refs: Vec<&Skill> = self.skills.iter().collect();
        let skills_prompt = build_skills_prompt(&skill_refs);
        if skills_prompt.is_empty() {
            String::new()
        } else {
            skills_prompt
        }
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
    fn test_prompt_mode_from_str() {
        assert_eq!(PromptMode::from_str("full"), PromptMode::Full);
        assert_eq!(PromptMode::from_str("minimal"), PromptMode::Minimal);
        assert_eq!(PromptMode::from_str("none"), PromptMode::None);
        assert_eq!(PromptMode::from_str("invalid"), PromptMode::Full); // Default
    }

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
        let template = r#"## Your Role
You are {{agent_name}}.

{{tools}}

## Safety
Be safe.

{{runtime}}"#;
        std::fs::write(tmp.path().join("AGENTS.md"), template).unwrap();

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
        let builder = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Full);

        let prompt = builder.build();

        // Fallback to minimal when no templates
        assert_eq!(prompt, "You are test-agent.");
    }

    #[test]
    fn test_builder_with_skills() {
        use crate::skills::Skill;
        use std::path::PathBuf;

        let tmp = TempDir::new().unwrap();
        let template = "{{skills}}";
        std::fs::write(tmp.path().join("AGENTS.md"), template).unwrap();

        let skills = vec![
            Skill {
                name: "docker".to_string(),
                description: "Docker operations".to_string(),
                file_path: PathBuf::from("/tmp/skills/docker/SKILL.md"),
                base_dir: PathBuf::from("/tmp/skills/docker"),
                tags: vec![],
                author: None,
            },
        ];

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Full)
            .with_skills(skills);

        let prompt = builder.build();

        // Should include skills section
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("docker: Docker operations"));
    }

    #[test]
    fn test_placeholder_replacement_inline() {
        let tmp = TempDir::new().unwrap();
        let template = r#"Agent: {{agent_name}}
Workspace: {{workspace}}
Channel: {{channel}}
Level: {{thinking_level}}"#;
        std::fs::write(tmp.path().join("AGENTS.md"), template).unwrap();

        let builder = SystemPromptBuilder::new("my-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Full)
            .with_model("k2p5")
            .with_thinking_level("high");

        let prompt = builder.build();

        assert!(prompt.contains("Agent: my-agent"));
        assert!(prompt.contains("Workspace:"));
        assert!(prompt.contains("Channel: discord"));
        assert!(prompt.contains("Level: high"));
    }

    #[test]
    fn test_conditional_sections() {
        let tmp = TempDir::new().unwrap();
        let template = "{{sandbox}}\n{{model_aliases}}\n{{self_update}}";
        std::fs::write(tmp.path().join("AGENTS.md"), template).unwrap();

        // With all conditions enabled
        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_sandbox(true)
            .with_model_aliases(vec!["fast".to_string(), "slow".to_string()]);

        let prompt = builder.build();

        assert!(prompt.contains("## Sandbox"));
        assert!(prompt.contains("Sandbox: enabled"));
        assert!(prompt.contains("## Model Aliases"));
        assert!(prompt.contains("- fast"));
        assert!(prompt.contains("- slow"));
        assert!(prompt.contains("## Self-Update"));
    }

    #[test]
    fn test_conditional_sections_disabled() {
        let tmp = TempDir::new().unwrap();
        let template = "{{sandbox}}\n{{model_aliases}}";
        std::fs::write(tmp.path().join("AGENTS.md"), template).unwrap();

        // With all conditions disabled
        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_sandbox(false);

        let prompt = builder.build();

        // Sections should be empty (placeholders removed with nothing inserted)
        assert!(!prompt.contains("## Sandbox"));
        assert!(!prompt.contains("## Model Aliases"));
    }

    #[test]
    fn test_minimal_mode_basic() {
        let tmp = TempDir::new().unwrap();
        // Template without conditional sections that minimal mode would skip
        let template = r#"## Your Role
You are {{agent_name}}.

{{tools}}

{{runtime}}"#;
        std::fs::write(tmp.path().join("AGENTS.md"), template).unwrap();

        let builder = SystemPromptBuilder::new("test-agent")
            .with_workspace(tmp.path())
            .with_mode(PromptMode::Minimal);

        let prompt = builder.build();

        // Should still have basic sections
        assert!(prompt.contains("## Your Role"));
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("## Runtime"));
    }
}
