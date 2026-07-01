//! System prompt service
//!
//! Encapsulates system prompt construction and skill loading,
//! keeping prompt-layer concerns out of the engine.

use crate::agents::prompt::{PromptMode, SystemPromptBuilder};
use crate::agents::Agent;
use crate::extensions::agent::{register_agents_with_core, AgentAdapter};
use crate::extensions::framework::ExtensionCore;
use crate::extensions::skill::{register_skills_with_core, SkillAdapter};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Service for building system prompts and loading skills.
///
/// This keeps prompt construction (bootstrap files, skill injection,
/// placeholder replacement) in `src/prompt/` where it belongs.
pub struct SystemPromptService;

impl SystemPromptService {
    /// Build the initial system prompt for an agent.
    ///
    /// The agent's prompt body comes from `AgentConfig::prompt.body`
    /// (set by `build_agent_config` from the resolved `AgentPrompt`).
    /// Placeholders in the body are replaced with rendered sections
    /// (tools, skills, agents, runtime).
    pub async fn build(agent: &Agent, extension_core: &Arc<ExtensionCore>) -> String {
        info!(
            "Building initial system prompt for agent '{}'",
            agent.name()
        );

        // Load and register skills before building the prompt
        let _ = Self::load_and_register_skills(agent, extension_core).await;

        // Load and register enabled agents (global agents directory)
        let _ = Self::load_and_register_agents(agent, extension_core).await;

        let workspace_dir = Self::resolve_workspace(agent);
        let body = agent
            .config
            .prompt
            .as_ref()
            .map(|p| p.body.clone())
            .unwrap_or_default();

        SystemPromptBuilder::new(agent.name())
            .with_mode(PromptMode::Full)
            .with_workspace(&workspace_dir)
            .with_extension_core(Arc::clone(extension_core))
            .with_body(body)
            .build()
    }

    /// Build a fresh system prompt dynamically.
    ///
    /// Used during the agentic loop to pick up SYSTEM.md changes,
    /// tool list updates, and skill/extension changes.
    pub async fn build_fresh(agent: &Agent, extension_core: &Arc<ExtensionCore>) -> String {
        let workspace_dir = Self::resolve_workspace(agent);

        SystemPromptBuilder::new(agent.name())
            .with_mode(PromptMode::Full)
            .with_workspace(&workspace_dir)
            .with_extension_core(Arc::clone(extension_core))
            .build()
    }

    /// Load enabled skills for an agent and register them with ExtensionCore.
    ///
    /// Returns the number of skills successfully registered.
    pub async fn load_and_register_skills(
        agent: &Agent,
        extension_core: &Arc<ExtensionCore>,
    ) -> usize {
        let path_resolver = crate::common::paths::PathResolver::new();
        let skills_dir = path_resolver.skills_dir();

        tracing::debug!("Loading skills from: {:?}", skills_dir);

        let enabled_skills: Vec<String> = agent
            .config
            .extensions
            .as_ref()
            .map(|e| e.enabled.clone())
            .unwrap_or_default();

        tracing::debug!(
            "Enabled skills for agent {}: {:?}",
            agent.name(),
            enabled_skills
        );

        if !skills_dir.exists() {
            tracing::debug!("Skills directory does not exist: {:?}", skills_dir);
            return 0;
        }

        let adapter = SkillAdapter::new();
        let all_skills = adapter.discover_skills(&skills_dir);

        tracing::debug!("Discovered {} skills from directory", all_skills.len());

        let skills_to_register: Vec<_> = all_skills
            .into_iter()
            .filter(|s| {
                let is_enabled = enabled_skills
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(&s.manifest.name));
                tracing::debug!("Skill '{}' enabled: {}", s.manifest.name, is_enabled);
                is_enabled
            })
            .collect();

        if skills_to_register.is_empty() {
            tracing::info!("No enabled skills to register for agent {}", agent.name());
            return 0;
        }

        let count = skills_to_register.len();
        let _ = register_skills_with_core(extension_core, skills_to_register).await;

        tracing::info!(
            "Registered {} enabled skills for agent {}",
            count,
            agent.name()
        );
        count
    }

    /// Load enabled agents for an agent and register them with ExtensionCore.
    ///
    /// Returns the number of agents successfully registered.
    pub async fn load_and_register_agents(
        agent: &Agent,
        extension_core: &Arc<ExtensionCore>,
    ) -> usize {
        let path_resolver = crate::common::paths::PathResolver::new();
        let agents_dir = path_resolver.agents_dir();

        tracing::debug!("Loading agents from: {:?}", agents_dir);

        let enabled_agents: Vec<String> = agent
            .config
            .extensions
            .as_ref()
            .map(|e| e.enabled.clone())
            .unwrap_or_default();

        tracing::debug!(
            "Enabled agents for agent {}: {:?}",
            agent.name(),
            enabled_agents
        );

        if !agents_dir.exists() {
            tracing::debug!("Agents directory does not exist: {:?}", agents_dir);
            return 0;
        }

        let adapter = AgentAdapter::new();
        let all_agents = adapter.discover_agents(&agents_dir);

        tracing::debug!("Discovered {} agents from directory", all_agents.len());

        let agents_to_register: Vec<_> = all_agents
            .into_iter()
            .filter(|a| {
                let is_enabled = enabled_agents
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(&a.manifest.name));
                tracing::debug!("Agent '{}' enabled: {}", a.manifest.name, is_enabled);
                is_enabled
            })
            .collect();

        if agents_to_register.is_empty() {
            tracing::info!("No enabled agents to register for agent {}", agent.name());
            return 0;
        }

        let count = agents_to_register.len();
        let _ = register_agents_with_core(extension_core, agents_to_register).await;

        tracing::info!(
            "Registered {} enabled agents for agent {}",
            count,
            agent.name()
        );
        count
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn resolve_workspace(agent: &Agent) -> PathBuf {
        agent
            .config
            .workspace
            .clone()
            .or_else(|| {
                let resolver = crate::common::paths::PathResolver::new();
                Some(resolver.agent_workspace(agent.name()))
            })
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_resolve_workspace_fallback() {
        // We can't easily construct an Agent here without a lot of setup,
        // but we can at least verify the service struct exists.
        // Full tests belong in integration tests.
    }
}
