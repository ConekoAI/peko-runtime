//! Agent Context - What agents know about each other
//!
//! Decentralized coordination: agents get context about other agents
//! and decide how to coordinate themselves.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Context provided to an agent about its environment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    /// This agent's DID
    pub self_did: String,
    /// View of the agent registry (other agents)
    pub registry_view: AgentRegistryView,
    /// Current states of other agents
    pub agent_states: HashMap<String, String>,
}

impl AgentContext {
    /// Create a minimal context
    pub fn new(self_did: impl Into<String>) -> Self {
        Self {
            self_did: self_did.into(),
            registry_view: AgentRegistryView::default(),
            agent_states: HashMap::new(),
        }
    }

    /// Find agents with a specific extension
    #[must_use]
    pub fn find_by_extension(&self, extension: &str) -> Vec<&AgentSummary> {
        self.registry_view
            .agents
            .iter()
            .filter(|a| a.extensions.contains(&extension.to_string()))
            .collect()
    }

    /// Get agent by DID
    #[must_use]
    pub fn get_agent(&self, did: &str) -> Option<&AgentSummary> {
        self.registry_view.agents.iter().find(|a| a.did == did)
    }

    /// Get agent state
    #[must_use]
    pub fn get_agent_state(&self, did: &str) -> Option<&str> {
        self.agent_states.get(did).map(std::string::String::as_str)
    }

    /// Check if agent is available (running and not busy)
    #[must_use]
    pub fn is_agent_available(&self, did: &str) -> bool {
        matches!(self.get_agent_state(did), Some("idle" | "running"))
    }
}

/// A filtered view of the registry for an agent
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRegistryView {
    /// List of other agents
    pub agents: Vec<AgentSummary>,
    /// Total agent count
    pub total_count: usize,
}

/// Summary of an agent for context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Agent DID
    pub did: String,
    /// Agent name
    pub name: String,
    /// Extensions (enabled extension names)
    pub extensions: Vec<String>,
    /// Agent description
    pub description: Option<String>,
}

/// Extension index for fast lookups
#[derive(Debug, Default)]
pub struct ExtensionIndex {
    /// Extension -> DIDs mapping
    index: HashMap<String, Vec<String>>,
}

impl ExtensionIndex {
    /// Create empty index
    #[must_use]
    pub fn new() -> Self {
        Self {
            index: HashMap::new(),
        }
    }

    /// Register agent extensions
    pub fn register(&mut self, did: &str, extensions: &[String]) {
        for ext in extensions {
            self.index
                .entry(ext.clone())
                .or_default()
                .push(did.to_string());
        }
    }

    /// Unregister agent
    pub fn unregister(&mut self, did: &str) {
        for dids in self.index.values_mut() {
            dids.retain(|d| d != did);
        }
    }

    /// Find agents by extension
    #[must_use]
    pub fn find(&self, extension: &str) -> Vec<String> {
        self.index.get(extension).cloned().unwrap_or_default()
    }

    /// Get all extensions
    #[must_use]
    pub fn extensions(&self) -> Vec<&String> {
        self.index.keys().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_index() {
        let mut idx = ExtensionIndex::new();

        idx.register("did:1", &["search".to_string(), "calc".to_string()]);
        idx.register("did:2", &["search".to_string()]);

        let searchers = idx.find("search");
        assert_eq!(searchers.len(), 2);
        assert!(searchers.contains(&"did:1".to_string()));
        assert!(searchers.contains(&"did:2".to_string()));

        let calc = idx.find("calc");
        assert_eq!(calc.len(), 1);

        idx.unregister("did:1");
        let searchers = idx.find("search");
        assert_eq!(searchers.len(), 1);
    }

    #[test]
    fn test_agent_context() {
        let mut ctx = AgentContext::new("did:self");

        ctx.registry_view.agents.push(AgentSummary {
            did: "did:1".to_string(),
            name: "SearchAgent".to_string(),
            extensions: vec!["search".to_string()],
            description: None,
        });

        ctx.agent_states
            .insert("did:1".to_string(), "idle".to_string());

        let searchers = ctx.find_by_extension("search");
        assert_eq!(searchers.len(), 1);

        assert!(ctx.is_agent_available("did:1"));
        assert!(!ctx.is_agent_available("did:unknown"));
    }
}
