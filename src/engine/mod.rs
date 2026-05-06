//! Core Engine - Agentic execution engine for Pekobot
//!
//! Provides the core agentic loop, runner, and state management.
//! This is the heart of Pekobot's agent execution.

pub mod chunker;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod input;
pub mod agentic_loop;
// Note: SimpleSession merged into Session in src/session/unified.rs
pub mod state;
pub mod stream_buffer;
pub mod stream_orchestrator;
pub mod stream_types;
pub mod tool_stream;

pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use stream_types::{ChannelOutput, EventStream, StreamingConfig, default_process_stream};
pub use event_processor::{ChannelAction, EventProcessor, ProcessorConfig};
pub use events::{AgenticEvent, LifecyclePhase};
pub use execution::{ExecutionMode, TaskId, TaskStatus, TaskSummary};
pub use input::{A2AMessageType, AgentInput, HookType, InputContext};
pub use agentic_loop::{AgenticLoop, AgenticResult, ToolCall};
// SimpleSession now unified - use crate::session::Session
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};
