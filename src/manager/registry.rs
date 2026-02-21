//! Local Registry - Agent metadata and discovery with capabilities

use crate::manager::context::{AgentRegistryView, AgentSummary, CapabilityIndex};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

/// Registry trait for agent discovery
#[async_trait::async_trait(?Send)]
pub trait Registry {
    /// Register an agent
    async fn register(&mut self, did: &str, name: &str) -> Result<()>;

    /// Unregister an agent
    async fn unregister(&mut self, did: &str) -> Result<()>;

    /// Get metadata by DID
    async fn get(&self, did: &str) -> Option<AgentMetadata>;

    /// Get metadata by name
    async fn get_by_name(&self, name: &str) -> Option<AgentMetadata>;

    /// List all agents
    async fn list(&self) -> Vec<AgentMetadata>;

    /// Find by capability
    async fn find_by_capability(&self, capability: &str) -> Vec<String>;
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

/// Capability record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRecord {
    /// Agent DID
    pub agent_did: String,
    /// Capability name
    pub capability: String,
    /// Version
    pub version: String,
    /// Metadata
    pub metadata: serde_json::Value,
}

/// Local in-memory registry
#[derive(Debug)]
pub struct LocalRegistry {
    /// Metadata by DID
    metadata: HashMap<String, AgentMetadata>,
    /// Capability index: capability -> DIDs
    capability_index: HashMap<String, Vec<String>>,
    /// Full capability records
    capability_records: HashMap<String, Vec<CapabilityRecord>>,
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
    CapabilitiesUpdated {
        did: String,
        capabilities: Vec<String>,
    },
}

impl LocalRegistry {
    /// Create new registry
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            capability_index: HashMap::new(),
            capability_records: HashMap::new(),
            name_index: HashMap::new(),
        }
    }

    /// Get a filtered view for an agent (doesn't include self)
    pub fn get_view(&self, self_did: &str) -> Result<AgentRegistryView> {
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

    /// Register a capability for an agent
    pub fn register_capability(&mut self, record: CapabilityRecord) -> Result<()> {
        let did = record.agent_did.clone();
        let cap = record.capability.clone();

        // Update agent metadata first (before cap is moved)
        if let Some(meta) = self.metadata.get_mut(&did) {
            if !meta.capabilities.contains(&cap) {
                meta.capabilities.push(cap.clone());
            }
        }

        // Add to capability index
        self.capability_index
            .entry(cap.clone())
            .or_default()
            .push(did.clone());

        // Add to records (cap is moved here)
        self.capability_records
            .entry(cap.clone())
            .or_default()
            .push(record);

        debug!("Registered capability {} for {}", cap, did);
        Ok(())
    }

    /// Find agents by capability
    pub fn find_by_capability(&self, capability: &str) -> Vec<String> {
        self.capability_index
            .get(capability)
            .cloned()
            .unwrap_or_default()
    }

    /// Get capability details
    pub fn get_capability(&self, capability: &str) -> Vec<CapabilityRecord> {
        self.capability_records
            .get(capability)
            .cloned()
            .unwrap_or_default()
    }

    /// List all capabilities
    pub fn list_capabilities(&self) -> Vec<String> {
        self.capability_index.keys().cloned().collect()
    }

    /// Update agent capabilities
    pub fn update_capabilities(&mut self, did: &str, capabilities: Vec<String>) -> Result<()> {
        // Remove old capabilities from index
        if let Some(meta) = self.metadata.get(did) {
            for cap in &meta.capabilities {
                if let Some(dids) = self.capability_index.get_mut(cap) {
                    dids.retain(|d| d != did);
                }
            }
        }

        // Add new capabilities to index
        for cap in &capabilities {
            self.capability_index
                .entry(cap.clone())
                .or_default()
                .push(did.to_string());
        }

        // Update metadata
        if let Some(meta) = self.metadata.get_mut(did) {
            meta.capabilities = capabilities;
        }

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl Registry for LocalRegistry {
    async fn register(&mut self, did: &str, name: &str) -> Result<()> {
        if self.metadata.contains_key(did) {
            return Err(anyhow!("Agent already registered: {}", did));
        }

        let meta = AgentMetadata {
            did: did.to_string(),
            name: name.to_string(),
            capabilities: vec![],
            description: None,
            registered_at: chrono::Utc::now(),
        };

        self.metadata.insert(did.to_string(), meta);
        self.name_index.insert(name.to_string(), did.to_string());

        info!("Registered agent: {} ({})", did, self.metadata.len());
        Ok(())
    }

    async fn unregister(&mut self, did: &str) -> Result<()> {
        if let Some(meta) = self.metadata.remove(did) {
            self.name_index.remove(&meta.name);

            // Remove from capability index
            for cap in &meta.capabilities {
                if let Some(dids) = self.capability_index.get_mut(cap) {
                    dids.retain(|d| d != did);
                }
            }
        }

        info!("Unregistered agent: {} ({})", did, self.metadata.len());
        Ok(())
    }

    async fn get(&self, did: &str) -> Option<AgentMetadata> {
        self.metadata.get(did).cloned()
    }

    async fn get_by_name(&self, name: &str) -> Option<AgentMetadata> {
        self.name_index
            .get(name)
            .and_then(|did| self.metadata.get(did).cloned())
    }

    async fn list(&self) -> Vec<AgentMetadata> {
        self.metadata.values().cloned().collect()
    }

    async fn find_by_capability(&self, capability: &str) -> Vec<String> {
        self.find_by_capability(capability)
    }
}

impl Default for LocalRegistry {
    fn default() -> Self {
        Self::new()
    }
}
