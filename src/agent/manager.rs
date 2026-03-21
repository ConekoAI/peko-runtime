//! Agent manager core implementation (DEPRECATED - Use StatelessAgentManager)
#![allow(deprecated)]

use crate::agent::Agent;
use crate::agent::{
    lifecycle::LifecycleManager,
    pool::{AgentHandle, AgentPool},
    registry::{CapabilityRecord, LocalRegistry, Registry as _},
    types::{AgentInfo, ManagerEvent},
};
use crate::identity::{storage::KeyStorage, Identity};
use crate::tools::ManagerCommand;
use anyhow::Result;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};

use tracing::{info, warn};

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
    /// Command channel for agent tools
    command_tx: mpsc::Sender<ManagerCommand>,
}

impl AgentManager {
    /// Create a new agent manager
    pub async fn new() -> Result<(Self, mpsc::Receiver<ManagerEvent>)> {
        let (events_tx, events_rx) = mpsc::channel(100);
        let (command_tx, _command_rx) = mpsc::channel(100);

        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pekobot");

        std::fs::create_dir_all(&data_dir)?;

        let pool = Arc::new(RwLock::new(AgentPool::new()));
        let registry = Arc::new(RwLock::new(LocalRegistry::new()));
        let identity_storage = Arc::new(RwLock::new(KeyStorage::new()?));
        let lifecycle = LifecycleManager::new();

        let manager = Self {
            pool,
            registry,
            identity_storage,
            lifecycle,
            events: events_tx,
            data_dir,
            command_tx,
        };

        Ok((manager, events_rx))
    }

    /// Create with custom data directory
    pub async fn with_data_dir(data_dir: PathBuf) -> Result<(Self, mpsc::Receiver<ManagerEvent>)> {
        let (events_tx, events_rx) = mpsc::channel(100);
        let (command_tx, _command_rx) = mpsc::channel(100);

        std::fs::create_dir_all(&data_dir)?;

        let pool = Arc::new(RwLock::new(AgentPool::new()));
        let registry = Arc::new(RwLock::new(LocalRegistry::new()));
        let identity_storage = Arc::new(RwLock::new(KeyStorage::new()?));
        let lifecycle = LifecycleManager::new();

        let manager = Self {
            pool,
            registry,
            identity_storage,
            lifecycle,
            events: events_tx,
            data_dir,
            command_tx,
        };

        Ok((manager, events_rx))
    }

    /// Spawn a new agent (create + start)
    pub async fn spawn(&self, config: crate::types::agent::AgentConfig) -> Result<AgentHandle> {
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
        handle.start()?;

        // Emit event
        let _ = self
            .events
            .send(ManagerEvent::AgentSpawned { did, name })
            .await;

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

        // Remove from registry
        {
            let mut reg = self.registry.write().await;
            reg.unregister(did).await?;
        }

        // Emit event
        let _ = self
            .events
            .send(ManagerEvent::AgentStopped {
                did: did.to_string(),
            })
            .await;

        Ok(())
    }

    /// Get an agent handle by DID
    pub async fn get(&self, did: &str) -> Option<AgentHandle> {
        let pool = self.pool.read().await;
        pool.get(did)
    }

    /// Get an agent handle by name
    pub async fn get_by_name(&self, name: &str) -> Option<AgentHandle> {
        let pool = self.pool.read().await;
        pool.get_by_name(name)
    }

    /// List all agents
    pub async fn list_agents(&self) -> Vec<AgentInfo> {
        let pool = self.pool.read().await;
        let pool_agents = pool.list();

        pool_agents
            .into_iter()
            .map(|a| AgentInfo {
                did: a.did.clone(),
                name: a.name.clone(),
                state: a.state,
                capabilities: vec![], // Would need to get from registry
                uptime_secs: a.uptime_secs,
                identity_info: crate::agent::types::IdentityInfo {
                    did: a.did,
                    scope: "local".to_string(),
                    created_at: None,
                },
                image_ref: None,
                image_digest: None,
                role: None,
                team_id: None,
                active_session_id: None,
                created_at: chrono::Utc::now(),
                skills: None,
            })
            .collect()
    }

    /// Load or create identity for an agent
    pub async fn get_or_create_identity(
        &self,
        name: &str,
        tenant: Option<&str>,
    ) -> Result<Identity> {
        let storage = self.identity_storage.read().await;

        let tenant = tenant.unwrap_or("default");
        let identity_name = format!("{tenant}-{name}");

        // Try to load existing
        if let Ok(identity) = storage.load(&identity_name) {
            info!("Loaded existing identity: {}", identity.did);
            return Ok(identity);
        }

        // Create new
        drop(storage);
        let storage = self.identity_storage.write().await;

        info!("Creating new identity for: {}", identity_name);
        let identity = Identity::generate(crate::identity::did::DIDScope::Local, Some(tenant))?;

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

    /// Send message to specific agent
    pub async fn send(&self, target_did: &str, message: &str) -> Result<()> {
        if let Some(handle) = self.get(target_did).await {
            handle.send(message)
        } else {
            Err(anyhow::anyhow!("Agent not found: {target_did}"))
        }
    }

    /// Stop all agents and shutdown
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down agent manager");

        let pool = self.pool.read().await;
        let agents = pool.list();
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
    #[must_use]
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Create agent communication tools
    #[must_use]
    pub fn create_communication_tools(&self, _agent_did: &str) -> Vec<Arc<dyn crate::tools::Tool>> {
        use crate::tools::{AgentInfoTool, AgentsListTool};
        use std::sync::Arc;

        let mut tools: Vec<Arc<dyn crate::tools::Tool>> = vec![];

        // Agents list tool (standalone agent - no team)
        tools.push(Arc::new(AgentsListTool::new(
            self.command_tx.clone(),
            None,  // team_id
            false, // allow_cross_team
        )));

        // Agent info tool (standalone agent - no team)
        tools.push(Arc::new(AgentInfoTool::new(
            self.command_tx.clone(),
            None,  // team_id
            false, // allow_cross_team
        )));

        // Agent spawn tool
        // AgentSpawnTool is registered by the agent module with SubagentExecutor

        // Note: sessions_send tool for A2A is registered by the agent module

        tools
    }

    /// Create all essential tools for an agent, filtered by agent config
    ///
    /// This async version includes MCP tools from configured MCP servers.
    pub async fn create_all_tools_for_agent(
        &self,
        agent: &crate::agent::Agent,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        use crate::tools::{ToolFactory, ToolFactoryConfig};

        // Create essential tools using factory (async version includes MCP)
        let factory_config = ToolFactoryConfig {
            workspace_dir: self.data_dir.clone(),
            cron_db_path: Some(self.data_dir.join("cron.db")),
            ..Default::default()
        };
        let result = ToolFactory::create_tools_async(&factory_config).await?;
        let mut tools = result.tools;

        // Add communication tools
        tools.extend(self.create_communication_tools(&agent.identity.did));

        // Filter tools based on agent config if specified
        if let Some(ref tool_config) = agent.config.tools {
            if !tool_config.enabled.is_empty() {
                tools = self.filter_tools(tools, &tool_config.enabled);
            }
        }

        Ok(tools)
    }

    /// Filter tools to only include enabled ones
    fn filter_tools(
        &self,
        tools: Vec<Arc<dyn crate::tools::Tool>>,
        enabled: &[String],
    ) -> Vec<Arc<dyn crate::tools::Tool>> {
        tools
            .into_iter()
            .filter(|tool| enabled.contains(&tool.name().to_string()))
            .collect()
    }

    /// Create all essential tools for an agent (legacy method - use `create_all_tools_for_agent_async`)
    #[must_use]
    pub fn create_all_tools(&self, agent_did: &str) -> Vec<Arc<dyn crate::tools::Tool>> {
        use crate::tools::{ToolFactory, ToolFactoryConfig};

        // Create essential tools using factory (sync version - no MCP)
        let factory_config = ToolFactoryConfig {
            workspace_dir: self.data_dir.clone(),
            cron_db_path: Some(self.data_dir.join("cron.db")),
            ..Default::default()
        };
        let result = ToolFactory::create_tools(&factory_config);
        let mut tools = result.tools;

        // Add communication tools
        tools.extend(self.create_communication_tools(agent_did));

        tools
    }

    /// Create all essential tools for an agent including MCP tools
    pub async fn create_all_tools_async(
        &self,
        agent_did: &str,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        use crate::tools::{ToolFactory, ToolFactoryConfig};

        // Create essential tools using factory (async version includes MCP)
        let factory_config = ToolFactoryConfig {
            workspace_dir: self.data_dir.clone(),
            cron_db_path: Some(self.data_dir.join("cron.db")),
            ..Default::default()
        };
        let result = ToolFactory::create_tools_async(&factory_config).await?;
        let mut tools = result.tools;

        // Add communication tools
        tools.extend(self.create_communication_tools(agent_did));

        Ok(tools)
    }

    /// Create tools with custom configuration
    #[must_use]
    pub fn create_tools_with_config(
        &self,
        agent_did: &str,
        config: crate::tools::ToolFactoryConfig,
        tool_config: Option<&crate::types::agent::ToolConfig>,
    ) -> Vec<Arc<dyn crate::tools::Tool>> {
        use crate::tools::ToolFactory;

        let result = ToolFactory::create_tools(&config);
        let mut tools = result.tools;
        tools.extend(self.create_communication_tools(agent_did));

        // Filter if tool config specified
        if let Some(tc) = tool_config {
            if !tc.enabled.is_empty() {
                return self.filter_tools(tools, &tc.enabled);
            }
        }

        tools
    }

    /// Create tools with custom configuration including MCP tools
    pub async fn create_tools_with_config_async(
        &self,
        agent_did: &str,
        config: crate::tools::ToolFactoryConfig,
        tool_config: Option<&crate::types::agent::ToolConfig>,
    ) -> anyhow::Result<Vec<Arc<dyn crate::tools::Tool>>> {
        use crate::tools::ToolFactory;

        let result = ToolFactory::create_tools_async(&config).await?;
        let mut tools = result.tools;
        tools.extend(self.create_communication_tools(agent_did));

        // Filter if tool config specified
        if let Some(tc) = tool_config {
            if !tc.enabled.is_empty() {
                return Ok(self.filter_tools(tools, &tc.enabled));
            }
        }

        Ok(tools)
    }
}
