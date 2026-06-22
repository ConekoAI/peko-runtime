//! Skill Adapter for the Extension system
//!
//! This adapter enables skills (SKILL.md files with YAML frontmatter) to be
//! managed through the unified Extension Architecture.
//!
//! # Skill Format
//!
//! Skills are markdown files with YAML frontmatter:
//! ```markdown
//! ---
//! name: docker
//! description: Docker container operations
//! tags: [devops, containers]
//! author: Pekora
//! ---
//!
//! # Docker Skill
//!
//! Instructions for the LLM...
//! ```
//!
//! # Hook Points
//!
//! Skills hook into:
//! - `PromptSystemSection { section: "skills" }` - Injects available skills into system prompt

use crate::extensions::framework::adapters::parsing;
use crate::extensions::framework::adapters::{ExtensionTypeAdapter, ManifestFormat};
#[cfg(test)]
use crate::extensions::framework::core::ExtensionServices;
use crate::extensions::framework::core::{
    HookBinding, HookContext, HookHandler, HookHandlerFactory, HookPoint,
};
use crate::extensions::framework::types::{ExtensionId, ExtensionManifest, HookId, HookOutput, HookResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Skill extension type identifier
pub const SKILL_EXTENSION_TYPE: &str = "skill";

/// Default priority for skill prompt injection
pub const SKILL_HOOK_PRIORITY: i32 = 100;

/// Skill adapter for Extension system
#[derive(Debug)]
pub struct SkillAdapter;

impl SkillAdapter {
    /// Create a new skill adapter
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Discover skills from a directory
    pub fn discover_skills(&self, path: &Path) -> Vec<DiscoveredSkill> {
        let mut skills = Vec::new();

        if !path.exists() {
            debug!("Skills directory does not exist: {:?}", path);
            return skills;
        }

        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read skills directory {:?}: {}", path, e);
                return skills;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                match self.parse_skill_manifest(&skill_md) {
                    Ok(manifest) => {
                        skills.push(DiscoveredSkill {
                            manifest,
                            file_path: skill_md,
                            base_dir: path,
                        });
                    }
                    Err(e) => {
                        warn!("Failed to parse skill from {:?}: {}", skill_md, e);
                    }
                }
            }
        }

        skills
    }

    /// Parse a SKILL.md file into an extension manifest
    fn parse_skill_manifest(&self, path: &Path) -> Result<ExtensionManifest> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;

        // Use shared parsing utility
        let (meta, _body): (SkillFrontmatter, _) = parsing::parse_yaml_frontmatter_typed(&content)
            .with_context(|| format!("Failed to parse frontmatter in {path:?}"))?;

        if meta.name.is_empty() {
            anyhow::bail!("Skill name cannot be empty");
        }
        if meta.description.is_empty() {
            anyhow::bail!("Skill description cannot be empty");
        }

        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut manifest = ExtensionManifest::new(
            &meta.name,
            SKILL_EXTENSION_TYPE,
            &meta.name,
            &meta.description,
            "1.0.0", // Skills don't have explicit versioning yet
            base_dir,
        );

        // Store additional metadata
        manifest.set("tags", meta.tags);
        manifest.set("author", meta.author.unwrap_or_default());
        manifest.set("skill_file", path.to_string_lossy().to_string());

        Ok(manifest)
    }
}

impl Default for SkillAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtensionTypeAdapter for SkillAdapter {
    fn extension_type(&self) -> &'static str {
        SKILL_EXTENSION_TYPE
    }

    fn manifest_format(&self) -> ManifestFormat {
        ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["name", "description"],
            file_name: "SKILL.md",
        }
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        // Skills only hook into the skills section of the system prompt
        vec![HookBinding::new(
            HookPoint::PromptSystemSection {
                section: "skills".to_string(),
                priority: SKILL_HOOK_PRIORITY,
            },
            Box::new(SkillPromptHandlerFactory {
                manifest: manifest.clone(),
            }),
        )]
    }

    fn parse_manifest(
        &self,
        path: &Path,
        content: &str,
    ) -> anyhow::Result<crate::extensions::framework::ExtensionManifest> {
        // Use shared parsing utility
        let (skill_frontmatter, _): (SkillFrontmatter, _) =
            parsing::parse_yaml_frontmatter_typed(content)
                .with_context(|| format!("Failed to parse SKILL.md frontmatter in {path:?}"))?;

        // Convert to ExtensionManifest
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut manifest = ExtensionManifest::new(
            &skill_frontmatter.name,
            SKILL_EXTENSION_TYPE,
            &skill_frontmatter.name,
            &skill_frontmatter.description,
            "1.0.0", // Skills don't have versions in their frontmatter, use default
            base_dir.to_path_buf(),
        );

        // Store additional metadata
        manifest.set("skill_file", path.to_string_lossy().to_string());
        manifest.set("tags", skill_frontmatter.tags);
        if let Some(author) = skill_frontmatter.author {
            manifest.set("author", author);
        }

        Ok(manifest)
    }
}

/// A discovered skill before registration
#[derive(Debug, Clone)]
pub struct DiscoveredSkill {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Full path to SKILL.md
    pub file_path: PathBuf,
    /// Skill base directory
    pub base_dir: PathBuf,
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

/// Factory for creating skill prompt handlers
#[derive(Debug, Clone)]
struct SkillPromptHandlerFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for SkillPromptHandlerFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(SkillPromptHandler {
            skill_name: self.manifest.name.clone(),
            description: self.manifest.description.clone(),
            file_path: PathBuf::from(
                self.manifest
                    .get("skill_file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
        })
    }
}

/// Handler that injects skill into prompt
#[derive(Debug, Clone)]
struct SkillPromptHandler {
    skill_name: String,
    description: String,
    file_path: PathBuf,
}

#[async_trait]
impl HookHandler for SkillPromptHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        // Format the skill entry for the prompt
        // Use full path (not compacted with ~) so the agent can read the file
        let path_display = self.file_path.to_string_lossy();
        let text = format!(
            "- {}: {} (location: {})",
            self.skill_name, self.description, path_display
        );

        HookResult::Continue(HookOutput::Text(text))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "skills".to_string(),
            priority: SKILL_HOOK_PRIORITY,
        }
    }

    fn priority(&self) -> i32 {
        SKILL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("SkillPromptHandler({})", self.skill_name)
    }
}

// parse_frontmatter now uses parsing::parse_yaml_frontmatter from shared utilities

/// Replace home directory with ~ to save tokens
#[cfg(test)]
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
pub fn read_skill_content(skill_path: &Path) -> Result<String> {
    std::fs::read_to_string(skill_path)
        .with_context(|| format!("Failed to read skill content from {skill_path:?}"))
}

/// Format skills for inclusion in system prompt
/// Matches `OpenClaw`'s formatSkillsForPrompt
#[must_use]
pub fn format_skills_for_prompt(skills: &[&DiscoveredSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut lines = vec!["<available_skills>".to_string()];

    for skill in skills {
        // Use full path (not compacted with ~) so the agent can read the file
        let path_display = skill.file_path.to_string_lossy();
        lines.push(format!(
            "- {}: {} (location: {})",
            skill.manifest.name, skill.manifest.description, path_display
        ));
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Build the complete skills section for system prompt
#[must_use]
pub fn build_skills_prompt(skills: &[&DiscoveredSkill]) -> String {
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

/// Helper to load skills from directory using the adapter
#[must_use]
pub fn load_skills_from_directory(path: &Path) -> Vec<DiscoveredSkill> {
    let adapter = SkillAdapter::new();
    adapter.discover_skills(path)
}

/// Register skills with an `ExtensionCore`
pub async fn register_skills_with_core(
    core: &crate::extensions::framework::ExtensionCore,
    skills: Vec<DiscoveredSkill>,
) -> Result<Vec<HookId>> {
    let mut hook_ids = Vec::new();

    for skill in skills {
        let extension_id = ExtensionId::new(&skill.manifest.id.0);

        // Create handler directly
        let handler = Arc::new(SkillPromptHandler {
            skill_name: skill.manifest.name.clone(),
            description: skill.manifest.description.clone(),
            file_path: skill.file_path,
        });

        let registration = core
            .register_hook(
                HookPoint::PromptSystemSection {
                    section: "skills".to_string(),
                    priority: SKILL_HOOK_PRIORITY,
                },
                handler,
                &extension_id,
            )
            .await?;

        hook_ids.push(registration.id);
        info!(
            skill_name = %skill.manifest.name,
            hook_id = %registration.id,
            "Registered skill with ExtensionCore"
        );
    }

    Ok(hook_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_skill(dir: &Path, name: &str, description: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        std::fs::create_dir(&skill_dir).unwrap();

        let content = format!(
            r"---
name: {name}
description: {description}
tags: [test]
author: Test Author
---

# Test Skill

This is a test skill.
"
        );

        let skill_md = skill_dir.join("SKILL.md");
        std::fs::write(&skill_md, content).unwrap();
        skill_md
    }

    #[test]
    fn test_skill_adapter_manifest_format() {
        let adapter = SkillAdapter::new();
        let format = adapter.manifest_format();

        assert!(matches!(
            format,
            ManifestFormat::YamlFrontmatterMarkdown { .. }
        ));
    }

    #[test]
    fn test_discover_skills() {
        let temp = TempDir::new().unwrap();

        create_test_skill(temp.path(), "skill1", "First skill");
        create_test_skill(temp.path(), "skill2", "Second skill");

        let adapter = SkillAdapter::new();
        let skills = adapter.discover_skills(temp.path());

        assert_eq!(skills.len(), 2);
        assert!(skills.iter().any(|s| s.manifest.name == "skill1"));
        assert!(skills.iter().any(|s| s.manifest.name == "skill2"));
    }

    #[test]
    fn test_parse_skill_manifest() {
        let temp = TempDir::new().unwrap();
        let skill_md = create_test_skill(temp.path(), "docker", "Docker operations");

        let adapter = SkillAdapter::new();
        let manifest = adapter.parse_skill_manifest(&skill_md).unwrap();

        assert_eq!(manifest.name, "docker");
        assert_eq!(manifest.description, "Docker operations");
        assert_eq!(manifest.extension_type, "skill");
        assert!(manifest.get("tags").is_some());
    }

    #[test]
    fn test_parse_frontmatter() {
        let content = r"---
name: test-skill
description: A test skill
tags: [test, example]
author: Test Author
---

# Test Skill

This is the body content.
";

        let (frontmatter, body) = parsing::parse_yaml_frontmatter(content).unwrap();
        assert!(frontmatter.contains("name: test-skill"));
        assert!(frontmatter.contains("description: A test skill"));
        assert!(body.contains("# Test Skill"));
    }

    #[test]
    fn test_format_skills_for_prompt() {
        let temp = TempDir::new().unwrap();
        let skill_md = create_test_skill(temp.path(), "docker", "Docker operations");

        let skill = DiscoveredSkill {
            manifest: ExtensionManifest::new(
                "docker",
                "skill",
                "docker",
                "Docker operations",
                "1.0.0",
                skill_md.parent().unwrap().to_path_buf(),
            ),
            file_path: skill_md,
            base_dir: temp.path().join("docker"),
        };

        let skills = vec![&skill];
        let prompt = format_skills_for_prompt(&skills);

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("docker: Docker operations"));
    }

    #[test]
    fn test_build_skills_prompt() {
        let temp = TempDir::new().unwrap();
        let skill_md = create_test_skill(temp.path(), "deploy", "Deployment workflow");

        let skill = DiscoveredSkill {
            manifest: ExtensionManifest::new(
                "deploy",
                "skill",
                "deploy",
                "Deployment workflow",
                "1.0.0",
                skill_md.parent().unwrap().to_path_buf(),
            ),
            file_path: skill_md,
            base_dir: temp.path().join("deploy"),
        };

        let skills = vec![&skill];
        let prompt = build_skills_prompt(&skills);

        assert!(prompt.contains("## Skills (mandatory)"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("deploy: Deployment workflow"));
    }

    #[test]
    fn test_compact_skill_path() {
        let home = dirs::home_dir().expect("Should have home dir");
        let path = home.join(".peko/skills/docker/SKILL.md");

        let compacted = compact_skill_path(&path);
        assert!(compacted.starts_with('~'));
    }

    #[tokio::test]
    async fn test_skill_handler() {
        // Use actual home directory for cross-platform compatibility
        let home = dirs::home_dir().expect("Should have home dir");
        let skill_path = home.join(".peko/skills/docker/SKILL.md");

        let handler = SkillPromptHandler {
            skill_name: "docker".to_string(),
            description: "Docker operations".to_string(),
            file_path: skill_path,
        };

        let ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "skills".to_string(),
                priority: 100,
            },
            crate::extensions::framework::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("docker: Docker operations"));
                // Path should be compacted to use ~ for home directory
                assert!(
                    text.contains('~') || text.contains(".peko"),
                    "Expected compacted path, got: {text}"
                );
            }
            _ => panic!("Expected Continue with Text, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_register_skills_with_core() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "skill1", "First skill");
        create_test_skill(temp.path(), "skill2", "Second skill");

        let core = crate::extensions::framework::ExtensionCore::new();
        let skills = load_skills_from_directory(temp.path());

        assert_eq!(skills.len(), 2);

        let hook_ids = register_skills_with_core(&core, skills).await.unwrap();

        assert_eq!(hook_ids.len(), 2);
        assert_eq!(core.hook_count().await, 2);
    }
}
