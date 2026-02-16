//! Coneko agent registry

use super::client::AgentInfo;

/// Local agent registry (when not using Coneko)
pub struct LocalRegistry {
    agents: Vec<AgentInfo>,
}

impl LocalRegistry {
    pub fn new() -> Self {
        Self { agents: vec![] }
    }

    pub fn register(&mut self, agent: AgentInfo) {
        self.agents.push(agent);
    }

    pub fn list(&self) -> &[AgentInfo] {
        &self.agents
    }

    pub fn find_by_capability(&self, capability: &str) -> Vec<&AgentInfo> {
        self.agents
            .iter()
            .filter(|a| a.capabilities.contains(&capability.to_string()))
            .collect()
    }
}
