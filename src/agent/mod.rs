//! Agent management module
//!
//! Note: The agentic loop has been moved to `engine::loop_`.
//! This module re-exports for backward compatibility.

// Re-export from engine for backward compatibility
pub use crate::engine::loop_::{AgenticLoop, AgenticResult, ToolCall};

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

    /// Execute a task with tools using the agentic loop
    pub async fn execute_with_tools(
        &self,
        prompt: &str,
    ) -> Result<crate::engine::loop_::AgenticResult> {
        use crate::engine::loop_::AgenticLoop;
        use crate::tools::*;
        use std::sync::Arc;

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        let result = if let Some(provider) = &self.provider {
            // Create core tools
            let tools: Vec<Arc<dyn Tool>> = vec![
                Arc::new(WebSearchTool::new(WebSearchConfig::default())),
                Arc::new(FetchTool::new(FetchConfig::default())),
                Arc::new(FileSystemTool::new()),
                Arc::new(ProcessTool::new()),
            ];

            // Use self (existing agent) and clone the Arc to the provider
            // This keeps the same identity across all messages!
            let agent_arc = Arc::new(self.clone_for_loop());
            let provider_arc = Arc::clone(provider);
            
            let loop_ = AgenticLoop::new(agent_arc, provider_arc, tools);

            match loop_.run(prompt).await {
                Ok(result) => Ok(result),
                Err(e) => {
                    error!("Agentic loop error: {}", e);
                    Err(e)
                }
            }
        } else {
            Err(anyhow::anyhow!("No provider configured"))
        };

        self.set_state(AgentState::Idle);
        result
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
            memory: None, // Don't clone memory to avoid lock issues
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
    pub async fn execute_streaming(
        &self,
        prompt: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>> {
        use crate::engine::loop_::AgenticLoop;
        use crate::engine::AgenticEvent;
        use crate::tools::*;
        use std::sync::Arc;

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(100);

        if let Some(provider) = &self.provider {
            // Create core tools
            let tools: Vec<Arc<dyn Tool>> = vec![
                Arc::new(WebSearchTool::new(WebSearchConfig::default())),
                Arc::new(FetchTool::new(FetchConfig::default())),
                Arc::new(FileSystemTool::new()),
                Arc::new(ProcessTool::new()),
            ];

            // Clone for the loop
            let agent_arc = Arc::new(self.clone_for_loop());
            let provider_arc = Arc::clone(provider);
            let prompt = prompt.to_string();

            // Spawn the streaming execution using spawn_local
            // This is required because Agent contains non-Send types
            let _handle = tokio::task::spawn_local(async move {
                info!("Spawned streaming task started");
                let loop_ = AgenticLoop::new(agent_arc, provider_arc, tools);

                // Run with streaming
                match loop_.run_streaming(&prompt, event_tx.clone()).await {
                    Ok(_) => {
                        info!("Streaming task completed successfully");
                        info!("Sending End event");
                        let _ = event_tx.send(AgenticEvent::Lifecycle {
                            run_id: prompt[..prompt.len().min(16)].to_string(),
                            phase: crate::engine::LifecyclePhase::End,
                            error: None,
                        }).await;
                    }
                    Err(e) => {
                        error!("Streaming execution error: {}", e);
                        let _ = event_tx.send(AgenticEvent::Lifecycle {
                            run_id: prompt[..prompt.len().min(16)].to_string(),
                            phase: crate::engine::LifecyclePhase::Error,
                            error: Some(format!("Error: {}", e)),
                        }).await;
                    }
                }
                info!("Spawned streaming task ending");
            });
            info!("Streaming task spawned");
        } else {
            return Err(anyhow::anyhow!("No provider configured"));
        }

        Ok(event_rx)
    }

    /// Execute with streaming support using AgenticLoopV2 (new message abstraction)
    ///
    /// Returns a channel receiver for AgenticEvents. The caller can
    /// display these events as they arrive for a responsive UI.
    ///
    /// Note: This method must be called within a tokio::task::LocalSet
    /// because Agent contains non-Send types (rusqlite connections).
    pub async fn execute_streaming_v2(
        &self,
        prompt: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>> {
        use crate::engine::loop_v2::AgenticLoopV2;
        use crate::engine::AgenticEvent;
        use crate::tools::*;
        use std::sync::Arc;

        // For interactive mode, allow consecutive messages without state check
        // Just ensure we're not in a nested call by checking if already busy
        let was_idle = self.state() == AgentState::Idle;
        
        if was_idle {
            self.set_state(AgentState::Busy);
        }

        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(100);

        if let Some(provider) = &self.provider {
            // Create core tools
            let tools: Vec<Arc<dyn Tool>> = vec![
                Arc::new(WebSearchTool::new(WebSearchConfig::default())),
                Arc::new(FetchTool::new(FetchConfig::default())),
                Arc::new(FileSystemTool::new()),
                Arc::new(ProcessTool::new()),
            ];

            // Clone for the loop
            let agent_arc = Arc::new(self.clone_for_loop());
            let provider_arc = Arc::clone(provider);
            let prompt = prompt.to_string();

            // Spawn the streaming execution using spawn_local
            let _handle = tokio::task::spawn_local(async move {
                info!("Spawned v2 streaming task started");
                let loop_ = AgenticLoopV2::new(agent_arc, provider_arc, tools);

                // Run with streaming
                match loop_.run(&prompt, event_tx.clone()).await {
                    Ok(_) => {
                        info!("V2 streaming task completed successfully");
                        let _ = event_tx.send(AgenticEvent::Lifecycle {
                            run_id: prompt[..prompt.len().min(16)].to_string(),
                            phase: crate::engine::LifecyclePhase::End,
                            error: None,
                        }).await;
                    }
                    Err(e) => {
                        error!("V2 streaming execution error: {}", e);
                        let _ = event_tx.send(AgenticEvent::Lifecycle {
                            run_id: prompt[..prompt.len().min(16)].to_string(),
                            phase: crate::engine::LifecyclePhase::Error,
                            error: Some(format!("Error: {}", e)),
                        }).await;
                    }
                }
                info!("Spawned v2 streaming task ending");
            });
            info!("V2 streaming task spawned");
        } else {
            // Reset state if we set it
            if was_idle {
                self.set_state(AgentState::Idle);
            }
            return Err(anyhow::anyhow!("No provider configured"));
        }

        Ok(event_rx)
    }

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
        use crate::types::provider::ProviderType;

        let api_key = match config.provider.get_api_key() {
            Ok(key) => key,
            Err(e) => {
                warn!("Failed to get API key: {}", e);
                return Ok(None);
            }
        };

        let model_config = config
            .provider
            .default_model_config()
            .cloned()
            .unwrap_or_default();

        let provider: Arc<dyn Provider> = match config.provider.provider_type {
            ProviderType::OpenAI => {
                let openai_config = crate::providers::OpenAIConfig {
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
                Arc::new(crate::providers::OpenAIProvider::new(openai_config)?)
            }
            ProviderType::Anthropic => {
                let anthropic_config = crate::providers::AnthropicConfig {
                    api_key,
                    base_url: config
                        .provider
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
                    model: model_config.name,
                    max_tokens: model_config.max_tokens,
                    temperature: model_config.temperature,
                    timeout_seconds: config.provider.timeout_seconds,
                };
                Arc::new(crate::providers::AnthropicProvider::new(anthropic_config)?)
            }
            ProviderType::Ollama => {
                let ollama_config = crate::providers::OllamaConfig {
                    base_url: config
                        .provider
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "http://localhost:11434".to_string()),
                    model: model_config.name,
                    temperature: model_config.temperature,
                    timeout_seconds: config.provider.timeout_seconds,
                    num_ctx: Some(4096),
                    num_gpu: None,
                };
                Arc::new(crate::providers::OllamaProvider::new(ollama_config)?)
            }
            ProviderType::OpenAICompatible => {
                let base_url = config
                    .provider
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                
                // Check if this is a Kimi endpoint
                if base_url.contains("moonshot.cn") {
                    Arc::new(crate::providers::KimiProvider::new(api_key))
                } else {
                    // Generic OpenAI-compatible provider
                    let compat_config = crate::providers::OpenAICompatibleConfig {
                        api_key,
                        base_url: base_url.clone(),
                        model: model_config.name,
                        max_tokens: model_config.max_tokens,
                        temperature: model_config.temperature,
                        timeout_seconds: config.provider.timeout_seconds,
                    };
                    Arc::new(crate::providers::OpenAICompatibleProvider::new("openai_compatible", compat_config)?)
                }
            }
            ProviderType::Kimi => {
                Arc::new(crate::providers::KimiProvider::new(api_key))
            }
            ProviderType::KimiCode => {
                let kimi_code_config = crate::providers::KimiCodeConfig {
                    api_key,
                    base_url: config
                        .provider
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://api.kimi.com/coding".to_string()),
                    model: model_config.name,
                    max_tokens: model_config.max_tokens,
                    temperature: model_config.temperature,
                    timeout_seconds: config.provider.timeout_seconds,
                };
                Arc::new(crate::providers::KimiCodeProvider::new(kimi_code_config)?)
            }
        };

        Ok(Some(provider))
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
