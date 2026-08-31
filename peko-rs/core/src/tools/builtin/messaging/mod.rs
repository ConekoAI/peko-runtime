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
//! [`SpawnError`], [`ExecutionConfig`],
//! [`SubagentResult`], and [`SubagentRunView`] are
//! the canonical DTOs the port traffics in. Root re-exports each via
//! `pub use crate::tools::builtin::messaging::...` for backwards
//! compatibility. [`SpawnCleanupPolicy`] is re-exported from
//! `peko_extension_host` (Phase 8 commit 2) so consumers of this
//! module only need one import path.
//!
//! Sprint 8 Commit 4 removed the `AgentConfig` mirror DTO — the
//! spawn path now uses `Arc<crate::agents::subagent_runtime_impl::AgentPrompt>`
//! directly (workspace Markdown as the single source of truth).
//! Root's canonical `crate::agents::agent_config::AgentConfig` is
//! still re-exported from `agent_compat` for the gateway loop
//! (`StatelessAgentService`) until Sprint 8b migrates that
//! consumer.
//!
//! ## Port
//!
//! [`SubagentRuntime`] is the four-method surface `AgentTool` needs:
//! capability check, disk resolution, audit, and execute-and-wait.
//! Production wiring uses `SubagentExecutorRuntime` (root's
//! `src/agents/subagent_runtime_impl.rs`); tests substitute a
//! `TestSubagentRuntime` fixture.

pub mod agent;
pub mod agent_compat;
pub mod dto;
pub mod subagent_runtime;

pub use agent::{AgentArgs, AgentTool};
pub use dto::{
    ExecutionConfig, SpawnCleanupPolicy, SpawnError, SubagentResult,
    SubagentRunView,
};
pub use subagent_runtime::{SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest, SubagentRuntime};

// Phase F4: `agent_compat` exposes the executor-typed constructor
// shim `new_agent_tool` that wraps an `Arc<SubagentExecutor>` in a
// `SubagentExecutorRuntime` adapter. Sprint 7 collapsed the four
// `agent_tool_with_*` variants into a single `new_agent_tool` —
// workspace moved onto the runtime port (`SubagentRuntime::workspace`)
// and `ToolContext::session_id` is the canonical session-key source.
// B4 cleanup: `DynamicSessionKeyProvider` was removed — it was
// write-only on the production path (only `set_session_key` was
// called once per subagent, no reads).
// B5 cleanup: `runtime_from_executor` was inlined into
// `new_agent_tool`; the only caller was the one inside the same
// module.
pub use agent_compat::new_agent_tool;
