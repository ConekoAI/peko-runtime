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
//! # Extension Architecture Integration (Phase 2)
//!
//! This module now supports dual-mode operation:
//! - Legacy mode: Direct skill loading via SkillsRegistry
//! - Extension mode: Registration with ExtensionCore for unified management
//!
//! The Extension mode is the preferred approach for new code.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

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
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    author: Option<String>,
}

/// Skills registry - manages loading and accessing skills
pub struct SkillsRegistry {
    skills: HashMap<String, Skill>,
    skills_dir: PathBuf,
}

impl SkillsRegistry {
    /// Create a new skills registry
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            skills: HashMap::new(),
            skills_dir: skills_dir.into(),
        }
    }

    /// Load all skills from the skills directory
    pub fn load_all(&mut self) -> Result<usize> {
        if !self.skills_dir.exists() {
            info!("Skills directory does not exist: {:?}", self.skills_dir);
            return Ok(0);
        }

        let entries = std::fs::read_dir(&self.skills_dir)
            .with_context(|| format!("Failed to read skills directory: {:?}", self.skills_dir))?;

        let mut count = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Only support SKILL.md format
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                match self.load_skill(&skill_md) {
                    Ok(skill) => {
                        info!("Loaded skill: {}", skill.name);
                        self.skills.insert(skill.name.clone(), skill);
                        count += 1;
                    }
                    Err(e) => {
                        warn!("Failed to load skill from {:?}: {}", skill_md, e);
                    }
                }
            }
        }

        info!("Loaded {} skills from {:?}", count, self.skills_dir);
        Ok(count)
    }

    /// Load a skill from a SKILL.md file
    fn load_skill(&self, path: &Path) -> Result<Skill> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;

        // Parse YAML frontmatter between --- markers
        let (frontmatter, _body) = parse_frontmatter(&content)
            .with_context(|| format!("Failed to parse frontmatter in {path:?}"))?;

        let meta: SkillFrontmatter = serde_yaml::from_str(&frontmatter)
            .with_context(|| format!("Failed to parse YAML frontmatter in {path:?}"))?;

        // Validate required fields
        if meta.name.is_empty() {
            anyhow::bail!("Skill name cannot be empty");
        }
        if meta.description.is_empty() {
            anyhow::bail!("Skill description cannot be empty");
        }

        Ok(Skill {
            name: meta.name,
            description: meta.description,
            file_path: path.to_path_buf(),
            base_dir: path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            tags: meta.tags,
            author: meta.author,
        })
    }

    /// Register all loaded skills with ExtensionCore (Phase 2: Extension Architecture)
    ///
    /// This method migrates skills from legacy mode to Extension mode, allowing
    /// them to be managed through the unified Extension system.
    ///
    /// # Arguments
    /// * `core` - The ExtensionCore to register with
    ///
    /// # Returns
    /// Vector of hook IDs for the registered skills
    ///
    /// # Example
    /// ```rust,ignore
    /// let mut registry = SkillsRegistry::new("~/.pekobot/skills");
    /// registry.load_all()?;
    ///
    /// let core = ExtensionCore::new();
    /// let hook_ids = registry.register_with_extension_core(&core).await?;
    /// ```
    pub async fn register_with_extension_core(
        &self,
        core: &crate::extensions::ExtensionCore,
    ) -> Result<Vec<crate::extensions::HookId>> {
        let discovered: Vec<DiscoveredSkill> = self
            .skills
            .values()
            .map(|skill| DiscoveredSkill {
                manifest: crate::extensions::types::ExtensionManifest::new(
                    &skill.name,
                    "skill",
                    &skill.name,
                    &skill.description,
                    "1.0.0",
                    skill.base_dir.clone(),
                ),
                file_path: skill.file_path.clone(),
                base_dir: skill.base_dir.clone(),
            })
            .collect();

        register_skills_with_core(core, discovered).await
    }

    /// Get a skill by name
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Get all loaded skills sorted by name
    #[must_use]
    pub fn list(&self) -> Vec<&Skill> {
        let mut skills: Vec<_> = self.skills.values().collect();
        skills.sort_by_key(|s| &s.name);
        skills
    }

    /// Get skills by tag
    #[must_use]
    pub fn find_by_tag(&self, tag: &str) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|s| s.tags.contains(&tag.to_string()))
            .collect()
    }

    /// Check if a skill is loaded
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.skills.contains_key(name)
    }

    /// Get the skills directory path
    #[must_use]
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Get the number of loaded skills
    #[must_use]
    pub fn count(&self) -> usize {
        self.skills.len()
    }
}

/// Parse YAML frontmatter from markdown content
/// Format:
/// ---
/// name: skill-name
/// description: What this skill does
/// ---
/// # Rest of content
fn parse_frontmatter(content: &str) -> Result<(String, String)> {
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
/// Matches `OpenClaw`'s formatSkillsForPrompt
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

/// Convenience function to load and register skills from a directory (Phase 2)
///
/// This is a one-step function to migrate from legacy skill loading to
/// Extension Architecture.
///
/// # Arguments
/// * `core` - The ExtensionCore to register with
/// * `skills_dir` - Path to the skills directory
///
/// # Returns
/// Number of skills registered
///
/// # Example
/// ```rust,ignore
/// let core = Arc::new(ExtensionCore::new());
/// let count = load_and_register_skills(&core, "~/.pekobot/skills").await?;
/// println!("Registered {} skills", count);
/// ```
pub async fn load_and_register_skills(
    core: &crate::extensions::ExtensionCore,
    skills_dir: impl Into<PathBuf>,
) -> Result<usize> {
    let mut registry = SkillsRegistry::new(skills_dir);
    registry.load_all()?;

    let hook_ids = registry.register_with_extension_core(core).await?;
    Ok(hook_ids.len())
}

/// Build skills prompt using ExtensionCore hooks (Phase 2)
///
/// This function uses the ExtensionCore to collect skill contributions
/// from registered handlers, providing a migration path from legacy
/// skill formatting.
///
/// If no ExtensionCore is provided, falls back to legacy behavior.
pub async fn build_skills_prompt_with_core(
    core: Option<&crate::extensions::ExtensionCore>,
    legacy_skills: &[&Skill],
) -> String {
    // Try ExtensionCore first
    if let Some(core) = core {
        let result = core
            .invoke_hook(
                crate::extensions::HookPoint::PromptSystemSection {
                    section: "skills".to_string(),
                    priority: 100,
                },
                crate::extensions::HookInput::Unit,
            )
            .await;

        // If we got text output from hooks, format it
        if let crate::extensions::HookResult::Continue(crate::extensions::HookOutput::Text(text)) =
            result
        {
            if !text.is_empty() {
                return format!(
                    r"## Skills (mandatory)
Before replying: scan <available_skills> <description> entries.
- If exactly one skill clearly applies: read its SKILL.md at <location> with `read`, then follow it.
- If multiple could apply: choose the most specific one, then read/follow it.
- If none clearly apply: do not read any SKILL.md.
Constraints: never read more than one skill up front; only read after selecting.

<available_skills>
{text}
</available_skills>"
                );
            }
        }
    }

    // Fallback to legacy behavior
    let skill_refs: Vec<&Skill> = legacy_skills.iter().copied().collect();
    build_skills_prompt(&skill_refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

    #[test]
    fn test_skills_registry_load() {
        let temp = TempDir::new().unwrap();
        let skill_dir = temp.path().join("test-skill");
        std::fs::create_dir(&skill_dir).unwrap();

        let skill_md = r#"---
name: test-skill
description: A test skill for unit testing
tags: [test, unit]
author: Test Suite
---

# Test Skill

This is a test skill used for unit testing.

## Usage

Run the tests with `cargo test`.
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let mut registry = SkillsRegistry::new(temp.path());
        let count = registry.load_all().unwrap();

        assert_eq!(count, 1);
        assert!(registry.has("test-skill"));

        let skill = registry.get("test-skill").unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "A test skill for unit testing");
        assert_eq!(skill.tags, vec!["test", "unit"]);
        assert_eq!(skill.author, Some("Test Suite".to_string()));
    }

    #[test]
    fn test_skills_registry_list_sorted() {
        let temp = TempDir::new().unwrap();

        for name in ["zebra", "alpha", "mike"] {
            let skill_dir = temp.path().join(name);
            std::fs::create_dir(&skill_dir).unwrap();
            let skill_md = format!(
                r#"---
name: {}
description: Test skill {}
---
"#,
                name, name
            );
            std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
        }

        let mut registry = SkillsRegistry::new(temp.path());
        registry.load_all().unwrap();

        let skills = registry.list();
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "zebra"]);
    }

    #[test]
    fn test_skills_registry_find_by_tag() {
        let temp = TempDir::new().unwrap();

        let skill1_dir = temp.path().join("skill1");
        std::fs::create_dir(&skill1_dir).unwrap();
        std::fs::write(
            skill1_dir.join("SKILL.md"),
            r#"---
name: skill1
description: Test
tags: [utility]
---
"#,
        )
        .unwrap();

        let skill2_dir = temp.path().join("skill2");
        std::fs::create_dir(&skill2_dir).unwrap();
        std::fs::write(
            skill2_dir.join("SKILL.md"),
            r#"---
name: skill2
description: Test
tags: [utility, network]
---
"#,
        )
        .unwrap();

        let mut registry = SkillsRegistry::new(temp.path());
        registry.load_all().unwrap();

        let utility_skills = registry.find_by_tag("utility");
        assert_eq!(utility_skills.len(), 2);

        let network_skills = registry.find_by_tag("network");
        assert_eq!(network_skills.len(), 1);
    }

    #[test]
    fn test_read_skill_content() {
        let temp = TempDir::new().unwrap();
        let skill_dir = temp.path().join("test-skill");
        std::fs::create_dir(&skill_dir).unwrap();

        let content = r#"---
name: test-skill
description: Test
---
# Full Content

This is the complete skill documentation.
"#;
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();

        let mut registry = SkillsRegistry::new(temp.path());
        registry.load_all().unwrap();

        let skill = registry.get("test-skill").unwrap();
        let full_content = read_skill_content(skill).unwrap();

        assert!(full_content.contains("# Full Content"));
        assert!(full_content.contains("This is the complete skill documentation."));
    }

    #[test]
    fn test_skills_registry_ignores_toml() {
        // TOML files should be ignored - only SKILL.md is supported
        let temp = TempDir::new().unwrap();
        let skill_dir = temp.path().join("legacy-skill");
        std::fs::create_dir(&skill_dir).unwrap();

        let skill_toml = r#"
[skill]
name = "legacy-skill"
description = "A legacy TOML skill"
version = "1.0.0"
author = "Legacy Author"
tags = ["legacy"]
"#;
        std::fs::write(skill_dir.join("SKILL.toml"), skill_toml).unwrap();

        let mut registry = SkillsRegistry::new(temp.path());
        let count = registry.load_all().unwrap();

        // Should NOT load TOML skills
        assert_eq!(count, 0);
        assert!(!registry.has("legacy-skill"));
    }
}
