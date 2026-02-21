//! Agent Runner - High-level interface for running agents

use crate::agent::Agent;
use crate::engine::loop_::{AgenticLoop, AgenticResult};
use crate::engine::EngineConfig;
use crate::providers::Provider;
use crate::tools::Tool;
use anyhow::Result;
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tracing::{debug, error, info, warn};

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
}

impl AgentRunner {
    /// Create a new agent runner
    pub fn new(
        agent: Arc<Agent>,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        config: impl Into<RunConfig>,
    ) -> Self {
        Self {
            agent,
            provider,
            tools,
            config: config.into(),
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

        // Create the agentic loop
        let loop_ = AgenticLoop::new(self.agent.clone(), self.provider.clone(), tools)
            .with_max_iterations(self.config.max_iterations);

        // Run with timeout
        let timeout_duration = Duration::from_secs(
            self.config.provider_timeout_secs * self.config.max_iterations as u64 + 10,
        );

        let result = match timeout(timeout_duration, loop_.run(prompt)).await {
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

        let tool_calls: Vec<String> = result.tool_calls.iter().map(|tc| tc.name.clone()).collect();

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

    /// Run with streaming (placeholder for future implementation)
    pub async fn run_streaming(&self, _prompt: &str) -> Result<RunResult> {
        // TODO: Implement streaming support
        warn!("Streaming not yet implemented, falling back to regular run");
        self.run(_prompt).await
    }
}

/// Builder for AgentRunner
pub struct AgentRunnerBuilder {
    agent: Option<Arc<Agent>>,
    provider: Option<Arc<dyn Provider>>,
    tools: Vec<Arc<dyn Tool>>,
    config: RunConfig,
}

impl AgentRunnerBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            agent: None,
            provider: None,
            tools: vec![],
            config: RunConfig::default(),
        }
    }

    /// Set the agent
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
    pub fn tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    /// Set max iterations
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.config.max_iterations = max;
        self
    }

    /// Set timeout
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.config.provider_timeout_secs = secs;
        self
    }

    /// Disable tools
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
