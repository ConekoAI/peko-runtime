//! Agent management module

use tracing::{debug, info};

/// Single agent runtime
pub struct Agent {
    pub did: String,
    pub name: String,
}

impl Agent {
    pub fn new(name: &str) -> Self {
        debug!("Creating new agent: {}", name);
        Self {
            did: format!("did:pekobot:local:{}", uuid::Uuid::new_v4()),
            name: name.to_string(),
        }
    }

    pub fn start(&self) {
        info!("Starting agent: {} ({})", self.name, self.did);
    }
}

/// Multi-agent orchestrator
pub struct Orchestrator {
    agents: Vec<Agent>,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self { agents: vec![] }
    }

    pub fn add_agent(&mut self, agent: Agent) {
        info!("Adding agent to orchestrator: {}", agent.name);
        self.agents.push(agent);
    }
}
