//! Agent Runner - High-level interface for running agents

use crate::agent::Agent;
use crate::engine::loop_v4::AgenticLoopV4;
use crate::engine::{AgenticEvent, EngineConfig};
use crate::extensions::core::ExtensionCore;
use crate::providers::Provider;
use crate::tools::Tool;
use anyhow::Result;
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tracing::{error, info, warn};

/// Configuration for running an agent
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Maximum iterations
    pub max_iterations: usize,
    /// Provider timeout in seconds
    pub provider_timeout_secs: u64,
    /// Enable tool execution
    pub enable_tools: bool,
    /// System prompt override
    pub system_prompt: Option<String>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            provider_timeout_secs: 30,
            enable_tools: true,
            system_prompt: None,
        }
    }
}

impl From<EngineConfig> for RunConfig {
    fn from(config: EngineConfig) -> Self {
        Self {
            max_iterations: config.max_iterations,
            provider_timeout_secs: config.provider_timeout_secs,
            enable_tools: config.enable_tools,
            system_prompt: None,
        }
    }
}

/// Result from running an agent
#[derive(Debug, Clone)]
pub struct RunResult {
    /// Whether execution succeeded
    pub success: bool,
    /// Final response
    pub response: String,
    /// Number of iterations
    pub iterations: usize,
    /// Tool calls made
    pub tool_calls: Vec<String>,
    /// Execution time in milliseconds
    pub execution_time_ms: u64,
    /// Any error message
    pub error: Option<String>,
}

/// Runner for executing agents
pub struct AgentRunner {
    /// The agent
    agent: Arc<Agent>,
    /// LLM provider
    provider: Arc<dyn Provider>,
    /// Available tools
    tools: Vec<Arc<dyn Tool>>,
    /// Run configuration
    config: RunConfig,
    /// Extension core for skill loading and hook integration
    extension_core: Arc<ExtensionCore>,
}

impl AgentRunner {
    /// Create a new agent runner
    pub fn new(
        agent: Arc<Agent>,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        config: impl Into<RunConfig>,
    ) -> Self {
        // Create or get the global extension core
        let extension_core = crate::extensions::core::global_core()
            .unwrap_or_else(|| {
                let core = Arc::new(ExtensionCore::new());
                crate::extensions::core::init_global_core(core.clone());
                core
            });
        
        Self {
            agent,
            provider,
            tools,
            config: config.into(),
            extension_core,
        }
    }
    
    /// Create a new agent runner with an existing ExtensionCore
    pub fn with_extension_core(
        agent: Arc<Agent>,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        config: impl Into<RunConfig>,
        extension_core: Arc<ExtensionCore>,
    ) -> Self {
        Self {
            agent,
            provider,
            tools,
            config: config.into(),
            extension_core,
        }
    }

    /// Run the agent with a prompt
    pub async fn run(&self, prompt: &str) -> Result<RunResult> {
        let start_time = std::time::Instant::now();
        info!(
            "AgentRunner starting for agent: {} (prompt: {} chars)",
            self.agent.name(),
            prompt.len()
        );

        // Filter tools if disabled
        let tools = if self.config.enable_tools {
            self.tools.clone()
        } else {
            vec![]
        };

        // Create the agentic loop with v4
        let loop_ = AgenticLoopV4::new(
            self.agent.clone(), 
            self.provider.clone(), 
            tools,
            Arc::clone(&self.extension_core),
        )
        .with_max_iterations(self.config.max_iterations);

        // Run with timeout
        let timeout_duration = Duration::from_secs(
            self.config.provider_timeout_secs * self.config.max_iterations as u64 + 10,
        );

        // Run with a no-op callback (events are logged but not streamed)
        let result = match timeout(
            timeout_duration,
            loop_.run(prompt, |_event| {
                // Events are ignored in non-streaming mode
                // They get logged inside loop_v4 anyway
            }),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                error!("Agentic loop error: {}", e);
                return Ok(RunResult {
                    success: false,
                    response: String::new(),
                    iterations: 0,
                    tool_calls: vec![],
                    execution_time_ms: start_time.elapsed().as_millis() as u64,
                    error: Some(e.to_string()),
                });
            }
            Err(_) => {
                warn!("Agent execution timed out");
                return Ok(RunResult {
                    success: false,
                    response: String::new(),
                    iterations: 0,
                    tool_calls: vec![],
                    execution_time_ms: start_time.elapsed().as_millis() as u64,
                    error: Some("Execution timed out".to_string()),
                });
            }
        };

        let execution_time_ms = start_time.elapsed().as_millis() as u64;

        let tool_calls: Vec<String> = result
            .tool_calls
            .iter()
            .filter_map(|tc| match tc {
                crate::types::message::ContentBlock::ToolCall { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();

        info!(
            "AgentRunner completed: success={}, iterations={}, tools={}, time={}ms",
            result.success,
            result.iterations,
            tool_calls.len(),
            execution_time_ms
        );

        Ok(RunResult {
            success: result.success,
            response: result.final_answer,
            iterations: result.iterations,
            tool_calls,
            execution_time_ms,
            error: None,
        })
    }

    /// Run with streaming support
    ///
    /// Returns a channel receiver that emits `AgenticEvents` during execution.
    /// The channel has a large buffer (10,000) to prevent event loss.
    ///
    /// # Note
    /// This method must be called within a `tokio::task::LocalSet` because
    /// Agent contains non-Send types (rusqlite connections).
    pub async fn run_streaming(
        &self,
        prompt: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<AgenticEvent>> {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(10000);

        // Filter tools if disabled
        let tools = if self.config.enable_tools {
            self.tools.clone()
        } else {
            vec![]
        };

        // Clone for the spawned task
        let agent = self.agent.clone();
        let provider = self.provider.clone();
        let max_iterations = self.config.max_iterations;
        let timeout_secs = self.config.provider_timeout_secs;
        let prompt = prompt.to_string();
        let event_tx_clone = event_tx.clone();

        // Spawn the execution in a local task (Agent is !Send due to rusqlite)
        tokio::task::spawn_local(async move {
            let start_time = std::time::Instant::now();
            info!(
                "AgentRunner (streaming) starting for agent: {} (prompt: {} chars)",
                agent.name(),
                prompt.len()
            );

            // Create or get extension core
            let extension_core = crate::extensions::core::global_core()
                .unwrap_or_else(|| {
                    let core = Arc::new(ExtensionCore::new());
                    crate::extensions::core::init_global_core(core.clone());
                    core
                });
            
            let loop_ = AgenticLoopV4::new(
                agent.clone(), 
                provider, 
                tools,
                extension_core,
            )
            .with_max_iterations(max_iterations);

            let timeout_duration = Duration::from_secs(timeout_secs * max_iterations as u64 + 10);

            let result = match timeout(
                timeout_duration,
                loop_.run(&prompt, {
                    let event_tx = event_tx_clone.clone();
                    move |event| {
                        let _ = event_tx.try_send(event);
                    }
                }),
            )
            .await
            {
                Ok(Ok(result)) => {
                    info!(
                        "AgentRunner (streaming) completed: success={}, iterations={}, time={}ms",
                        result.success,
                        result.iterations,
                        start_time.elapsed().as_millis()
                    );
                    result
                }
                Ok(Err(e)) => {
                    error!("Agentic loop error: {}", e);
                    let _ = event_tx_clone.try_send(AgenticEvent::Lifecycle {
                        run_id: "error".to_string(),
                        phase: crate::engine::LifecyclePhase::Error,
                        error: Some(e.to_string()),
                    });
                    return;
                }
                Err(_) => {
                    warn!("Agent execution timed out");
                    let _ = event_tx_clone.try_send(AgenticEvent::Lifecycle {
                        run_id: "timeout".to_string(),
                        phase: crate::engine::LifecyclePhase::Aborted,
                        error: Some("Execution timed out".to_string()),
                    });
                    return;
                }
            };

            // Send completion event
            let _ = event_tx_clone.try_send(AgenticEvent::Lifecycle {
                run_id: "complete".to_string(),
                phase: crate::engine::LifecyclePhase::End,
                error: if result.success {
                    None
                } else {
                    Some("Execution failed".to_string())
                },
            });
        });

        Ok(event_rx)
    }
}

/// Builder for `AgentRunner`
pub struct AgentRunnerBuilder {
    agent: Option<Arc<Agent>>,
    provider: Option<Arc<dyn Provider>>,
    tools: Vec<Arc<dyn Tool>>,
    config: RunConfig,
}

impl AgentRunnerBuilder {
    /// Create a new builder
    #[must_use]
    pub fn new() -> Self {
        Self {
            agent: None,
            provider: None,
            tools: vec![],
            config: RunConfig::default(),
        }
    }

    /// Set the agent
    #[must_use]
    pub fn agent(mut self, agent: Arc<Agent>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Set the provider
    pub fn provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Add a tool
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add multiple tools
    #[must_use]
    pub fn tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    /// Set max iterations
    #[must_use]
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.config.max_iterations = max;
        self
    }

    /// Set timeout
    #[must_use]
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.config.provider_timeout_secs = secs;
        self
    }

    /// Disable tools
    #[must_use]
    pub fn disable_tools(mut self) -> Self {
        self.config.enable_tools = false;
        self
    }

    /// Build the runner
    pub fn build(self) -> Result<AgentRunner> {
        let agent = self
            .agent
            .ok_or_else(|| anyhow::anyhow!("Agent is required"))?;
        let provider = self
            .provider
            .ok_or_else(|| anyhow::anyhow!("Provider is required"))?;

        Ok(AgentRunner::new(agent, provider, self.tools, self.config))
    }
}

impl Default for AgentRunnerBuilder {
    fn default() -> Self {
        Self::new()
    }
}
