//! Agent Manager - Unified agent lifecycle and coordination
//!
//! Provides decentralized agent management:
//! - Agent lifecycle (spawn, start, stop, restart)
//! - Agent pool management
//! - Discovery (who's available, what can they do)
//! - Context sharing (agents know about each other)
//! - Capability registry (local)
//! - Identity management
//! - Portable agent (export/import)
//!
//! Design principle: Agents coordinate themselves. The manager just provides
//! the context and messaging infrastructure.

pub mod context;
pub mod lifecycle;
pub mod pool;
pub mod registry;

pub use context::{AgentContext, AgentRegistryView, CapabilityIndex};
pub use lifecycle::{LifecycleManager, LifecycleState, LifecycleTransition};
pub use pool::{AgentHandle, AgentPool, PoolAgentInfo, PoolConfig};
pub use registry::{CapabilityRecord, LocalRegistry, Registry, RegistryEvent};

use crate::agent::Agent;
use crate::engine::state::AgentState;
use crate::identity::{storage::KeyStorage, Identity};
use crate::portable::{ExportOptions, ImportOptions, Packager, Unpackager};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

/// Unified agent manager
pub struct AgentManager {
    /// Agent pool (running agents)
    pool: Arc<RwLock<AgentPool>>,
    /// Local registry (agent metadata + capabilities)
    registry: Arc<RwLock<LocalRegistry>>,
    /// Identity storage
    identity_storage: Arc<RwLock<KeyStorage>>,
    /// Lifecycle manager
    lifecycle: LifecycleManager,
    /// Event channel
    events: mpsc::Sender<ManagerEvent>,
    /// Data directory
    data_dir: PathBuf,
}

/// Events emitted by the manager
#[derive(Debug, Clone)]
pub enum ManagerEvent {
    /// Agent spawned
    AgentSpawned { did: String, name: String },
    /// Agent started
    AgentStarted { did: String },
    /// Agent stopped
    AgentStopped { did: String },
    /// Agent crashed
    AgentCrashed { did: String, error: String },
    /// Agent registered (discovered)
    AgentDiscovered { did: String, capabilities: Vec<String> },
    /// Context updated
    ContextUpdated { did: String },
    /// Agent exported
    AgentExported { did: String, path: PathBuf },
    /// Agent imported
    AgentImported { did: String, name: String },
}

/// Agent info with full details
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// Agent DID
    pub did: String,
    /// Agent name
    pub name: String,
    /// Current state
    pub state: AgentState,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Uptime (seconds)
    pub uptime_secs: u64,
    /// Identity info
    pub identity_info: IdentityInfo,
}

/// Identity information
#[derive(Debug, Clone)]
pub struct IdentityInfo {
    /// DID
    pub did: String,
    /// Scope (local, tenant, global)
    pub scope: String,
    /// Created at
    pub created_at: Option<String>,
}

impl AgentManager {
    /// Create a new agent manager
    pub async fn new() -> Result<(Self, mpsc::Receiver<ManagerEvent>)> {
        let (events_tx, events_rx) = mpsc::channel(100);
        
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pekobot");
        
        std::fs::create_dir_all(&data_dir)?;

        let pool = Arc::new(RwLock::new(AgentPool::new()));
        let registry = Arc::new(RwLock::new(LocalRegistry::new()));
        let identity_storage = Arc::new(RwLock::new(KeyStorage::new()?));
        let lifecycle = LifecycleManager::new();

        Ok((
            Self {
                pool,
                registry,
                identity_storage,
                lifecycle,
                events: events_tx,
                data_dir,
            },
            events_rx,
        ))
    }

    /// Create with custom data directory
    pub async fn with_data_dir(data_dir: PathBuf) -> Result<(Self, mpsc::Receiver<ManagerEvent>)> {
        let (events_tx, events_rx) = mpsc::channel(100);
        
        std::fs::create_dir_all(&data_dir)?;

        let pool = Arc::new(RwLock::new(AgentPool::new()));
        let registry = Arc::new(RwLock::new(LocalRegistry::new()));
        let identity_storage = Arc::new(RwLock::new(KeyStorage::new()?));
        let lifecycle = LifecycleManager::new();

        Ok((
            Self {
                pool,
                registry,
                identity_storage,
                lifecycle,
                events: events_tx,
                data_dir,
            },
            events_rx,
        ))
    }

    /// Spawn a new agent (create + start)
    pub async fn spawn(
        &self,
        config: crate::types::agent::AgentConfig,
    ) -> Result<AgentHandle> {
        info!("Spawning agent: {}", config.name);

        // Create agent
        let agent = Agent::new(config).await?;
        let did = agent.did().to_string();
        let name = agent.name().to_string();

        // Register in registry
        {
            let mut reg = self.registry.write().await;
            reg.register(&did, &name).await?;

            // Register capabilities
            for cap in &agent.config.capabilities {
                reg.register_capability(CapabilityRecord {
                    agent_did: did.clone(),
                    capability: cap.name.clone(),
                    version: cap.version.clone(),
                    metadata: serde_json::json!({
                        "description": cap.description,
                        "estimated_cost": cap.estimated_cost,
                    }),
                })?;
            }
        }

        // Add to pool
        let handle = {
            let mut pool = self.pool.write().await;
            pool.add(Arc::new(agent)).await?
        };

        // Start the agent
        self.lifecycle.start(&did).await?;
        handle.start().await?;

        // Emit event
        let _ = self.events.send(ManagerEvent::AgentSpawned { did, name }).await;

        Ok(handle)
    }

    /// Stop an agent
    pub async fn stop(&self, did: &str) -> Result<()> {
        info!("Stopping agent: {}", did);

        self.lifecycle.stop(did).await?;

        {
            let mut pool = self.pool.write().await;
            pool.stop(did).await?;
        }

        let _ = self.events.send(ManagerEvent::AgentStopped {
            did: did.to_string(),
        }).await;

        Ok(())
    }

    /// Get agent by DID
    pub async fn get(&self, did: &str) -> Option<AgentHandle> {
        let pool = self.pool.read().await;
        pool.get(did).await
    }

    /// Get agent by name
    pub async fn get_by_name(&self, name: &str) -> Option<AgentHandle> {
        let pool = self.pool.read().await;
        pool.get_by_name(name).await
    }

    /// List all agents
    pub async fn list(&self) -> Vec<AgentInfo> {
        let pool = self.pool.read().await;
        let basic_list = pool.list().await;
        
        // Enhance with identity info
        let mut infos = vec![];
        for info in basic_list {
            let identity_info = IdentityInfo {
                did: info.did.clone(),
                scope: "local".to_string(),
                created_at: None,
            };
            
            // Get capabilities from registry
            let reg = self.registry.read().await;
            let capabilities = reg.get(&info.did).await
                .map(|m| m.capabilities)
                .unwrap_or_default();
            
            infos.push(AgentInfo {
                did: info.did,
                name: info.name,
                state: info.state,
                capabilities,
                uptime_secs: info.uptime_secs,
                identity_info,
            });
        }
        
        infos
    }

    /// Get agents by capability
    pub async fn find_by_capability(
        &self,
        capability: &str,
    ) -> Vec<AgentHandle> {
        let reg = self.registry.read().await;
        let dids = reg.find_by_capability(capability);
        drop(reg);

        let mut handles = vec![];
        for did in dids {
            if let Some(handle) = self.get(&did).await {
                handles.push(handle);
            }
        }
        handles
    }

    /// Get context for an agent (what it knows about other agents)
    pub async fn get_context(&self, did: &str) -> Result<AgentContext> {
        let reg = self.registry.read().await;
        let view = reg.get_view(did)?;

        let pool = self.pool.read().await;
        let states = pool.get_states().await;

        Ok(AgentContext {
            self_did: did.to_string(),
            registry_view: view,
            agent_states: states,
        })
    }

    /// Export an agent to a .agent package
    pub async fn export_agent(
        &self,
        did: &str,
        options: ExportOptions,
    ) -> Result<PathBuf> {
        info!("Exporting agent: {}", did);

        let agent = self.get(did).await
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", did))?;

        // Get agent's config and identity
        // For now, we need to reconstruct these from the agent
        // In a real implementation, we'd store these references
        
        // Create packager with the agent's info
        // Note: This is a simplified version - in practice we'd need
        // to properly extract config and identity from the agent
        let config = crate::types::agent::AgentConfig::default();
        let identity = self.get_or_create_identity(&agent.name(), None).await?;
        
        let packager = Packager::new(
            config,
            identity,
            None, // memory_path
        );
        
        let path = packager.export(options).await?;

        let _ = self.events.send(ManagerEvent::AgentExported {
            did: did.to_string(),
            path: path.clone(),
        }).await;

        Ok(path)
    }

    /// Import an agent from a .agent package
    pub async fn import_agent(
        &self,
        file_path: &str,
        options: ImportOptions,
    ) -> Result<AgentHandle> {
        info!("Importing agent from: {}", file_path);

        let unpackager = Unpackager::new(file_path)
            .with_base_dir(&self.data_dir);
        
        let result = unpackager.import(options).await?;
        
        let did = result.did;
        let name = result.name;

        // Register in registry
        {
            let mut reg = self.registry.write().await;
            reg.register(&did, &name).await?;
        }

        // Note: The agent was already created by the unpackager
        // We just need to add it to our pool
        // For now, this is a simplified version

        let _ = self.events.send(ManagerEvent::AgentImported {
            did,
            name,
        }).await;

        // Return a handle - in practice we'd need to get the actual agent
        Err(anyhow::anyhow!("Import creates agent directly - needs integration"))
    }

    /// Get or create identity for an agent
    pub async fn get_or_create_identity(
        &self,
        name: &str,
        tenant: Option<&str>,
    ) -> Result<Identity> {
        let storage = self.identity_storage.read().await;
        
        let tenant = tenant.unwrap_or("default");
        let identity_name = format!("{}-{}", tenant, name);

        // Try to load existing
        if let Ok(identity) = storage.load(&identity_name) {
            info!("Loaded existing identity: {}", identity.did);
            return Ok(identity);
        }

        // Create new
        drop(storage);
        let mut storage = self.identity_storage.write().await;
        
        info!("Creating new identity for: {}", identity_name);
        let identity = Identity::generate(
            crate::identity::did::DIDScope::Local,
            Some(tenant),
        )?;

        storage.store(&identity)?;
        info!("Created and stored new identity: {}", identity.did);

        Ok(identity)
    }

    /// List all capabilities in the system
    pub async fn list_capabilities(&self) -> Vec<String> {
        let reg = self.registry.read().await;
        reg.list_capabilities()
    }

    /// Get capability details
    pub async fn get_capability_details(&self, capability: &str) -> Vec<CapabilityRecord> {
        let reg = self.registry.read().await;
        reg.get_capability(capability)
    }

    /// Broadcast message to all agents
    pub async fn broadcast(&self, message: &str) -> Result<()> {
        let pool = self.pool.read().await;
        pool.broadcast(message).await
    }

    /// Send message to specific agent
    pub async fn send(&self, target_did: &str, message: &str) -> Result<()> {
        if let Some(handle) = self.get(target_did).await {
            handle.send(message).await
        } else {
            Err(anyhow::anyhow!("Agent not found: {}", target_did))
        }
    }

    /// Stop all agents and shutdown
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down agent manager");

        let pool = self.pool.read().await;
        let agents = pool.list().await;
        drop(pool);

        for agent in agents {
            if let Err(e) = self.stop(&agent.did).await {
                warn!("Error stopping agent {}: {}", agent.did, e);
            }
        }

        info!("Agent manager shutdown complete");
        Ok(())
    }

    /// Get data directory
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }
}
