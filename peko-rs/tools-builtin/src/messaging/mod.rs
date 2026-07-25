//! `peko_tools_builtin::messaging` — `Agent` tool surface + `SubagentRuntime` port.
//!
//! Phase 10e extracts the `Agent` tool (Claude Code parity — spawns
//! subagents for isolated task execution). Per the Phase 10 plan rule
//! ("Built-ins must not import daemon state"), the tool here does NOT
//! call `crate::agents::subagent_executor::SubagentExecutor`
//! directly. It speaks to a runtime port trait ([`SubagentRuntime`])
//! that the daemon/agent side implements.
//!
//! ## DTOs
//!
//! [`AgentConfig`], [`SpawnError`], [`ExecutionConfig`],
//! [`CompletedRun`], [`SubagentResult`], and [`SubagentRunView`] are
//! the canonical DTOs the port traffics in. Root re-exports each via
//! `pub use peko_tools_builtin::messaging::...` for backwards
//! compatibility. [`SpawnCleanupPolicy`] is re-exported from
//! `peko_extension_host` (Phase 8 commit 2) so consumers of this
//! module only need one import path.
//!
//! ## Port
//!
//! [`SubagentRuntime`] is the four-method surface `AgentTool` needs:
//! capability check, disk resolution, audit, and execute-and-wait.
//! Production wiring uses `SubagentExecutorRuntime` (root's
//! `src/agents/subagent_runtime_impl.rs`); tests substitute a
//! `TestSubagentRuntime` fixture.

pub mod agent;
pub mod dto;
pub mod subagent_runtime;

pub use agent::{AgentArgs, AgentTool, SessionKeyProvider, StaticSessionKeyProvider};
pub use dto::{
    AgentConfig, CompletedRun, ExecutionConfig, SpawnCleanupPolicy, SpawnError, SubagentResult,
    SubagentRunView,
};
pub use subagent_runtime::{SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest, SubagentRuntime};
