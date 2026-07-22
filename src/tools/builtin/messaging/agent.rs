//! `Agent` tool — root-side compatibility shim.
//!
//! Phase 10e moved the canonical `AgentTool` and `SubagentRuntime`
//! port into `peko_tools_builtin::messaging`. This file is now a thin
//! shim that:
//!
//! 1. Re-exports the built-in's `AgentTool`, `SessionKeyProvider`,
//!    `StaticSessionKeyProvider`, and `SharedSubagentRuntime` so
//!    existing `crate::tools::builtin::messaging::AgentTool` import
//!    paths keep working.
//! 2. Re-exports the lifted DTOs (`AgentConfig`, `ExecutionConfig`,
//!    `SpawnError`, `SubagentRunView`, `SpawnCleanupPolicy`,
//!    `CompletedRun`, `SubagentResult`).
//! 3. Defines [`DynamicSessionKeyProvider`] (root-only: the daemon
//!    mutates the session key at runtime, which is a runtime concern
//!    the built-in crate intentionally does not own).
//! 4. Provides executor-typed constructor shims (`new`, `with_workspace`,
//!    `with_session_provider`, `with_workspace_and_session`) that wrap
//!    an `Arc<SubagentExecutor>` in a [`SubagentExecutorRuntime`]
//!    adapter before handing it to the built-in `AgentTool`. This
//!    preserves the existing call shape so existing call sites in
//!    `src/agents/agent.rs` and `src/principal/agent_runner.rs`
//!    compile unchanged.

use std::sync::Arc;

pub use peko_tools_builtin::messaging::{
    AgentArgs, AgentTool, CompletedRun, ExecutionConfig, SessionKeyProvider, SharedSubagentRuntime,
    SpawnAuditEvent, SpawnCleanupPolicy, SpawnRequest, StaticSessionKeyProvider, SubagentResult,
    SubagentRunView,
};

use crate::agents::subagent_executor::SubagentExecutor;
use crate::agents::subagent_runtime_impl::SubagentExecutorRuntime;

pub use crate::agents::agent_config::AgentConfig;
pub use crate::agents::subagent_error::SpawnError;

/// Build the runtime port from a `SubagentExecutor` handle.
#[must_use]
pub fn runtime_from_executor(executor: Arc<SubagentExecutor>) -> SharedSubagentRuntime {
    Arc::new(SubagentExecutorRuntime::new(executor))
}

// ─── Executor-typed constructor shims (preserves root API shape) ──

/// Create an `AgentTool` with an executor-backed runtime.
#[must_use]
pub fn new_agent_tool(executor: Arc<SubagentExecutor>) -> AgentTool {
    AgentTool::new(runtime_from_executor(executor))
}

/// Create an `AgentTool` with a workspace and an executor-backed runtime.
#[must_use]
pub fn agent_tool_with_workspace(
    executor: Arc<SubagentExecutor>,
    workspace: Option<std::path::PathBuf>,
) -> AgentTool {
    AgentTool::with_workspace(runtime_from_executor(executor), workspace)
}

/// Create an `AgentTool` with a session-key provider and an
/// executor-backed runtime.
#[must_use]
pub fn agent_tool_with_session_provider(
    executor: Arc<SubagentExecutor>,
    provider: Box<dyn SessionKeyProvider>,
) -> AgentTool {
    AgentTool::with_session_provider(runtime_from_executor(executor), provider)
}

/// Create an `AgentTool` with workspace, session-key provider, and an
/// executor-backed runtime.
#[must_use]
pub fn agent_tool_with_workspace_and_session(
    executor: Arc<SubagentExecutor>,
    workspace: Option<std::path::PathBuf>,
    provider: Box<dyn SessionKeyProvider>,
) -> AgentTool {
    AgentTool::with_workspace_and_session(runtime_from_executor(executor), workspace, provider)
}

// ─── DynamicSessionKeyProvider (root-only runtime concern) ────────

/// Dynamic session key provider that can be updated at runtime.
///
/// Root-owned because the daemon mutates session keys as it processes
/// messages from different sessions. The built-in crate intentionally
/// does not own this — it is one of the few runtime concerns that
/// still legitimately belongs in root after the Phase 10e extraction.
#[derive(Clone)]
pub struct DynamicSessionKeyProvider {
    session_key: Arc<std::sync::RwLock<String>>,
}

impl DynamicSessionKeyProvider {
    #[must_use]
    pub fn new(initial_key: impl Into<String>) -> Self {
        Self {
            session_key: Arc::new(std::sync::RwLock::new(initial_key.into())),
        }
    }

    /// Update the current session key
    pub fn set_session_key(&self, key: impl Into<String>) {
        if let Ok(mut guard) = self.session_key.write() {
            *guard = key.into();
        }
    }

    /// Get the current session key
    #[must_use]
    pub fn get_session_key(&self) -> String {
        self.session_key
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

impl SessionKeyProvider for DynamicSessionKeyProvider {
    fn current_session_key(&self) -> String {
        self.get_session_key()
    }
}

// (Note: `impl SessionKeyProvider for Arc<DynamicSessionKeyProvider>`
// is provided in `peko_tools_builtin::messaging` as a blanket impl
// over `Arc<T: SessionKeyProvider>` — the orphan rule forbids the
// impl in this crate since `Arc` is foreign.)
