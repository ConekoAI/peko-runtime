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
//! This module provides data structures and parsing utilities.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// Re-export Extension Architecture integration
pub use crate::extensions::adapters::skill_adapter::{
    DiscoveredSkill, SkillAdapter, build_skills_prompt as build_discovered_skills_prompt,
    format_skills_for_prompt as format_discovered_skills_prompt,
    load_skills_from_directory, register_skills_with_core,
};

/// A skill is documentation that teaches the LLM how to do something
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Skill name (from frontmatter)
    pub name: String,
    /// Description (from frontmatter)
    pub description: String,
    /// Full path to SKILL.md
    pub file_path: PathBuf,
    /// Skill directory (for relative references)
    pub base_dir: PathBuf,
    /// Optional metadata (tags, author, etc.)
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: Option<String>,
}

/// YAML frontmatter from SKILL.md
#[derive(Debug, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: Option<String>,
}

/// Parse YAML frontmatter from markdown content
/// Format:
/// ---
/// name: skill-name
/// description: What this skill does
/// ---
/// # Rest of content
pub fn parse_frontmatter(content: &str) -> Result<(String, String)> {
    let mut lines = content.lines().peekable();

    // Must start with ---
    match lines.next() {
        Some("---") => {}
        _ => anyhow::bail!("SKILL.md must start with --- frontmatter delimiter"),
    }

    let mut frontmatter_lines = Vec::new();
    let mut found_end = false;

    for line in lines.by_ref() {
        if line == "---" {
            found_end = true;
            break;
        }
        frontmatter_lines.push(line);
    }

    if !found_end {
        anyhow::bail!("Frontmatter must end with ---");
    }

    let body = lines.collect::<Vec<_>>().join("\n");
    Ok((frontmatter_lines.join("\n"), body))
}

/// Format skills for inclusion in system prompt
pub fn format_skills_for_prompt(skills: &[&Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut lines = vec!["<available_skills>".to_string()];

    for skill in skills {
        // Use relative path from home directory to save tokens
        let path_display = compact_skill_path(&skill.file_path);
        lines.push(format!(
            "- {}: {} (location: {})",
            skill.name, skill.description, path_display
        ));
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Build the skills section for system prompt
pub fn build_skills_prompt(skills: &[&Skill]) -> String {
    let skills_block = format_skills_for_prompt(skills);
    if skills_block.is_empty() {
        return String::new();
    }

    format!(
        r"## Skills (mandatory)
Before replying: scan <available_skills> <description> entries.
- If exactly one skill clearly applies: read its SKILL.md at <location> with `read`, then follow it.
- If multiple could apply: choose the most specific one, then read/follow it.
- If none clearly apply: do not read any SKILL.md.
Constraints: never read more than one skill up front; only read after selecting.

{skills_block}"
    )
}

/// Replace home directory with ~ to save tokens
fn compact_skill_path(path: &Path) -> String {
    let path_str = path.to_string_lossy();
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path_str.starts_with(home_str.as_ref()) {
            return path_str.replacen(home_str.as_ref(), "~", 1);
        }
    }
    path_str.to_string()
}

/// Read the full content of a skill file
pub fn read_skill_content(skill: &Skill) -> Result<String> {
    std::fs::read_to_string(&skill.file_path)
        .with_context(|| format!("Failed to read skill content from {:?}", skill.file_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill
tags: [test, example]
author: Test Author
---
# Test Skill

This is the body content.
"#;

        let (frontmatter, body) = parse_frontmatter(content).unwrap();
        assert!(frontmatter.contains("name: test-skill"));
        assert!(frontmatter.contains("description: A test skill"));
        assert!(body.contains("# Test Skill"));
        assert!(body.contains("This is the body content."));
    }

    #[test]
    fn test_parse_frontmatter_missing_delimiter() {
        let content = "name: test\ndescription: Test";
        assert!(parse_frontmatter(content).is_err());
    }

    #[test]
    fn test_parse_frontmatter_missing_end() {
        let content = "---\nname: test\ndescription: Test";
        assert!(parse_frontmatter(content).is_err());
    }

    #[test]
    fn test_format_skills_for_prompt() {
        // Use actual home directory for this test
        let home_dir = dirs::home_dir().expect("Should have home dir");
        let skill_path = home_dir.join(".pekobot/skills/docker/SKILL.md");

        let skill = Skill {
            name: "docker".to_string(),
            description: "Docker container operations".to_string(),
            file_path: skill_path.clone(),
            base_dir: home_dir.join(".pekobot/skills/docker"),
            tags: vec!["devops".to_string()],
            author: Some("Pekora".to_string()),
        };

        let skills = vec![&skill];
        let prompt = format_skills_for_prompt(&skills);

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("</available_skills>"));
        assert!(prompt.contains("docker: Docker container operations"));
        // Should use ~ instead of full home path (path separator may vary by platform)
        // On Windows, the path might have mixed separators after replacement
        let has_tilde_path = prompt.contains("~/.pekobot/skills/docker/SKILL.md")
            || prompt.contains("~\\.pekobot/skills/docker/SKILL.md")
            || prompt.contains("~\\.pekobot\\skills\\docker\\SKILL.md");
        assert!(has_tilde_path, "Expected path with ~, got: {}", prompt);
    }

    #[test]
    fn test_format_skills_empty() {
        let skills: Vec<&Skill> = vec![];
        let prompt = format_skills_for_prompt(&skills);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_build_skills_prompt() {
        let skill = Skill {
            name: "deploy".to_string(),
            description: "Production deployment workflow".to_string(),
            file_path: PathBuf::from("/tmp/skills/deploy/SKILL.md"),
            base_dir: PathBuf::from("/tmp/skills/deploy"),
            tags: vec![],
            author: None,
        };

        let skills = vec![&skill];
        let prompt = build_skills_prompt(&skills);

        assert!(prompt.contains("## Skills (mandatory)"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("deploy: Production deployment workflow"));
    }

    #[test]
    fn test_build_skills_prompt_empty() {
        let skills: Vec<&Skill> = vec![];
        let prompt = build_skills_prompt(&skills);
        assert!(prompt.is_empty());
    }
}
