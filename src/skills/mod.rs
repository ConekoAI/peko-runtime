//! Skills system for agents
//!
//! Skills are modular capabilities that can be loaded from TOML manifests.
//! Each skill can define tools, prompts, and configuration.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

/// A skill is a user-defined capability with tools and prompts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Skill name (unique identifier)
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Version (semver)
    pub version: String,
    /// Optional author
    #[serde(default)]
    pub author: Option<String>,
    /// Tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
    /// Tools defined by this skill
    #[serde(default)]
    pub tools: Vec<SkillTool>,
    /// System prompts provided by this skill
    #[serde(default)]
    pub prompts: Vec<String>,
    /// File path where skill was loaded from
    #[serde(skip)]
    pub location: Option<PathBuf>,
}

/// A tool defined by a skill
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTool {
    /// Tool name
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Tool kind: "shell", "http", "script"
    pub kind: String,
    /// Command/URL/script to execute
    pub command: String,
    /// Additional arguments
    #[serde(default)]
    pub args: HashMap<String, String>,
}

/// Skill manifest (SKILL.toml structure)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillManifest {
    skill: SkillMeta,
    #[serde(default)]
    tools: Vec<SkillTool>,
    #[serde(default)]
    prompts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillMeta {
    name: String,
    description: String,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

fn default_version() -> String {
    "0.1.0".to_string()
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
            if path.is_dir() {
                match self.load_skill_from_dir(&path) {
                    Ok(skill) => {
                        info!("Loaded skill: {} v{}", skill.name, skill.version);
                        self.skills.insert(skill.name.clone(), skill);
                        count += 1;
                    }
                    Err(e) => {
                        warn!("Failed to load skill from {:?}: {}", path, e);
                    }
                }
            }
        }

        info!("Loaded {} skills from {:?}", count, self.skills_dir);
        Ok(count)
    }

    /// Load a single skill from a directory
    fn load_skill_from_dir(&self, dir: &Path) -> Result<Skill> {
        let manifest_path = dir.join("SKILL.toml");
        
        if !manifest_path.exists() {
            anyhow::bail!("SKILL.toml not found in {:?}", dir);
        }

        let content = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read {:?}", manifest_path))?;

        let manifest: SkillManifest = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {:?}", manifest_path))?;

        let skill = Skill {
            name: manifest.skill.name,
            description: manifest.skill.description,
            version: manifest.skill.version,
            author: manifest.skill.author,
            tags: manifest.skill.tags,
            tools: manifest.tools,
            prompts: manifest.prompts,
            location: Some(dir.to_path_buf()),
        };

        Ok(skill)
    }

    /// Get a skill by name
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Get all loaded skills
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Get skills by tag
    pub fn find_by_tag(&self, tag: &str) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|s| s.tags.contains(&tag.to_string()))
            .collect()
    }

    /// Get all tools from all skills
    pub fn all_tools(&self) -> Vec<(&str, &SkillTool)> {
        self.skills
            .values()
            .flat_map(|s| s.tools.iter().map(move |t| (s.name.as_str(), t)))
            .collect()
    }

    /// Check if a skill is loaded
    pub fn has(&self, name: &str) -> bool {
        self.skills.contains_key(name)
    }

    /// Get the skills directory path
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }
}

/// Load a skill from a TOML string (for testing)
pub fn parse_skill_manifest(content: &str) -> Result<Skill> {
    let manifest: SkillManifest = toml::from_str(content)
        .context("Failed to parse skill manifest")?;

    Ok(Skill {
        name: manifest.skill.name,
        description: manifest.skill.description,
        version: manifest.skill.version,
        author: manifest.skill.author,
        tags: manifest.skill.tags,
        tools: manifest.tools,
        prompts: manifest.prompts,
        location: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_skill_manifest() {
        let toml = r#"
[skill]
name = "test-skill"
description = "A test skill"
version = "1.0.0"
author = "Test Author"
tags = ["test", "example"]

[[tools]]
name = "echo"
description = "Echo a message"
kind = "shell"
command = "echo"

prompts = ["You are a helpful assistant."]
"#;

        let result = parse_skill_manifest(toml);
        assert!(result.is_ok());
        let skill = result.unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.version, "1.0.0");
        assert_eq!(skill.tools.len(), 1);
    }

    #[test]
    fn test_skills_registry() {
        let temp = TempDir::new().unwrap();
        let skill_dir = temp.path().join("test-skill");
        std::fs::create_dir(&skill_dir).unwrap();

        let manifest = r#"
[skill]
name = "test-skill"
description = "A test skill"
version = "0.1.0"
"#;
        std::fs::write(skill_dir.join("SKILL.toml"), manifest).unwrap();

        let mut registry = SkillsRegistry::new(temp.path());
        let count = registry.load_all().unwrap();
        
        assert_eq!(count, 1);
        assert!(registry.has("test-skill"));
        assert!(registry.get("test-skill").is_some());
    }

    #[test]
    fn test_find_by_tag() {
        let temp = TempDir::new().unwrap();
        
        let skill1_dir = temp.path().join("skill1");
        std::fs::create_dir(&skill1_dir).unwrap();
        std::fs::write(
            skill1_dir.join("SKILL.toml"),
            r#"[skill]
name = "skill1"
description = "Test"
tags = ["utility"]
"#,
        ).unwrap();

        let skill2_dir = temp.path().join("skill2");
        std::fs::create_dir(&skill2_dir).unwrap();
        std::fs::write(
            skill2_dir.join("SKILL.toml"),
            r#"[skill]
name = "skill2"
description = "Test"
tags = ["utility", "network"]
"#,
        ).unwrap();

        let mut registry = SkillsRegistry::new(temp.path());
        registry.load_all().unwrap();

        let utility_skills = registry.find_by_tag("utility");
        assert_eq!(utility_skills.len(), 2);

        let network_skills = registry.find_by_tag("network");
        assert_eq!(network_skills.len(), 1);
    }
}
