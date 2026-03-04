//! Core Engine - Agentic execution engine for Pekobot
//!
//! Provides the core agentic loop, runner, and state management.
//! This is the heart of Pekobot's agent execution.

pub mod chunker;
pub mod events;
pub mod loop_;
pub mod loop_v3;
pub mod runner;
pub mod simple_session;
pub mod state;
pub mod tool_stream;

pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use events::{AgenticEvent, EventRouter, LifecyclePhase};
pub use loop_::{AgenticLoop, AgenticResult, ToolCall};
pub use loop_v3::AgenticLoopV3;
pub use runner::AgentRunner;
pub use simple_session::SimpleSession;
pub use state::{AgentState, StateMachine};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};

use crate::agent::Agent;
use crate::providers::Provider;
use crate::tools::Tool;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Core engine for running agents
pub struct Engine {
    /// The agent being run
    agent: Arc<Agent>,
    /// LLM provider
    provider: Arc<dyn Provider>,
    /// Available tools
    tools: Vec<Arc<dyn Tool>>,
    /// Engine configuration
    config: EngineConfig,
    /// State machine
    state: Arc<RwLock<StateMachine>>,
}

/// Engine configuration
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Maximum iterations per request
    pub max_iterations: usize,
    /// Enable tool execution
    pub enable_tools: bool,
    /// Enable memory
    pub enable_memory: bool,
    /// Timeout for provider calls (seconds)
    pub provider_timeout_secs: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            enable_tools: true,
            enable_memory: true,
            provider_timeout_secs: 30,
        }
    }
}

impl Engine {
    /// Create a new engine
    pub fn new(
        agent: Arc<Agent>,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        config: Option<EngineConfig>,
    ) -> Self {
        let config = config.unwrap_or_default();
        let state = Arc::new(RwLock::new(StateMachine::new()));

        Self {
            agent,
            provider,
            tools,
            config,
            state,
        }
    }

    /// Run the engine with a user prompt
    pub async fn run(&self, prompt: &str) -> Result<EngineResult> {
        let state = self.state.write().await;
        state.set_busy();
        drop(state);

        info!("Engine starting for agent: {}", self.agent.name());

        let runner = AgentRunner::new(
            self.agent.clone(),
            self.provider.clone(),
            self.tools.clone(),
            self.config.clone(),
        );

        let run_result = runner.run(prompt).await?;

        let state = self.state.write().await;
        state.set_idle();

        Ok(EngineResult {
            success: run_result.success,
            response: run_result.response,
            iterations: run_result.iterations,
            tool_names: run_result.tool_calls,
            execution_time_ms: run_result.execution_time_ms,
        })
    }

    /// Get current state
    pub async fn state(&self) -> AgentState {
        let state = self.state.read().await;
        state.current()
    }

    /// Stop the engine
    pub async fn stop(&self) -> Result<()> {
        let state = self.state.write().await;
        state.set_idle();
        warn!("Engine stopping for agent: {}", self.agent.name());
        Ok(())
    }
}

/// Result from engine execution
#[derive(Debug, Clone)]
pub struct EngineResult {
    /// Whether execution succeeded
    pub success: bool,
    /// Final response
    pub response: String,
    /// Number of iterations
    pub iterations: usize,
    /// Tool call names made
    pub tool_names: Vec<String>,
    /// Execution time (ms)
    pub execution_time_ms: u64,
}
