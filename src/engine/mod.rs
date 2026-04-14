//! Core Engine - Agentic execution engine for Pekobot
//!
//! Provides the core agentic loop, runner, and state management.
//! This is the heart of Pekobot's agent execution.

pub mod async_tool_executor;
pub mod chunker;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod input;
pub mod loop_v4;
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
// SimpleSession now unified - use crate::session::UnifiedSession
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use async_tool_executor::{AsyncCapability, AsyncToolExecutor, ToolProgress};
pub use task_manager::{TaskManager, TaskManagerConfig, TaskManagerStats};
pub use tool_executor::{ToolExecutor, ToolExecutionContext};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};

#[cfg(test)]
mod tool_executor_test;
