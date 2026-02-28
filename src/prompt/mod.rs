//! System prompt generation and management
//!
//! Provides OpenClaw-compatible prompt assembly with:
//! - Bootstrap file injection (AGENTS.md, SOUL.md, etc.)
//! - Multi-section prompt building
//! - Prompt modes (full, minimal, none)

pub mod bootstrap;
pub mod builder;

pub use bootstrap::default_workspace_dir;
pub use builder::{SystemPromptBuilder, PromptMode};

use std::path::PathBuf;

/// Get the default prompt configuration
pub fn default_prompt_config(agent_name: &str) -> builder::SystemPromptBuilder {
    builder::SystemPromptBuilder::new(agent_name)
}

/// Build a prompt for a sub-agent (minimal mode)
pub fn build_subagent_prompt(agent_name: &str, workspace: PathBuf) -> String {
    builder::SystemPromptBuilder::new(agent_name)
        .with_mode(builder::PromptMode::Minimal)
        .with_workspace(workspace)
        .build()
}

/// Build a prompt for main session (full mode)
pub fn build_main_prompt(
    agent_name: &str,
    workspace: PathBuf,
    tools: Vec<std::sync::Arc<dyn crate::tools::Tool>>,
) -> String {
    builder::SystemPromptBuilder::new(agent_name)
        .with_mode(builder::PromptMode::Full)
        .with_workspace(workspace)
        .with_tools(tools)
        .build()
}
