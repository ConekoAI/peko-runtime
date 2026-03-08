//! Agent management module

use crate::identity::{did::DIDScope, storage::KeyStorage, Identity};
use crate::memory::sqlite::SqliteMemory;
use crate::providers::Provider;
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
    /// LLM provider (stored in Arc for sharing with agentic loop)
    provider: Option<Arc<dyn Provider>>,
}

impl Agent {
    /// Create tools for this agent based on configuration
    fn create_tools(&self) -> Vec<Arc<dyn crate::tools::Tool>> {
        use crate::tools::*;

        // Create all available tools
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(WebSearchTool::new(WebSearchConfig::default())),
            Arc::new(FetchTool::new(FetchConfig::default())),
            Arc::new(FileSystemTool::new()),
            Arc::new(ProcessTool::new()),
        ];

        // Add session introspection tools
        let session_registry =
            InMemorySessionRegistry::new(format!("agent:{}:cli:default", self.config.name));
        tools.push(Arc::new(SessionStatusTool::new(Box::new(session_registry))));

        // Filter based on agent config if specified
        if let Some(ref tool_config) = self.config.tools {
            if !tool_config.enabled.is_empty() {
                tools = tools
                    .into_iter()
                    .filter(|tool| tool_config.enabled.contains(&tool.name().to_string()))
                    .collect();
            }
        }

        tools
    }

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
            state: Arc::new(RwLock::new(AgentState::Idle)),
            identity,
            memory,
            provider,
        };

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

    /// Get provider reference
    pub fn get_provider(&self) -> Option<&dyn Provider> {
        self.provider.as_deref()
    }

    /// Set state
    /// Set agent state (public for channel use)
    pub fn set_state(&self, state: AgentState) {
        let mut current = self.state.write().unwrap();
        debug!(
            "Agent {} state: {:?} -> {:?}",
            self.config.name, *current, state
        );
        *current = state;
    }

    /// Execute a task with the LLM provider using the unified callback API.
    ///
    /// The `on_event` callback receives streaming events (thinking tokens,
    /// tool calls, lifecycle events, etc.). Use `|_| {}` if you don't need
    /// streaming updates.
    ///
    /// Returns the final result including the answer and tool calls made.
    pub async fn execute(
        &self,
        prompt: &str,
        on_event: impl Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    ) -> Result<crate::engine::AgenticResultV4> {
        use crate::engine::loop_v4::AgenticLoopV4;

        use std::sync::Arc;

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        let result = if let Some(provider) = &self.provider {
            let tools = self.create_tools();

            let supports_native = provider.supports_native_tools();
            info!(
                "Executing with {} tool calling",
                if supports_native {
                    "native"
                } else {
                    "text-based"
                }
            );

            let agent_arc = Arc::new(self.clone_for_loop());
            let provider_arc = Arc::clone(provider);

            let loop_ = AgenticLoopV4::new(agent_arc, provider_arc, tools);

            match loop_.run(prompt, on_event).await {
                Ok(result) => Ok(result),
                Err(e) => {
                    error!("Agentic loop V4 error: {}", e);
                    Err(e)
                }
            }
        } else {
            Err(anyhow::anyhow!("No provider configured"))
        };

        self.set_state(AgentState::Idle);
        result
    }

    /// Execute with a channel-based event interface.
    ///
    /// Convenience wrapper around `execute()` that returns a receiver for
    /// async event streaming. Use this when you need to process events
    /// in a separate async context.
    pub async fn execute_streaming(
        &self,
        prompt: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>> {
        // Use a large buffer to prevent event loss during bursts
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(10000);

        // Spawn the execution in a task
        let prompt = prompt.to_string();
        let agent_arc = Arc::new(self.clone_for_loop());

        // We need to get the provider and tools into the task
        if let Some(provider) = &self.provider {
            let tools = self.create_tools();

            let provider_arc = Arc::clone(provider);
            let event_tx_clone = event_tx.clone();

            tokio::task::spawn_local(async move {
                use crate::engine::loop_v4::AgenticLoopV4;

                let loop_ = AgenticLoopV4::new(agent_arc, provider_arc, tools);

                let _result = loop_
                    .run(&prompt, move |event| {
                        // Try to send event - log if dropped (buffer full means consumer is slow)
                        if let Err(_) = event_tx_clone.try_send(event) {
                            warn!("Agent event dropped (channel full)");
                        }
                    })
                    .await;
            });
        } else {
            return Err(anyhow::anyhow!("No provider configured"));
        }

        Ok(event_rx)
    }

    /// Execute with streaming support, optionally resuming from an existing session.
    ///
    /// If `existing_session` is provided, the agent will resume from that session
    /// with the given history. This enables conversation continuity across
    /// separate command invocations.
    pub async fn execute_streaming_with_session(
        &self,
        prompt: &str,
        existing_session: Option<crate::engine::SimpleSession>,
        history: Option<Vec<crate::providers::ChatMessage>>,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>> {
        // Use a large buffer to prevent event loss during bursts
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(10000);

        // Spawn the execution in a task
        let prompt = prompt.to_string();
        let agent_arc = Arc::new(self.clone_for_loop());

        // We need to get the provider and tools into the task
        if let Some(provider) = &self.provider {
            let tools = self.create_tools();

            let provider_arc = Arc::clone(provider);
            let event_tx_clone = event_tx.clone();

            tokio::task::spawn_local(async move {
                use crate::engine::loop_v4::AgenticLoopV4;

                let loop_ = AgenticLoopV4::new(agent_arc, provider_arc, tools);

                let _result = loop_
                    .run_with_resume(
                        &prompt,
                        move |event| {
                            // Try to send event - log if dropped (buffer full means consumer is slow)
                            if let Err(_) = event_tx_clone.try_send(event) {
                                warn!("Agent event dropped (channel full)");
                            }
                        },
                        existing_session,
                        history,
                    )
                    .await;
            });
        } else {
            return Err(anyhow::anyhow!("No provider configured"));
        }

        Ok(event_rx)
    }

    /// Clone the agent for use in the agentic loop
    /// This creates a shallow copy that shares the same identity
    fn clone_for_loop(&self) -> Self {
        Self {
            config: self.config.clone(),
            state: Arc::new(RwLock::new(AgentState::Busy)),
            identity: Identity {
                did: self.identity.did.clone(),
                document: self.identity.document.clone(),
                keypair: None, // Don't clone keypair to avoid security issues
            },
            memory: None,   // Don't clone memory to avoid lock issues
            provider: None, // Provider passed separately to avoid double-Arc
        }
    }

    /// Execute with streaming support
    ///
    /// Returns a channel receiver for AgenticEvents. The caller can
    /// display these events as they arrive for a responsive UI.
    ///
    /// Note: This method must be called within a tokio::task::LocalSet
    /// because Agent contains non-Send types (rusqlite connections).
    /// Search memory
    pub fn search_memory(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::types::memory::MemoryEntry>> {
        match &self.memory {
            Some(memory) => {
                let entries = memory.search(query, limit)?;
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

    async fn init_provider(config: &AgentConfig) -> Result<Option<Arc<dyn Provider>>> {
        use crate::providers::registry::create_provider;
        use crate::types::provider::ProviderType;

        // Map ProviderType to string for registry lookup
        let provider_name = match config.provider.provider_type {
            ProviderType::OpenAI => "openai",
            ProviderType::Anthropic => "anthropic",
            ProviderType::Kimi => "kimi",
            ProviderType::KimiCode => "kimi_code", // Use kimi_code metadata (Anthropic API)
            ProviderType::Ollama => "ollama",
            ProviderType::OpenAICompatible => {
                // Use base_url to determine provider
                if let Some(ref url) = config.provider.base_url {
                    if url.contains("moonshot.cn") || url.contains("kimi") {
                        "kimi"
                    } else if url.contains("groq") {
                        "groq"
                    } else if url.contains("together") {
                        "together"
                    } else if url.contains("fireworks") {
                        "fireworks"
                    } else {
                        "openai"
                    }
                } else {
                    "openai"
                }
            }
            _ => "openai", // Default fallback
        };

        // Get the provider type for the registry
        let provider_type = match provider_name {
            "openai" => ProviderType::OpenAI,
            "anthropic" => ProviderType::Anthropic,
            "kimi" => ProviderType::Kimi,
            "kimi_code" => ProviderType::KimiCode,
            "ollama" => ProviderType::Ollama,
            _ => ProviderType::OpenAICompatible,
        };

        match create_provider(provider_type, &config.provider) {
            Ok(provider) => Ok(Some(provider)),
            Err(e) => {
                warn!("Failed to create provider: {}", e);
                Ok(None)
            }
        }
    }
    /// Execute with native tool calling using AgenticLoopV4 (unified API).
    ///
    /// This is the recommended method for agent execution with native tool calling support.
    /// The `on_event` callback receives all streaming events (text deltas, tool calls, etc.).

    /// Execute with native tool calling and return a channel receiver for events.
    ///
    /// This is a convenience wrapper around execute_native() that provides
    /// a channel-based interface for code that expects async event streaming.

    /// Check if the configured provider supports native tool calling
    pub fn supports_native_tools(&self) -> bool {
        self.provider
            .as_ref()
            .map(|p| p.supports_native_tools())
            .unwrap_or(false)
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
                provider_type: ProviderType::Ollama,
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
}
