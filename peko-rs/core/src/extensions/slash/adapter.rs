//! Slash Adapter for the Extension system
//!
//! This adapter enables slash commands (COMMAND.md files with YAML frontmatter)
//! to be managed through the unified Extension Architecture.
//!
//! # Slash Command Format
//!
//! Slash commands are markdown files with YAML frontmatter:
//! ```markdown
//! ---
//! name: deploy
//! description: Deploy the current service
//! ---
//!
//! # Deploy
//!
//! Usage instructions...
//! ```
//!
//! # Hook Points
//!
//! Slash commands are user-invoked and do not hook into the system prompt.
//! They are dispatched from `src/commands/send.rs`.

use crate::extensions::framework::adapters::parsing;
use crate::extensions::framework::adapters::{ExtensionTypeAdapter, ManifestFormat};
use crate::extensions::framework::core::HookBinding;
use crate::extensions::framework::types::ExtensionManifest;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Slash extension type identifier
pub const SLASH_EXTENSION_TYPE: &str = "slash";

/// Slash adapter for Extension system
#[derive(Debug)]
pub struct SlashAdapter;

impl SlashAdapter {
    /// Create a new slash adapter
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Discover slash commands from a directory
    pub fn discover_commands(&self, path: &Path) -> Vec<DiscoveredCommand> {
        let mut commands = Vec::new();

        if !path.exists() {
            debug!("Slash commands directory does not exist: {:?}", path);
            return commands;
        }

        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read slash commands directory {:?}: {}", path, e);
                return commands;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let command_md = path.join("COMMAND.md");
            if command_md.exists() {
                match self.parse_command_manifest(&command_md) {
                    Ok(manifest) => {
                        commands.push(DiscoveredCommand {
                            manifest,
                            file_path: command_md,
                            base_dir: path,
                        });
                    }
                    Err(e) => {
                        warn!("Failed to parse slash command from {:?}: {}", command_md, e);
                    }
                }
            }
        }

        commands
    }

    /// Parse a COMMAND.md file into an extension manifest
    fn parse_command_manifest(&self, path: &Path) -> Result<ExtensionManifest> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;

        let (meta, _body): (SlashFrontmatter, _) = parsing::parse_yaml_frontmatter_typed(&content)
            .with_context(|| format!("Failed to parse frontmatter in {path:?}"))?;

        if meta.name.is_empty() {
            anyhow::bail!("Slash command name cannot be empty");
        }
        if meta.description.is_empty() {
            anyhow::bail!("Slash command description cannot be empty");
        }

        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut manifest = ExtensionManifest::new(
            &meta.name,
            SLASH_EXTENSION_TYPE,
            &meta.name,
            &meta.description,
            meta.version.as_deref().unwrap_or("1.0.0"),
            base_dir,
        );

        manifest.set("command_file", path.to_string_lossy().to_string());
        manifest.set("tags", meta.tags);
        if let Some(author) = meta.author {
            manifest.set("author", author);
        }

        Ok(manifest)
    }
}

impl Default for SlashAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtensionTypeAdapter for SlashAdapter {
    fn extension_type(&self) -> &'static str {
        SLASH_EXTENSION_TYPE
    }

    fn manifest_format(&self) -> ManifestFormat {
        ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["name", "description"],
            file_name: "COMMAND.md",
        }
    }

    fn resolve_hooks(&self, _manifest: &ExtensionManifest) -> Vec<HookBinding> {
        // Slash commands are user-invoked and do not hook into the system prompt.
        Vec::new()
    }

    fn parse_manifest(
        &self,
        path: &Path,
        content: &str,
    ) -> anyhow::Result<crate::extensions::framework::ExtensionManifest> {
        let (command_frontmatter, _): (SlashFrontmatter, _) =
            parsing::parse_yaml_frontmatter_typed(content)
                .with_context(|| format!("Failed to parse COMMAND.md frontmatter in {path:?}"))?;

        if command_frontmatter.name.is_empty() {
            anyhow::bail!("Slash command name cannot be empty");
        }
        if command_frontmatter.description.is_empty() {
            anyhow::bail!("Slash command description cannot be empty");
        }

        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut manifest = ExtensionManifest::new(
            &command_frontmatter.name,
            SLASH_EXTENSION_TYPE,
            &command_frontmatter.name,
            &command_frontmatter.description,
            command_frontmatter.version.as_deref().unwrap_or("1.0.0"),
            base_dir.to_path_buf(),
        );

        manifest.set("command_file", path.to_string_lossy().to_string());
        manifest.set("tags", command_frontmatter.tags);
        if let Some(author) = command_frontmatter.author {
            manifest.set("author", author);
        }

        Ok(manifest)
    }
}

/// A discovered slash command before registration
#[derive(Debug, Clone)]
pub struct DiscoveredCommand {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Full path to COMMAND.md
    pub file_path: PathBuf,
    /// Slash command base directory
    pub base_dir: PathBuf,
}

/// YAML frontmatter from COMMAND.md
#[derive(Debug, Deserialize)]
pub struct SlashFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Helper to load slash commands from directory using the adapter
#[must_use]
pub fn load_commands_from_directory(path: &Path) -> Vec<DiscoveredCommand> {
    let adapter = SlashAdapter::new();
    adapter.discover_commands(path)
}

/// Register slash commands with an `ExtensionCore`
pub async fn register_commands_with_core(
    _core: &crate::extensions::framework::ExtensionCore,
    _commands: Vec<DiscoveredCommand>,
) -> Result<Vec<crate::extensions::framework::types::HookId>> {
    // Slash commands are user-invoked and do not register hooks in v0.
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_command(dir: &Path, name: &str, description: &str) -> PathBuf {
        let command_dir = dir.join(name);
        std::fs::create_dir(&command_dir).unwrap();

        let content = format!(
            r"---
name: {name}
description: {description}
tags: [test]
author: Test Author
---

# Test Command

This is a test slash command.
"
        );

        let command_md = command_dir.join("COMMAND.md");
        std::fs::write(&command_md, content).unwrap();
        command_md
    }

    #[test]
    fn test_slash_adapter_manifest_format() {
        let adapter = SlashAdapter::new();
        let format = adapter.manifest_format();

        assert!(matches!(
            format,
            ManifestFormat::YamlFrontmatterMarkdown { .. }
        ));
    }

    #[test]
    fn test_discover_commands() {
        let temp = TempDir::new().unwrap();

        create_test_command(temp.path(), "deploy", "Deploy service");
        create_test_command(temp.path(), "rollback", "Rollback service");

        let adapter = SlashAdapter::new();
        let commands = adapter.discover_commands(temp.path());

        assert_eq!(commands.len(), 2);
        assert!(commands.iter().any(|c| c.manifest.name == "deploy"));
        assert!(commands.iter().any(|c| c.manifest.name == "rollback"));
    }

    #[test]
    fn test_parse_command_manifest() {
        let temp = TempDir::new().unwrap();
        let command_md = create_test_command(temp.path(), "deploy", "Deploy service");

        let adapter = SlashAdapter::new();
        let manifest = adapter.parse_command_manifest(&command_md).unwrap();

        assert_eq!(manifest.name, "deploy");
        assert_eq!(manifest.description, "Deploy service");
        assert_eq!(manifest.extension_type, "slash");
        assert!(manifest.get("tags").is_some());
    }

    #[test]
    fn test_parse_manifest_trait_method() {
        let temp = TempDir::new().unwrap();
        let command_md = create_test_command(temp.path(), "rollback", "Rollback service");
        let content = std::fs::read_to_string(&command_md).unwrap();

        let adapter = SlashAdapter::new();
        let manifest = adapter.parse_manifest(&command_md, &content).unwrap();

        assert_eq!(manifest.name, "rollback");
        assert_eq!(manifest.extension_type, "slash");
    }

    #[test]
    fn test_load_commands_from_directory() {
        let temp = TempDir::new().unwrap();
        create_test_command(temp.path(), "deploy", "Deploy service");

        let commands = load_commands_from_directory(temp.path());
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].manifest.name, "deploy");
    }

    #[test]
    fn test_empty_directory() {
        let temp = TempDir::new().unwrap();
        let commands = load_commands_from_directory(temp.path());
        assert!(commands.is_empty());
    }

    #[test]
    fn test_missing_frontmatter_fails() {
        let temp = TempDir::new().unwrap();
        let command_dir = temp.path().join("broken");
        std::fs::create_dir(&command_dir).unwrap();
        std::fs::write(command_dir.join("COMMAND.md"), "# No frontmatter\n").unwrap();

        let adapter = SlashAdapter::new();
        assert!(adapter.discover_commands(temp.path()).is_empty());
    }
}
