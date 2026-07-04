//! Agent Adapter for the Extension system
//!
//! This adapter enables agents (AGENT.md files with YAML frontmatter) to be
//! managed through the unified Extension Architecture.
//!
//! # Agent Format
//!
//! Agents are markdown files with YAML frontmatter:
//! ```markdown
//! ---
//! name: math
//! description: Mathematics and calculations
//! color: "#ff0000"
//! ---
//!
//! # Math Agent
//!
//! Instructions for the LLM...
//! ```
//!
//! # Hook Points
//!
//! Agents hook into:
//! - `PromptSystemSection { section: "agents" }` - Injects available agents into system prompt

use crate::extensions::framework::adapters::parsing;
use crate::extensions::framework::adapters::{ExtensionTypeAdapter, ManifestFormat};
use crate::extensions::framework::core::{
    HookBinding, HookContext, HookHandler, HookHandlerFactory, HookPoint,
};
use crate::extensions::framework::types::{
    ExtensionId, ExtensionManifest, HookId, HookOutput, HookResult,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Agent extension type identifier
pub const AGENT_EXTENSION_TYPE: &str = "agent";

/// Default priority for agent prompt injection
pub const AGENT_HOOK_PRIORITY: i32 = 90;

/// Agent adapter for Extension system
#[derive(Debug)]
pub struct AgentAdapter;

impl AgentAdapter {
    /// Create a new agent adapter
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Discover agents from a directory
    pub fn discover_agents(&self, path: &Path) -> Vec<DiscoveredAgent> {
        let mut agents = Vec::new();

        if !path.exists() {
            debug!("Agents directory does not exist: {:?}", path);
            return agents;
        }

        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read agents directory {:?}: {}", path, e);
                return agents;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let agent_md = path.join("AGENT.md");
            if agent_md.exists() {
                match self.parse_agent_manifest(&agent_md) {
                    Ok(manifest) => {
                        agents.push(DiscoveredAgent {
                            manifest,
                            file_path: agent_md,
                            base_dir: path,
                        });
                    }
                    Err(e) => {
                        warn!("Failed to parse agent from {:?}: {}", agent_md, e);
                    }
                }
            }
        }

        agents
    }

    /// Parse an AGENT.md file into an extension manifest
    fn parse_agent_manifest(&self, path: &Path) -> Result<ExtensionManifest> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;

        let (meta, _body): (AgentFrontmatter, _) = parsing::parse_yaml_frontmatter_typed(&content)
            .with_context(|| format!("Failed to parse frontmatter in {path:?}"))?;

        if meta.name.is_empty() {
            anyhow::bail!("Agent name cannot be empty");
        }
        if meta.description.is_empty() {
            anyhow::bail!("Agent description cannot be empty");
        }

        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut manifest = ExtensionManifest::new(
            &meta.name,
            AGENT_EXTENSION_TYPE,
            &meta.name,
            &meta.description,
            "1.0.0",
            base_dir,
        );

        manifest.set("agent_file", path.to_string_lossy().to_string());
        manifest.set("color", meta.color.unwrap_or_default());

        Ok(manifest)
    }
}

impl Default for AgentAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtensionTypeAdapter for AgentAdapter {
    fn extension_type(&self) -> &'static str {
        AGENT_EXTENSION_TYPE
    }

    fn manifest_format(&self) -> ManifestFormat {
        ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["name", "description"],
            file_name: "AGENT.md",
        }
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![HookBinding::new(
            HookPoint::PromptSystemSection {
                section: "agents".to_string(),
                priority: AGENT_HOOK_PRIORITY,
            },
            Box::new(AgentPromptHandlerFactory {
                manifest: manifest.clone(),
            }),
        )]
    }

    fn parse_manifest(
        &self,
        path: &Path,
        content: &str,
    ) -> anyhow::Result<crate::extensions::framework::ExtensionManifest> {
        let (agent_frontmatter, _): (AgentFrontmatter, _) =
            parsing::parse_yaml_frontmatter_typed(content)
                .with_context(|| format!("Failed to parse AGENT.md frontmatter in {path:?}"))?;

        if agent_frontmatter.name.is_empty() {
            anyhow::bail!("Agent name cannot be empty");
        }
        if agent_frontmatter.description.is_empty() {
            anyhow::bail!("Agent description cannot be empty");
        }

        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut manifest = ExtensionManifest::new(
            &agent_frontmatter.name,
            AGENT_EXTENSION_TYPE,
            &agent_frontmatter.name,
            &agent_frontmatter.description,
            "1.0.0",
            base_dir.to_path_buf(),
        );

        manifest.set("agent_file", path.to_string_lossy().to_string());
        if let Some(color) = agent_frontmatter.color {
            manifest.set("color", color);
        }

        Ok(manifest)
    }
}

/// A discovered agent before registration
#[derive(Debug, Clone)]
pub struct DiscoveredAgent {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Full path to AGENT.md
    pub file_path: PathBuf,
    /// Agent base directory
    pub base_dir: PathBuf,
}

/// YAML frontmatter from AGENT.md
#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    color: Option<String>,
}

/// Factory for creating agent prompt handlers
#[derive(Debug, Clone)]
struct AgentPromptHandlerFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for AgentPromptHandlerFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(AgentPromptHandler {
            agent_name: self.manifest.name.clone(),
            description: self.manifest.description.clone(),
            file_path: PathBuf::from(
                self.manifest
                    .get("agent_file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
        })
    }
}

/// Handler that injects agent into prompt
#[derive(Debug, Clone)]
struct AgentPromptHandler {
    agent_name: String,
    description: String,
    file_path: PathBuf,
}

#[async_trait]
impl HookHandler for AgentPromptHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Filter at handle-time using the principal's enabled-agent allowlist
        // from `AgentStateRegistry`. The prompt builder injects `principal_id`
        // via `ctx.state["tool_context"]`.
        let principal_id = ctx
            .get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context")
            .and_then(|tc| tc.principal_id.as_ref())
            .map(|pid| crate::principal::PrincipalId(pid.clone()));

        let enabled = crate::principal::AgentStateRegistry::global()
            .is_agent_enabled(principal_id.as_ref(), &self.agent_name)
            .await;

        if !enabled {
            return HookResult::PassThrough;
        }

        let path_display = self.file_path.to_string_lossy();
        let text = format!(
            "- {}: {} (location: {})",
            self.agent_name, self.description, path_display
        );

        HookResult::Continue(HookOutput::Text(text))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "agents".to_string(),
            priority: AGENT_HOOK_PRIORITY,
        }
    }

    fn priority(&self) -> i32 {
        AGENT_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("AgentPromptHandler({})", self.agent_name)
    }
}

/// Helper to load agents from directory using the adapter
#[must_use]
pub fn load_agents_from_directory(path: &Path) -> Vec<DiscoveredAgent> {
    let adapter = AgentAdapter::new();
    adapter.discover_agents(path)
}

/// Register agents with an `ExtensionCore`
pub async fn register_agents_with_core(
    core: &crate::extensions::framework::ExtensionCore,
    agents: Vec<DiscoveredAgent>,
) -> Result<Vec<HookId>> {
    let mut hook_ids = Vec::new();

    for agent in agents {
        let extension_id =
            ExtensionId::new(format!("{}:{}", AGENT_EXTENSION_TYPE, agent.manifest.id.0));

        let handler = Arc::new(AgentPromptHandler {
            agent_name: agent.manifest.name.clone(),
            description: agent.manifest.description.clone(),
            file_path: agent.file_path,
        });

        let registration = core
            .register_hook(
                HookPoint::PromptSystemSection {
                    section: "agents".to_string(),
                    priority: AGENT_HOOK_PRIORITY,
                },
                handler,
                &extension_id,
            )
            .await?;

        hook_ids.push(registration.id);
        info!(
            agent_name = %agent.manifest.name,
            hook_id = %registration.id,
            "Registered agent with ExtensionCore"
        );
    }

    Ok(hook_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::ExtensionServices;
    use tempfile::TempDir;

    fn create_test_agent(dir: &Path, name: &str, description: &str) -> PathBuf {
        let agent_dir = dir.join(name);
        std::fs::create_dir(&agent_dir).unwrap();

        let content = format!(
            r"---
name: {name}
description: {description}
color: '#ff0000'
---

# Test Agent

This is a test agent.
"
        );

        let agent_md = agent_dir.join("AGENT.md");
        std::fs::write(&agent_md, content).unwrap();
        agent_md
    }

    #[test]
    fn test_agent_adapter_manifest_format() {
        let adapter = AgentAdapter::new();
        let format = adapter.manifest_format();

        assert!(matches!(
            format,
            ManifestFormat::YamlFrontmatterMarkdown { .. }
        ));
    }

    #[test]
    fn test_discover_agents() {
        let temp = TempDir::new().unwrap();

        create_test_agent(temp.path(), "agent1", "First agent");
        create_test_agent(temp.path(), "agent2", "Second agent");

        let adapter = AgentAdapter::new();
        let agents = adapter.discover_agents(temp.path());

        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.manifest.name == "agent1"));
        assert!(agents.iter().any(|a| a.manifest.name == "agent2"));
    }

    #[test]
    fn test_parse_agent_manifest() {
        let temp = TempDir::new().unwrap();
        let agent_md = create_test_agent(temp.path(), "math", "Math operations");

        let adapter = AgentAdapter::new();
        let manifest = adapter.parse_agent_manifest(&agent_md).unwrap();

        assert_eq!(manifest.name, "math");
        assert_eq!(manifest.description, "Math operations");
        assert_eq!(manifest.extension_type, "agent");
    }

    #[tokio::test]
    async fn test_register_agents_with_core() {
        let temp = TempDir::new().unwrap();
        create_test_agent(temp.path(), "agent1", "First agent");
        create_test_agent(temp.path(), "agent2", "Second agent");

        let core = crate::extensions::framework::ExtensionCore::new();
        let agents = load_agents_from_directory(temp.path());

        assert_eq!(agents.len(), 2);

        let hook_ids = register_agents_with_core(&core, agents).await.unwrap();

        assert_eq!(hook_ids.len(), 2);
        assert_eq!(core.hook_count().await, 2);
    }

    #[tokio::test]
    async fn test_agent_handler_emits_when_enabled() {
        let temp = TempDir::new().unwrap();
        let agent_md = create_test_agent(temp.path(), "math", "Math operations");

        let handler = AgentPromptHandler {
            agent_name: "math".to_string(),
            description: "Math operations".to_string(),
            file_path: agent_md,
        };

        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "agents".to_string(),
                priority: AGENT_HOOK_PRIORITY,
            },
            crate::extensions::framework::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        let principal_id = crate::principal::PrincipalId("test-handler".to_string());
        crate::principal::AgentStateRegistry::global()
            .register(
                principal_id.clone(),
                crate::principal::AgentState::new(vec!["math".to_string()]),
            )
            .await;
        ctx.set_state(
            "tool_context",
            crate::extensions::framework::types::ToolRuntimeContext::new()
                .with_principal_id(principal_id.0.clone()),
        );

        let result = handler.handle(ctx).await;

        crate::principal::AgentStateRegistry::global()
            .unregister(&principal_id)
            .await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("math: Math operations"));
            }
            _ => panic!("Expected Continue with Text, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_agent_handler_filters_disabled_agents() {
        let temp = TempDir::new().unwrap();
        let agent_md = create_test_agent(temp.path(), "math", "Math operations");

        let handler = AgentPromptHandler {
            agent_name: "math".to_string(),
            description: "Math operations".to_string(),
            file_path: agent_md,
        };

        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "agents".to_string(),
                priority: AGENT_HOOK_PRIORITY,
            },
            crate::extensions::framework::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        let principal_id = crate::principal::PrincipalId("test-filter".to_string());
        crate::principal::AgentStateRegistry::global()
            .register(
                principal_id.clone(),
                crate::principal::AgentState::new(vec!["other".to_string()]),
            )
            .await;
        ctx.set_state(
            "tool_context",
            crate::extensions::framework::types::ToolRuntimeContext::new()
                .with_principal_id(principal_id.0.clone()),
        );

        let result = handler.handle(ctx).await;

        crate::principal::AgentStateRegistry::global()
            .unregister(&principal_id)
            .await;

        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected disabled agent to emit nothing, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_agent_handler_fail_closed_without_principal_id() {
        let temp = TempDir::new().unwrap();
        let agent_md = create_test_agent(temp.path(), "math", "Math operations");

        let handler = AgentPromptHandler {
            agent_name: "math".to_string(),
            description: "Math operations".to_string(),
            file_path: agent_md,
        };

        let ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "agents".to_string(),
                priority: AGENT_HOOK_PRIORITY,
            },
            crate::extensions::framework::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;

        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected no principal_id to emit nothing, got {result:?}"
        );
    }
}
