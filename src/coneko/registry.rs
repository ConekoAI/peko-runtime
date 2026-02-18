//! Agent registry implementations
//!
//! Provides both local and remote (Coneko) agent registries for agent discovery.

use super::client::AgentInfo;
use crate::types::agent::AgentCapability;
use std::collections::HashMap;

/// Local agent registry (in-memory storage)
pub struct LocalRegistry {
    agents: HashMap<String, AgentInfo>,
}

impl LocalRegistry {
    /// Create a new empty local registry
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    /// Register an agent in the local registry
    pub fn register(&mut self,
        did: &str,
        name: &str,
        endpoint: &str,
        capabilities: Vec<AgentCapability>,
        scope: &str,
        tenant: &str,
    ) {
        let info = AgentInfo {
            did: did.to_string(),
            name: name.to_string(),
            endpoint: endpoint.to_string(),
            capabilities,
            scope: scope.to_string(),
            tenant: tenant.to_string(),
            metadata: None,
        };
        
        self.agents.insert(did.to_string(), info);
    }

    /// Unregister an agent
    pub fn unregister(&mut self, did: &str) -> Option<AgentInfo> {
        self.agents.remove(did)
    }

    /// Get all registered agents
    pub fn list(&self) -> Vec<&AgentInfo> {
        self.agents.values().collect()
    }

    /// Find agent by DID
    pub fn find_by_did(&self, did: &str) -> Option<&AgentInfo> {
        self.agents.get(did)
    }

    /// Find agents by capability
    pub fn find_by_capability(&self,
        capability: &str,
    ) -> Vec<&AgentInfo> {
        self.agents
            .values()
            .filter(|a| {
                a.capabilities.iter().any(|c| c.name == capability)
            })
            .collect()
    }

    /// Find agents by multiple capabilities (ANY match)
    pub fn find_by_any_capability(
        &self,
        capabilities: &[String],
    ) -> Vec<&AgentInfo> {
        self.agents
            .values()
            .filter(|a| {
                a.capabilities.iter().any(|c| {
                    capabilities.contains(&c.name)
                })
            })
            .collect()
    }

    /// Find agents by multiple capabilities (ALL match)
    pub fn find_by_all_capabilities(
        &self,
        capabilities: &[String],
    ) -> Vec<&AgentInfo> {
        self.agents
            .values()
            .filter(|a| {
                let agent_caps: Vec<String> = a
                    .capabilities
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                
                capabilities.iter().all(|c| agent_caps.contains(c))
            })
            .collect()
    }

    /// Find agents by scope
    pub fn find_by_scope(&self,
        scope: &str,
    ) -> Vec<&AgentInfo> {
        self.agents
            .values()
            .filter(|a| a.scope == scope)
            .collect()
    }

    /// Find agents by tenant
    pub fn find_by_tenant(&self,
        tenant: &str,
    ) -> Vec<&AgentInfo> {
        self.agents
            .values()
            .filter(|a| a.tenant == tenant)
            .collect()
    }

    /// Get agent count
    pub fn count(&self) -> usize {
        self.agents.len()
    }

    /// Clear all agents
    pub fn clear(&mut self) {
        self.agents.clear();
    }
}

impl Default for LocalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Unified agent registry that combines local and Coneko sources
pub struct UnifiedRegistry {
    local: LocalRegistry,
    coneko: Option<super::ConekoAdapter>,
}

impl UnifiedRegistry {
    /// Create a new unified registry
    pub fn new(coneko: Option<super::ConekoAdapter>) -> Self {
        Self {
            local: LocalRegistry::new(),
            coneko,
        }
    }

    /// Register a local agent
    pub fn register_local(
        &mut self,
        did: &str,
        name: &str,
        endpoint: &str,
        capabilities: Vec<AgentCapability>,
        scope: &str,
        tenant: &str,
    ) {
        self.local.register(did, name, endpoint, capabilities, scope, tenant);
    }

    /// Discover agents by capability
    /// First checks local registry, then queries Coneko if available
    pub async fn discover(
        &self,
        capability: Option<&str>,
    ) -> anyhow::Result<Vec<AgentInfo>> {
        let mut results = Vec::new();
        let mut seen_dids = std::collections::HashSet::new();

        // Add local agents
        let local_agents = if let Some(cap) = capability {
            self.local.find_by_capability(cap)
        } else {
            self.local.list()
        };

        for agent in local_agents {
            if seen_dids.insert(agent.did.clone()) {
                results.push(agent.clone());
            }
        }

        // Query Coneko if available
        if let Some(ref coneko) = self.coneko {
            let caps = capability.map(|c| vec![c.to_string()]);
            match coneko.discover_agents(caps, None, None).await {
                Ok(remote_agents) => {
                    for agent in remote_agents {
                        if seen_dids.insert(agent.did.clone()) {
                            results.push(agent);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to query Coneko: {}", e);
                }
            }
        }

        Ok(results)
    }

    /// Find agent by DID (checks local first, then Coneko)
    pub async fn find_by_did(
        &self,
        did: &str,
    ) -> anyhow::Result<Option<AgentInfo>> {
        // Check local first
        if let Some(agent) = self.local.find_by_did(did) {
            return Ok(Some(agent.clone()));
        }

        // Query Coneko if available
        if let Some(ref coneko) = self.coneko {
            // TODO: Add specific DID lookup to Coneko API
            // For now, we search and filter
            match coneko.discover_agents(None, None, None).await {
                Ok(agents) => {
                    return Ok(agents.into_iter().find(|a| a.did == did));
                }
                Err(e) => {
                    tracing::warn!("Failed to query Coneko: {}", e);
                }
            }
        }

        Ok(None)
    }

    /// Get local registry reference
    pub fn local(&self) -> &LocalRegistry {
        &self.local
    }

    /// Get mutable local registry
    pub fn local_mut(&mut self) -> &mut LocalRegistry {
        &mut self.local
    }

    /// Check if Coneko is enabled
    pub fn has_coneko(&self) -> bool {
        self.coneko.as_ref().map_or(false, |c| c.is_enabled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_capability(name: &str) -> AgentCapability {
        AgentCapability {
            name: name.to_string(),
            version: "1.0".to_string(),
            description: None,
            parameters: None,
            required_auth: None,
            estimated_cost: None,
            estimated_duration: None,
        }
    }

    #[test]
    fn test_local_registry() {
        let mut registry = LocalRegistry::new();
        
        registry.register(
            "did:pekobot:local:test:agent1",
            "Agent 1",
            "http://localhost:8001",
            vec![create_test_capability("messaging")],
            "local",
            "test",
        );
        
        registry.register(
            "did:pekobot:local:test:agent2",
            "Agent 2",
            "http://localhost:8002",
            vec![
                create_test_capability("messaging"),
                create_test_capability("task_execution"),
            ],
            "local",
            "test",
        );

        assert_eq!(registry.count(), 2);
        
        let messaging = registry.find_by_capability("messaging");
        assert_eq!(messaging.len(), 2);
        
        let task_exec = registry.find_by_capability("task_execution");
        assert_eq!(task_exec.len(), 1);
        
        registry.unregister("did:pekobot:local:test:agent1");
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn test_find_by_all_capabilities() {
        let mut registry = LocalRegistry::new();
        
        registry.register(
            "did:pekobot:local:test:agent1",
            "Agent 1",
            "http://localhost:8001",
            vec![create_test_capability("messaging")],
            "local",
            "test",
        );
        
        registry.register(
            "did:pekobot:local:test:agent2",
            "Agent 2",
            "http://localhost:8002",
            vec![
                create_test_capability("messaging"),
                create_test_capability("task_execution"),
            ],
            "local",
            "test",
        );

        let results = registry.find_by_all_capabilities(&vec![
            "messaging".to_string(),
            "task_execution".to_string(),
        ]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].did, "did:pekobot:local:test:agent2");
    }
}
