//! Skills system for agents
//!
//! Skills are documentation that teach the LLM how to perform specific tasks.
//! Each skill is a markdown file (SKILL.md) with YAML frontmatter containing
//! metadata like name and description.
//!
//! The LLM uses skills by:
//! 1. Seeing available skills in the system prompt
//! 2. Deciding which skill applies to the current task
//! 3. Using the `read` tool to fetch the full SKILL.md content
//! 4. Following the instructions in the skill using existing tools (exec, etc.)
//!
//! Skills format follows the Anthropic Skills specification:
//! https://github.com/anthropics/skills
//!
//! # Extension Architecture Integration
//!
//! Skills are now managed through the Extension system via ExtensionManager.
//! This module re-exports the Extension Architecture integration.

// Re-export Extension Architecture integration
pub use crate::extensions::adapters::skill_adapter::{
    DiscoveredSkill, SkillAdapter, build_skills_prompt,
    format_skills_for_prompt, load_skills_from_directory, register_skills_with_core,
};

// Re-export parsing utilities
pub use crate::extensions::adapters::parsing::parse_yaml_frontmatter as parse_frontmatter;
