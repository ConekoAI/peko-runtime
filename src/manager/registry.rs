//! Local Registry - Agent metadata and discovery

use crate::manager::context::{AgentRegistryView, AgentSummary, CapabilityIndex};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Registry trait for agent discovery
#[async_trait::async_trait(?Send)]
pub trait Registry {
    /// Register an agent (takes DID and name, not Arc<Agent> for Send safety)
    async fn register(
        &mut self,
        did: &str,
        name: &str,
    ) -> Result<()>;

    /// Unregister an agent
    async fn unregister(
        &mut self,
        did: &str,
    ) -> Result<()>;

    /// Get metadata by DID
    async fn get(
        &self,
        did: &str,
    ) -> Option<AgentMetadata>;

    /// Get metadata by name
    async fn get_by_name(
        &self,
        name: &str,
    ) -> Option<AgentMetadata>;

    /// List all agents
    async fn list(
        &self,
    ) -> Vec<AgentMetadata>;

    /// Find by capability
    async fn find_by_capability(
        &self,
        capability: &str,
    ) -> Vec<String>;
}

/// Agent metadata stored in registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    /// Agent DID
    pub did: String,
    /// Agent name
    pub name: String,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Description
    pub description: Option<String>,
    /// When registered
    pub registered_at: chrono::DateTime<chrono::Utc>,
}

/// Local in-memory registry
#[derive(Debug)]
pub struct LocalRegistry {
    /// Metadata by DID
    metadata: HashMap<String, AgentMetadata>,
    /// Capability index
    capabilities: CapabilityIndex,
    /// Name -> DID mapping
    name_index: HashMap<String, String>,
}

/// Registry events
#[derive(Debug, Clone)]
pub enum RegistryEvent {
    /// Agent registered
    Registered { did: String, name: String },
    /// Agent unregistered
    Unregistered { did: String },
    /// Capabilities updated
    CapabilitiesUpdated { did: String, capabilities: Vec<String> },
}

impl LocalRegistry {
    /// Create new registry
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            capabilities: CapabilityIndex::new(),
            name_index: HashMap::new(),
        }
    }

    /// Get a filtered view for an agent (doesn't include self)
    pub fn get_view(
        &self,
        self_did: &str,
    ) -> Result<AgentRegistryView> {
        let agents: Vec<AgentSummary> = self
            .metadata
            .iter()
            .filter(|(did, _)| did.as_str() != self_did)
            .map(|(_, meta)| AgentSummary {
                did: meta.did.clone(),
                name: meta.name.clone(),
                capabilities: meta.capabilities.clone(),
                description: meta.description.clone(),
            })
            .collect();

        Ok(AgentRegistryView {
            total_count: self.metadata.len(),
            agents,
        })
    }

    /// Update agent capabilities
    pub fn update_capabilities(
        &mut self,
        did: &str,
        capabilities: Vec<String>,
    ) -> Result<()> {
        // Remove old capabilities
        self.capabilities.unregister(did);

        // Add new capabilities
        let caps: Vec<String> = capabilities.clone();
        self.capabilities.register(did, &caps);

        // Update metadata
        if let Some(meta) = self.metadata.get_mut(did) {
            meta.capabilities = capabilities;
        }

        debug!("Updated capabilities for {}: {:?}", did, caps);
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl Registry for LocalRegistry {
    async fn register(
        &mut self,
        did: &str,
        name: &str,
    ) -> Result<()> {
        if self.metadata.contains_key(did) {
            return Err(anyhow!("Agent already registered: {}", did));
        }

        // Create metadata
        let meta = AgentMetadata {
            did: did.to_string(),
            name: name.to_string(),
            capabilities: vec![], // Would extract from config
            description: None,
            registered_at: chrono::Utc::now(),
        };

        // Store
        self.metadata.insert(did.to_string(), meta);
        self.name_index.insert(name.to_string(), did.to_string());

        info!("Registered agent: {} ({})", did, self.metadata.len());
        Ok(())
    }

    async fn unregister(
        &mut self,
        did: &str,
    ) -> Result<()> {
        if let Some(meta) = self.metadata.remove(did) {
            self.name_index.remove(&meta.name);
            self.capabilities.unregister(did);
        }

        info!("Unregistered agent: {} ({})", did, self.metadata.len());
        Ok(())
    }

    async fn get(
        &self,
        did: &str,
    ) -> Option<AgentMetadata> {
        self.metadata.get(did).cloned()
    }

    async fn get_by_name(
        &self,
        name: &str,
    ) -> Option<AgentMetadata> {
        self.name_index
            .get(name)
            .and_then(|did| self.metadata.get(did).cloned())
    }

    async fn list(
        &self,
    ) -> Vec<AgentMetadata> {
        self.metadata.values().cloned().collect()
    }

    async fn find_by_capability(
        &self,
        capability: &str,
    ) -> Vec<String> {
        self.capabilities.find(capability)
    }
}

impl Default for LocalRegistry {
    fn default() -> Self {
        Self::new()
    }
}
