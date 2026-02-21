//! Agent management module
//!
//! Note: The agentic loop has been moved to `engine::loop_`.
//! This module re-exports for backward compatibility.

// Re-export from engine for backward compatibility
pub use crate::engine::loop_::{AgenticLoop, AgenticResult, ToolCall};

use crate::a2a::{A2AFlowHandler, A2AProtocol, SharedRegistry};
use crate::identity::{did::DIDScope, storage::KeyStorage, Identity};
use crate::memory::sqlite::SqliteMemory;
use crate::providers::{OpenAIConfig, OpenAIProvider, Provider};
use crate::types::agent::{AgentConfig, AgentState};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::{debug, error, info, warn};

/// Single agent runtime
pub struct Agent {
    /// Agent configuration
    pub config: AgentConfig,
    /// Current state
    state: Arc<RwLock<AgentState>>,
    /// Agent identity
    pub identity: Identity,
    /// Memory store
    memory: Option<SqliteMemory>,
    /// LLM provider
    provider: Option<Box<dyn Provider>>,
}

impl Agent {
    /// Create a new agent with the given configuration
    pub async fn new(config: AgentConfig) -> Result<Self> {
        info!("Creating agent: {}", config.name);

        // Load or create identity
        let identity = Self::load_or_create_identity(&config).await?;

        // Initialize memory if configured
        let memory = Self::init_memory(&config, &identity).await?;

        // Initialize provider if configured
        let provider = Self::init_provider(&config).await?;

        let agent = Self {
            config,
            state: Arc::new(RwLock::new(AgentState::Initializing)),
            identity,
            memory,
            provider,
        };

        agent.set_state(AgentState::Idle);
        info!(
            "Agent {} initialized with DID: {}",
            agent.config.name, agent.identity.did
        );

        Ok(agent)
    }

    /// Start the agent
    pub async fn start(&self) -> Result<()> {
        info!(
            "Starting agent: {} ({})",
            self.config.name, self.identity.did
        );

        self.set_state(AgentState::Idle);

        // Store startup message in memory
        if let Some(memory) = &self.memory {
            let _ = memory.store(
                &format!(
                    "Agent {} started at {}",
                    self.config.name,
                    chrono::Utc::now()
                ),
                Some(serde_json::json!({
                    "event": "startup",
                    "agent_name": self.config.name,
                    "did": self.identity.did,
                })),
            );
        }

        Ok(())
    }

    /// Stop the agent
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping agent: {}", self.config.name);
        self.set_state(AgentState::ShuttingDown);

        // Store shutdown message in memory
        if let Some(memory) = &self.memory {
            let _ = memory.store(
                &format!(
                    "Agent {} stopped at {}",
                    self.config.name,
                    chrono::Utc::now()
                ),
                Some(serde_json::json!({
                    "event": "shutdown",
                    "agent_name": self.config.name,
                })),
            );
        }

        Ok(())
    }

    /// Get current state
    pub fn state(&self) -> AgentState {
        self.state.read().unwrap().clone()
    }

    /// Set state
    fn set_state(&self, state: AgentState) {
        let mut current = self.state.write().unwrap();
        debug!(
            "Agent {} state: {:?} -> {:?}",
            self.config.name, *current, state
        );
        *current = state;
    }

    /// Execute a task with the LLM provider
    pub async fn execute(&self, prompt: &str) -> Result<String> {
        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        // Store the prompt in memory
        if let Some(memory) = &self.memory {
            let _ = memory.store(
                prompt,
                Some(serde_json::json!({
                    "type": "prompt",
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })),
            );
        }

        let result = if let Some(provider) = &self.provider {
            debug!("Executing prompt with provider: {}", provider.name());
            match provider.complete(prompt).await {
                Ok(response) => {
                    // Store the response in memory
                    if let Some(memory) = &self.memory {
                        let _ = memory.store(
                            &response,
                            Some(serde_json::json!({
                                "type": "response",
                                "timestamp": chrono::Utc::now().to_rfc3339(),
                            })),
                        );
                    }
                    Ok(response)
                }
                Err(e) => {
                    error!("Provider error: {}", e);
                    Err(e)
                }
            }
        } else {
            warn!("No provider configured, returning echo response");
            Ok(format!("Echo: {prompt}"))
        };

        self.set_state(AgentState::Idle);
        result
    }

    /// Search memory
    pub fn search_memory(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::types::memory::MemoryEntry>> {
        // Convert from sqlite MemoryEntry to types MemoryEntry
        match &self.memory {
            Some(memory) => {
                let entries = memory.search(query, limit)?;
                // Map to the correct type
                Ok(entries
                    .into_iter()
                    .map(|e| crate::types::memory::MemoryEntry {
                        id: e.id,
                        agent_did: self.did().to_string(),
                        scope: crate::types::memory::MemoryScope::Agent,
                        memory_type: "search_result".to_string(),
                        content: serde_json::json!({"content": e.content}),
                        embedding: None,
                        created_at: e.timestamp,
                        updated_at: e.timestamp,
                        expires_at: None,
                        importance: 0.5,
                        thread_id: None,
                        tags: vec![],
                        source: "sqlite".to_string(),
                    })
                    .collect())
            }
            None => Ok(vec![]),
        }
    }

    /// Store in memory
    pub fn store_memory(
        &self,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        match &self.memory {
            Some(memory) => memory.store(content, metadata),
            None => Err(anyhow::anyhow!("Memory not initialized")),
        }
    }

    /// Create a flow handler for this agent
    pub fn create_flow_handler(&self) -> A2AFlowHandler {
        A2AFlowHandler::new(self.did())
    }

    /// Handle an incoming A2A message
    pub async fn handle_a2a_message(
        &mut self,
        message: &crate::a2a::A2AMessage,
    ) -> Result<Option<crate::a2a::A2AMessage>> {
        info!(
            "Agent {} handling A2A message: {:?}",
            self.name(),
            message.message_type
        );

        // Store the message in memory
        if let Some(memory) = &self.memory {
            let _ = memory.store(
                &format!("A2A message received: {:?}", message.message_type),
                Some(serde_json::json!({
                    "sender": message.sender.did,
                    "message_type": format!("{:?}", message.message_type),
                    "message_id": message.message_id,
                    "thread_id": message.thread_id,
                })),
            );
        }

        // For now, just acknowledge
        Ok(None)
    }

    /// Get agent DID
    pub fn did(&self) -> &str {
        &self.identity.did
    }

    /// Get agent name
    pub fn name(&self) -> &str {
        &self.config.name
    }

    // Private helper methods

    async fn load_or_create_identity(config: &AgentConfig) -> Result<Identity> {
        let storage = KeyStorage::new()?;

        let tenant = config.tenant.as_deref().unwrap_or("default");
        let identity_name = format!("{}-{}", tenant, config.name);

        // Try to load existing identity
        if let Ok(identity) = storage.load(&identity_name) {
            info!("Loaded existing identity: {}", identity.did);
            return Ok(identity);
        }

        // Create new identity
        info!("Creating new identity for: {}", identity_name);
        let identity = Identity::generate(DIDScope::Local, Some(tenant))?;

        storage.store(&identity)?;
        info!("Created and stored new identity: {}", identity.did);

        Ok(identity)
    }

    async fn init_memory(
        config: &AgentConfig,
        identity: &Identity,
    ) -> Result<Option<SqliteMemory>> {
        if let Some(memory_config) = &config.memory {
            let path = memory_config
                .database_path
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
                    path.push("pekobot");
                    path.push("memory.db");
                    path
                });

            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            let namespace = identity.did.replace(':', "_");
            let memory = SqliteMemory::new(&path, &namespace)
                .context("Failed to initialize SQLite memory")?;

            info!("Memory initialized at: {:?}", path);
            Ok(Some(memory))
        } else {
            Ok(None)
        }
    }

    async fn init_provider(config: &AgentConfig) -> Result<Option<Box<dyn Provider>>> {
        use crate::types::provider::ProviderType;

        if config.provider.provider_type == ProviderType::OpenAI {
            let api_key = config.provider.get_api_key()?;

            let model_config = config
                .provider
                .default_model_config()
                .cloned()
                .unwrap_or_default();

            let openai_config = OpenAIConfig {
                api_key,
                base_url: config
                    .provider
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
                model: model_config.name,
                max_tokens: model_config.max_tokens,
                temperature: model_config.temperature,
                timeout_seconds: config.provider.timeout_seconds,
            };

            let provider = OpenAIProvider::new(openai_config)?;
            Ok(Some(Box::new(provider)))
        } else {
            // Other providers not yet implemented
            warn!(
                "Provider type not yet implemented: {:?}",
                config.provider.provider_type
            );
            Ok(None)
        }
    }
}

/// Multi-agent orchestrator with A2A support
pub struct Orchestrator {
    /// A2A Protocol handler
    protocol: Option<A2AProtocol>,
    /// Shared registry reference
    registry: Option<SharedRegistry>,
}

impl Orchestrator {
    /// Create a new orchestrator
    #[must_use]
    pub fn new() -> Self {
        Self {
            protocol: None,
            registry: None,
        }
    }

    /// Create an orchestrator with A2A registry
    pub fn with_registry(registry: SharedRegistry) -> Self {
        let protocol = A2AProtocol::new(registry.clone());
        Self {
            protocol: Some(protocol),
            registry: Some(registry),
        }
    }

    /// Add an agent to the orchestrator
    pub async fn add_agent(&mut self, agent: Agent) -> Result<()> {
        info!("Adding agent to orchestrator: {}", agent.name());

        let did = agent.did().to_string();
        let flow_handler = agent.create_flow_handler();

        // Register in registry if available
        if let Some(registry) = &self.registry {
            registry.register(agent).await?;
        }

        // Register flow handler if protocol is available
        if let Some(protocol) = &mut self.protocol {
            protocol.register_agent_handler(&did, flow_handler);
        }

        Ok(())
    }

    /// Get the A2A protocol handler
    pub fn protocol(&mut self) -> Option<&mut A2AProtocol> {
        self.protocol.as_mut()
    }

    /// Get the registry
    #[must_use]
    pub fn registry(&self) -> Option<&SharedRegistry> {
        self.registry.as_ref()
    }

    /// Start all agents
    pub async fn start_all(&self) -> Result<()> {
        if let Some(registry) = &self.registry {
            registry.start_all().await?;
        }
        Ok(())
    }

    /// Stop all agents
    pub async fn stop_all(&self) -> Result<()> {
        if let Some(registry) = &self.registry {
            registry.stop_all().await?;
        }
        Ok(())
    }

    /// List all registered agents
    pub async fn list_agents(&self) -> Vec<(String, String)> {
        if let Some(registry) = &self.registry {
            registry.list_agents().await
        } else {
            vec![]
        }
    }

    /// Find agent by DID
    pub async fn find_by_did(&self, did: &str) -> Option<crate::a2a::ArcAgent> {
        if let Some(registry) = &self.registry {
            registry.get_by_did(did).await
        } else {
            None
        }
    }

    /// Find agent by name
    pub async fn find_by_name(&self, name: &str) -> Option<crate::a2a::ArcAgent> {
        if let Some(registry) = &self.registry {
            registry.get_by_name(name).await
        } else {
            None
        }
    }

    /// Run the orchestrator with A2A message loop
    pub async fn run(&mut self) -> Result<()> {
        if let (Some(protocol), Some(_registry)) = (&mut self.protocol, &self.registry) {
            use crate::a2a::registry::create_registry;
            // Create a dummy receiver - this is a simplified version
            // In production, you'd pass the actual receiver from create_registry
            let (_, receiver) = create_registry();
            protocol.run(receiver).await?;
        }
        Ok(())
    }
}

impl Default for Orchestrator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::agent::AgentConfig;

    #[tokio::test]
    async fn test_agent_creation() {
        use crate::types::provider::{ProviderConfig, ProviderType};

        let config = AgentConfig {
            name: "test-agent".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama, // Use Ollama which doesn't require API key
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await;
        assert!(agent.is_ok());

        let agent = agent.unwrap();
        assert_eq!(agent.name(), "test-agent");
        assert!(agent.did().starts_with("did:pekobot:"));
    }

    #[test]
    fn test_orchestrator() {
        let orch = Orchestrator::new();
        // Orchestrator without registry has no agents
        assert!(orch.registry().is_none());
    }

    #[tokio::test]
    async fn test_orchestrator_with_registry() {
        use crate::a2a::registry::create_registry;
        use crate::types::provider::{ProviderConfig, ProviderType};

        let (registry, _receiver) = create_registry();
        let mut orch = Orchestrator::with_registry(registry);

        // Create and add a test agent
        let config = AgentConfig {
            name: "test-agent".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama, // Use Ollama which doesn't require API key
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();
        orch.add_agent(agent).await.unwrap();

        // Verify agent is registered
        let agents = orch.list_agents().await;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].1, "test-agent");
    }
}
