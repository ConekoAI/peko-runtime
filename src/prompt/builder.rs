//! System prompt builder with multi-section support
//!
//! Matches OpenClaw's section-based prompt assembly

use crate::prompt::bootstrap::{inject_bootstrap_files, BootstrapConfig, default_workspace_dir};
use crate::tools::Tool;
use chrono::Local;
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

/// System prompt section
#[derive(Debug, Clone)]
pub enum PromptSection {
    /// Tool descriptions
    Tooling(String),
    /// Safety guardrails
    Safety(String),
    /// Available skills
    Skills(String),
    /// Self-update instructions
    SelfUpdate(String),
    /// Workspace info
    Workspace(String),
    /// Documentation pointers
    Documentation(String),
    /// Injected bootstrap files
    ProjectContext(String),
    /// Sandbox info
    Sandbox(String),
    /// Date/time
    CurrentDateTime(String),
    /// Reply tag syntax
    ReplyTags(String),
    /// Heartbeat behavior
    Heartbeats(String),
    /// Runtime info
    Runtime(String),
    /// Reasoning level
    Reasoning(String),
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
    
    /// Build the complete system prompt
    pub fn build(self) -> String {
        if self.mode == PromptMode::None {
            return format!("You are {}.", self.agent_name);
        }
        
        let mut prompt = String::new();
        
        // Build sections based on mode
        let sections = self.build_sections();
        
        for section in sections {
            let content = match section {
                PromptSection::Tooling(s) => self.format_section("Tools", &s),
                PromptSection::Safety(s) => self.format_section("Safety", &s),
                PromptSection::Skills(s) if self.mode != PromptMode::Minimal => {
                    self.format_section("Skills", &s)
                }
                PromptSection::SelfUpdate(s) if self.mode != PromptMode::Minimal => {
                    self.format_section("Self-Update", &s)
                }
                PromptSection::Workspace(s) => self.format_section("Workspace", &s),
                PromptSection::Documentation(s) => self.format_section("Documentation", &s),
                PromptSection::ProjectContext(s) => {
                    if self.mode == PromptMode::Minimal {
                        self.format_section("Subagent Context", &s)
                    } else {
                        self.format_section("Project Context", &s)
                    }
                }
                PromptSection::Sandbox(s) => self.format_section("Sandbox", &s),
                PromptSection::CurrentDateTime(s) => self.format_section("Current Date & Time", &s),
                PromptSection::ReplyTags(s) if self.mode != PromptMode::Minimal => {
                    self.format_section("Reply Tags", &s)
                }
                PromptSection::Heartbeats(s) if self.mode != PromptMode::Minimal => {
                    self.format_section("Heartbeats", &s)
                }
                PromptSection::Runtime(s) => self.format_section("Runtime", &s),
                PromptSection::Reasoning(s) => self.format_section("Reasoning", &s),
                _ => continue, // Skip sections filtered by mode
            };
            
            if !content.trim().is_empty() {
                prompt.push_str(&content);
                prompt.push('\n');
            }
        }
        
        prompt.trim().to_string()
    }
    
    fn format_section(&self, title: &str, content: &str) -> String {
        format!("## {}\n{}\n", title, content)
    }
    
    fn build_sections(&self) -> Vec<PromptSection> {
        let mut sections = vec![];
        
        // 1. Tooling
        sections.push(PromptSection::Tooling(self.build_tooling_section()));
        
        // 2. Safety (always included)
        sections.push(PromptSection::Safety(self.build_safety_section()));
        
        // 3. Skills (Full mode only)
        if self.mode == PromptMode::Full {
            sections.push(PromptSection::Skills(self.build_skills_section()));
        }
        
        // 4. Self-Update (Full mode only)
        if self.mode == PromptMode::Full {
            sections.push(PromptSection::SelfUpdate(self.build_self_update_section()));
        }
        
        // 5. Workspace
        sections.push(PromptSection::Workspace(self.build_workspace_section()));
        
        // 6. Documentation
        sections.push(PromptSection::Documentation(self.build_documentation_section()));
        
        // 7. Project Context (bootstrap files)
        sections.push(PromptSection::ProjectContext(self.build_project_context()));
        
        // 8. Sandbox (if enabled)
        sections.push(PromptSection::Sandbox(self.build_sandbox_section()));
        
        // 9. Current Date & Time
        sections.push(PromptSection::CurrentDateTime(self.build_datetime_section()));
        
        // 10. Reply Tags (Full mode only)
        if self.mode == PromptMode::Full {
            sections.push(PromptSection::ReplyTags(self.build_reply_tags_section()));
        }
        
        // 11. Heartbeats (Full mode only)
        if self.mode == PromptMode::Full {
            sections.push(PromptSection::Heartbeats(self.build_heartbeats_section()));
        }
        
        // 12. Runtime
        sections.push(PromptSection::Runtime(self.build_runtime_section()));
        
        // 13. Reasoning
        sections.push(PromptSection::Reasoning(self.build_reasoning_section()));
        
        sections
    }
    
    fn build_tooling_section(&self) -> String {
        if self.tools.is_empty() {
            return "No tools available.".to_string();
        }
        
        let descriptions: Vec<String> = self
            .tools
            .iter()
            .map(|t| format!("- `{}`: {}", t.name(), t.description()))
            .collect();
        
        format!(
            "You have access to the following tools:\n\n{}\n\n\
             When you need to use a tool, respond in this exact format:\n\
             TOOL_CALL: {{\"name\": \"tool_name\", \"parameters\": {{\"key\": \"value\"}}}}\n\n\
             When you have a final answer, respond with:\n\
             FINAL_ANSWER: your answer here",
            descriptions.join("\n")
        )
    }
    
    fn build_safety_section(&self) -> String {
        "Safety guardrails are advisory. They guide behavior but do not enforce policy. \
         Avoid power-seeking behavior or bypassing oversight. \
         Use tool policy and sandboxing for hard enforcement."
            .to_string()
    }
    
    fn build_skills_section(&self) -> String {
        // TODO: Load from skills directory when implemented
        "When eligible skills exist, they will be listed here with their paths. \
         Use `read` to load SKILL.md at the listed location when needed."
            .to_string()
    }
    
    fn build_self_update_section(&self) -> String {
        "To update configuration: modify config files and restart.\n\
         To update Pekobot: run `pekobot system update`."
            .to_string()
    }
    
    fn build_workspace_section(&self) -> String {
        format!("Working directory: `{}`", self.workspace.display())
    }
    
    fn build_documentation_section(&self) -> String {
        "Pekobot documentation is available in the workspace.\n\
         Tools are discovered dynamically from the tool registry."
            .to_string()
    }
    
    fn build_project_context(&self) -> String {
        let injected = inject_bootstrap_files(&self.bootstrap_config);
        
        if injected.sections.is_empty() {
            return "<!-- No bootstrap files found. Create AGENTS.md, SOUL.md, etc. in your workspace. -->".to_string();
        }
        
        let mut context = String::new();
        for section in injected.sections {
            context.push_str(&format!(
                "### {} (from `{}`){}\n\n{}\n\n",
                section.name,
                section.source_file,
                if section.truncated { " [truncated]" } else { "" },
                section.content
            ));
        }
        
        context
    }
    
    fn build_sandbox_section(&self) -> String {
        // TODO: Check if sandbox is enabled from config
        "Sandbox: not enabled\n\
         Tools run with full host access."
            .to_string()
    }
    
    fn build_datetime_section(&self) -> String {
        let now = Local::now();
        format!(
            "Timezone: {} ({})",
            now.format("%Z"),
            now.format("%:z")
        )
    }
    
    fn build_reply_tags_section(&self) -> String {
        "Reply tags: `[[reply_to_current]]` or `[[reply_to:<id>]]`\n\
         Use these to request specific reply behavior."
            .to_string()
    }
    
    fn build_heartbeats_section(&self) -> String {
        "Heartbeat prompt: Read HEARTBEAT.md and follow instructions.\n\
         Ack with HEARTBEAT_OK when nothing needs attention.\n\
         Write durable memories before compaction."
            .to_string()
    }
    
    fn build_runtime_section(&self) -> String {
        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown".to_string());
        
        format!(
            "Agent: {}\n\
             Host: {}\n\
             OS: {}\n\
             Model: {}",
            self.agent_name,
            hostname,
            std::env::consts::OS,
            self.model
        )
    }
    
    fn build_reasoning_section(&self) -> String {
        format!(
            "Thinking level: {}\n\
             Toggle visibility with /reasoning command.",
            self.thinking_level
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_prompt_mode_from_str() {
        assert_eq!(PromptMode::from_str("full"), PromptMode::Full);
        assert_eq!(PromptMode::from_str("minimal"), PromptMode::Minimal);
        assert_eq!(PromptMode::from_str("none"), PromptMode::None);
        assert_eq!(PromptMode::from_str("invalid"), PromptMode::Full); // Default
    }
    
    #[test]
    fn test_builder_basic() {
        let builder = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::None);
        
        let prompt = builder.build();
        assert_eq!(prompt, "You are test-agent.");
    }
    
    #[test]
    fn test_builder_full_mode_has_sections() {
        let builder = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Full);
        
        let prompt = builder.build();
        
        // Check for section headers
        assert!(prompt.contains("## Tools"));
        assert!(prompt.contains("## Safety"));
        assert!(prompt.contains("## Workspace"));
        assert!(prompt.contains("## Runtime"));
        assert!(prompt.contains("## Reasoning"));
        
        // Check for content
        assert!(prompt.contains("test-agent"));
        assert!(prompt.contains("OS:"));
    }
    
    #[test]
    fn test_builder_minimal_mode_omits_sections() {
        let builder = SystemPromptBuilder::new("test-agent")
            .with_mode(PromptMode::Minimal);
        
        let prompt = builder.build();
        
        // Should have core sections
        assert!(prompt.contains("## Tools"));
        assert!(prompt.contains("## Safety"));
        
        // Should NOT have these in minimal mode
        assert!(!prompt.contains("## Skills"));
        assert!(!prompt.contains("## Heartbeats"));
        
        // Should have Subagent Context instead of Project Context
        assert!(prompt.contains("## Subagent Context") || prompt.contains("## Project Context"));
    }
}
