//! `Agent` tool — root-side compatibility shim.
//!
//! Phase 10e moved the canonical `AgentTool` and `SubagentRuntime`
//! port into `peko_tools_builtin::messaging`. This file is now a thin
//! shim that:
//!
//! 1. Re-exports the built-in's `AgentTool` and
//!    `SharedSubagentRuntime` so existing
//!    `crate::tools::builtin::messaging::AgentTool` import paths
//!    keep working.
//! 2. Re-exports the lifted DTOs (`AgentConfig`, `ExecutionConfig`,
//!    `SpawnError`, `SubagentRunView`, `SpawnCleanupPolicy`,
//!    `SubagentResult`).
//! 3. Provides the executor-typed constructor shim [`new_agent_tool`]
//!    that wraps an `Arc<SubagentExecutor>` in a
//!    [`SubagentExecutorRuntime`] adapter before handing it to the
//!    built-in `AgentTool`. Sprint 7 collapsed the previous
//!    four-variant constructor (`new`, `with_workspace`,
//!    `with_session_provider`, `with_workspace_and_session`) into
//!    one — workspace lives on the runtime port now, and the caller's
//!    session id is read from `ToolContext::session_id` on the
//!    production path (the runtime port's `session_id()` accessor is
//!    the fallback).
//!
//! B5 cleanup: [`runtime_from_executor`] was inlined — it had no
//! external consumers; the only call was the one inside
//! [`new_agent_tool`]. The `pub use` re-export of it from
//! `messaging/mod.rs` is gone too.
//!
//! B4 cleanup: [`DynamicSessionKeyProvider`] was deleted — only
//! `set_session_key` was called (once per subagent, in
//! `subagent_executor.rs`); the `get_session_key` reader had no
//! callers. `ToolContext::session_id` is the canonical production
//! session-key source.

use std::sync::Arc;

pub use crate::tools::builtin::messaging::{
    AgentArgs, AgentTool, ExecutionConfig, SharedSubagentRuntime, SpawnAuditEvent,
    SpawnCleanupPolicy, SpawnRequest, SubagentResult, SubagentRunView,
};

use crate::agents::subagent_executor::SubagentExecutor;
use crate::agents::subagent_runtime_impl::SubagentExecutorRuntime;

pub use crate::agents::agent_config::AgentConfig;
pub use crate::agents::subagent_error::SpawnError;

/// Create an `AgentTool` with an executor-backed runtime.
///
/// Sprint 7: the previous constructor also accepted a `workspace`
/// and a session-key provider; both have moved onto the runtime
/// port (`SubagentRuntime::workspace` + `SubagentRuntime::session_id`)
/// or are now read from `ToolContext::session_id` on the production
/// `execute_with_context` path. The executor's
/// `with_principal_workspace` builder still binds a workspace on
/// the executor; the tool reads it via the runtime port.
#[must_use]
pub fn new_agent_tool(executor: Arc<SubagentExecutor>) -> AgentTool {
    AgentTool::new(Arc::new(SubagentExecutorRuntime::new(executor)))
}