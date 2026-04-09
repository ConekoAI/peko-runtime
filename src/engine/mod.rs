//! Core Engine - Agentic execution engine for Pekobot
//!
//! Provides the core agentic loop, runner, and state management.
//! This is the heart of Pekobot's agent execution.

pub mod async_agentic_loop;
pub mod async_tool_executor;
pub mod chunker;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod input;
pub mod loop_v4;
pub mod runner;
// Note: SimpleSession merged into UnifiedSession in src/session/unified.rs
pub mod state;
pub mod stream_buffer;
pub mod stream_orchestrator;
pub mod task_manager;
pub mod tool_executor;
pub mod tool_stream;

pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use event_processor::{ChannelAction, EventProcessor, ProcessorConfig};
pub use events::{AgenticEvent, EventRouter, LifecyclePhase};
pub use execution::{ExecutionMode, TaskExecutor, TaskId, TaskStatus, TaskSummary};
pub use input::{A2AMessageType, AgentInput, HookType, InputContext};
pub use loop_v4::{AgenticLoopV4, AgenticResult as AgenticResultV4, ToolCall};
pub use runner::AgentRunner;
// SimpleSession now unified - use crate::session::UnifiedSession
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use async_agentic_loop::{AsyncAgenticConfig, AsyncAgenticLoop, AsyncToolMetrics};
pub use async_tool_executor::{AsyncCapability, AsyncToolExecutor, ToolProgress};
pub use task_manager::{TaskManager, TaskManagerConfig, TaskManagerStats};
pub use tool_executor::{ToolExecutor, ToolExecutionContext};
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
    /// Task manager for tool execution
    task_manager: Arc<TaskManager>,
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
    /// Default tool timeout (seconds)
    pub tool_timeout_secs: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            enable_tools: true,
            enable_memory: true,
            provider_timeout_secs: 30,
            tool_timeout_secs: 120,
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
        let task_manager = Arc::new(TaskManager::with_config(TaskManagerConfig {
            default_timeout: std::time::Duration::from_secs(config.tool_timeout_secs),
        }));

        Self {
            agent,
            provider,
            tools,
            config,
            state,
            task_manager,
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

    /// Get the task manager
    #[must_use]
    pub fn task_manager(&self) -> &Arc<TaskManager> {
        &self.task_manager
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


#[cfg(test)]
mod tool_executor_test;
