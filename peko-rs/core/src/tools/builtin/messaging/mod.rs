//! Messaging tools — root-side compatibility shims.
//!
//! Tools for inter-agent communication and messaging:
//! - `Agent`: Spawn sub-agents
//!
//! Phase 10e moved the canonical `AgentTool` and `SubagentRuntime`
//! port into `peko_tools_builtin::messaging`. This module re-exports
//! the tool, the lifted DTOs, and the executor-typed constructor
//! shims from `crate::tools::builtin::messaging::agent` (the root
//! shim file). The runtime-side `SubagentExecutorRuntime` adapter
//! lives in `crate::agents::subagent_runtime_impl`.
//!
//! `principal_send` (principal-level cross-runtime) lives in
//! `crate::tunnel::principal_send_tool`. Tools layer no longer
//! depends on tunnel.

pub mod agent;

pub use agent::{
    agent_tool_with_session_provider, agent_tool_with_workspace,
    agent_tool_with_workspace_and_session, new_agent_tool, runtime_from_executor, AgentArgs,
    AgentTool, CompletedRun, DynamicSessionKeyProvider, ExecutionConfig, SessionKeyProvider,
    SharedSubagentRuntime, SpawnAuditEvent, SpawnCleanupPolicy, SpawnRequest,
    StaticSessionKeyProvider, SubagentResult, SubagentRunView,
};
// `AgentConfig` and `SpawnError` re-exports live in
// `crate::tools::builtin::messaging::agent` (alongside the shim
// constructors); see that module's `pub use` lines.
