//! Subagent resolution helper for the `Agent` tool.
//!
//! `AgentService` no longer implements standalone agent CRUD. It only
//! resolves a `subagent_type` name to an [`AgentConfig`]:
//!
//! - When scoped to a Principal workspace, it prefers
//!   `<workspace>/agents/<name>/AGENT.md` (or `<workspace>/agents/<name>.md`)
//!   and falls back to the global `~/.peko/agents/<name>/config.toml` layout.
//! - Without a workspace, it resolves from the global layout only.

use crate::agents::agent_config::AgentConfig;
use crate::common::identifiers::parse_agent_name;
use crate::common::paths::PathResolver;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Service for resolving subagent configurations.
#[derive(Debug, Clone)]
pub struct AgentService {
    resolver: PathResolver,
    /// Optional principal workspace. When set, the `Agent` tool will first
    /// look for subagents under `<workspace>/agents/<name>/AGENT.md` before
    /// falling back to the global `~/.peko/agents/<name>/config.toml` layout.
    principal_workspace: Option<PathBuf>,
}

impl AgentService {
    /// Create a new agent service with the given path resolver.
    #[must_use]
    pub fn new(resolver: PathResolver) -> Self {
        Self {
            resolver,
            principal_workspace: None,
        }
    }

    /// Create an agent service scoped to a Principal workspace.
    ///
    /// Subagent resolution will prefer principal agents (`agents/<name>/AGENT.md`)
    /// and fall back to global agents if no matching principal agent exists.
    #[must_use]
    pub fn for_principal(workspace: impl Into<PathBuf>) -> Self {
        Self {
            resolver: PathResolver::new(),
            principal_workspace: Some(workspace.into()),
        }
    }

    /// Load an agent config by name from the filesystem.
    ///
    /// This is the resolution hook for the `Agent` tool's `subagent_type`
    /// parameter: `subagent_type` maps to `~/.peko/agents/<name>/config.toml`.
    ///
    /// If a principal workspace was configured, the service first tries to
    /// resolve the agent from `<workspace>/agents/<name>/AGENT.md`.
    pub async fn resolve_subagent_type(&self, name: &str) -> Result<AgentConfig> {
        if let Some(ref workspace) = self.principal_workspace {
            match self.resolve_principal_agent(name, workspace).await {
                Ok(config) => return Ok(config),
                Err(e) => {
                    tracing::debug!(
                        "Principal agent '{name}' not found in workspace '{}': {e}; falling back to global agent",
                        workspace.display()
                    );
                }
            }
        }

        let agent_name = parse_agent_name(name)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Subagent type '{name}' not found at {config_path:?}");
        }
        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: AgentConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse agent config for '{name}'"))?;
        Ok(config)
    }

    /// Resolve a principal agent from its `AGENT.md` extension.
    async fn resolve_principal_agent(&self, name: &str, workspace: &Path) -> Result<AgentConfig> {
        let agents_dir = workspace.join("agents");

        // Prefer the directory layout, then fall back to the flat layout.
        let agent_md = {
            let dir_layout = agents_dir.join(name).join("AGENT.md");
            let flat_layout = agents_dir.join(format!("{name}.md"));

            if dir_layout.exists() {
                dir_layout
            } else if flat_layout.exists() {
                flat_layout
            } else {
                anyhow::bail!(
                    "No agent prompt found for principal agent '{name}' at {:?} or {:?}",
                    dir_layout,
                    flat_layout
                );
            }
        };

        let prompt = crate::principal::agent_prompt::load_agent_prompt(&agent_md)
            .with_context(|| format!("Failed to load principal agent prompt '{name}'"))?;

        Ok(AgentConfig {
            name: prompt.name,
            description: prompt.frontmatter.description,
            prompt: Some(prompt.body),
            ..AgentConfig::default()
        })
    }

    /// Check whether a global agent config exists for the given name.
    #[must_use]
    pub fn agent_exists(&self, name: &str) -> bool {
        if let Ok(agent_name) = parse_agent_name(name) {
            self.resolver.agent_exists(agent_name)
        } else {
            false
        }
    }

    /// Get the path resolver.
    #[must_use]
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_service_creation() {
        let resolver = PathResolver::new();
        let _service = AgentService::new(resolver);
    }

    #[tokio::test]
    async fn test_resolve_principal_agent_flat_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("workspace");
        let agents_dir = workspace.join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        std::fs::write(
            agents_dir.join("worker.md"),
            r"---
name: Worker Bot
description: A flat-file worker agent
---

You are a worker.
",
        )
        .unwrap();

        let service = AgentService::for_principal(&workspace);
        let config = service.resolve_subagent_type("worker").await.unwrap();

        assert_eq!(config.name, "Worker Bot");
        assert_eq!(
            config.description.as_deref(),
            Some("A flat-file worker agent")
        );
        assert!(config
            .prompt
            .as_deref()
            .unwrap()
            .contains("You are a worker."));
    }

    #[tokio::test]
    async fn test_resolve_principal_agent_directory_layout() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("workspace");
        let agent_dir = workspace.join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();

        std::fs::write(
            agent_dir.join("AGENT.md"),
            r"---
name: Worker Bot
description: A directory-layout worker agent
---

You are a worker.
",
        )
        .unwrap();

        let service = AgentService::for_principal(&workspace);
        let config = service.resolve_subagent_type("worker").await.unwrap();

        assert_eq!(config.name, "Worker Bot");
        assert_eq!(
            config.description.as_deref(),
            Some("A directory-layout worker agent")
        );
    }
}
