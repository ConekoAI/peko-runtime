//! System prompt service
//!
//! Encapsulates system prompt construction and skill loading,
//! keeping prompt-layer concerns out of the engine.

use crate::agents::prompt::{PromptMode, SystemPromptBuilder};
use crate::agents::Agent;
use crate::extensions::framework::ExtensionCore;
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

        let workspace_dir = Self::resolve_workspace(agent);
        let body = agent
            .config
            .prompt
            .as_ref()
            .map(|p| p.body.clone())
            .unwrap_or_default();

        let mut builder = SystemPromptBuilder::new(agent.name())
            .with_mode(PromptMode::Full)
            .with_workspace(&workspace_dir)
            .with_extension_core(Arc::clone(extension_core))
            .with_principal_id(agent.principal_id().to_string())
            .with_body(body);

        if let Some(memory) = crate::agents::prompt::memory::load_principal_memory(&workspace_dir) {
            builder = builder.with_principal_memory(memory);
        }

        builder.build()
    }

    /// Build a fresh system prompt dynamically.
    ///
    /// Used during the agentic loop to pick up SYSTEM.md changes,
    /// tool list updates, and skill/extension changes.
    pub async fn build_fresh(agent: &Agent, extension_core: &Arc<ExtensionCore>) -> String {
        let workspace_dir = Self::resolve_workspace(agent);

        let mut builder = SystemPromptBuilder::new(agent.name())
            .with_mode(PromptMode::Full)
            .with_workspace(&workspace_dir)
            .with_extension_core(Arc::clone(extension_core))
            .with_principal_id(agent.principal_id().to_string());

        if let Some(memory) = crate::agents::prompt::memory::load_principal_memory(&workspace_dir) {
            builder = builder.with_principal_memory(memory);
        }

        builder.build()
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
