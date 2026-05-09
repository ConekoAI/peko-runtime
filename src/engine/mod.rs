//! Core Engine - Agentic execution engine for Pekobot
//!
//! Provides the core agentic loop, runner, and state management.
//! This is the heart of Pekobot's agent execution.

pub mod agentic_loop;
pub mod chunker;
pub mod compaction_orchestrator;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod input;
pub mod tool_executor;
// Note: SimpleSession merged into Session in src/session/unified.rs
pub mod state;
pub mod stream_buffer;
pub mod stream_orchestrator;
pub mod stream_types;
pub mod tool_stream;

pub use agentic_loop::{AgenticLoop, AgenticResult, ToolCall};
pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use event_processor::{ChannelAction, EventProcessor, ProcessorConfig};
pub use events::{AgenticEvent, LifecyclePhase};
pub use execution::{ExecutionMode, TaskId, TaskStatus, TaskSummary};
pub use input::{A2AMessageType, AgentInput, HookType, InputContext};
pub use stream_types::{default_process_stream, ChannelOutput, EventStream, StreamingConfig};
// SimpleSession now unified - use crate::session::Session
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};
