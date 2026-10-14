//! F37 canonical tool-execution funnel.
//!
//! [`execute_tool_via_core`] and [`execute_tool_via_core_with_context`]
//! are the single chokepoint through which every tool invocation in the
//! agentic loop routes. They wrap [`ToolFunnel::execute`] with
//! cancel-bridging so a soft-interrupt `CancellationToken` becomes a
//! real `watch::Receiver<bool>` (`AbortSignal`) before reaching the
//! dispatcher.
//!
//! Phase 9b.N.2: lifted from `src/engine/tool_runtime.rs`. The
//! surrounding `ToolRuntime` struct + `register_builtins` stay in root
//! because the concrete `BashTool` registration still references
//! `src/tools/builtin/bash.rs`. The two pure helper functions are
//! lifted on their own because they have no BashTool coupling.
//!
//! The receiver is `&dyn ToolFunnel` — the engine-facing seam (ADR-066
//! D2) implemented by root's `ToolingRuntime`.

use crate::tooling::{ToolCallSpec, ToolFunnel};
use anyhow::Result;
use peko_tools_core::{bridge_from_cancellation_token, AbortSignalBridgeGuard};

/// Canonical tool execution via the [`ToolFunnel`] host surface.
///
/// All production code should call this (or `ToolRuntime::execute_tool`)
/// to ensure consistent behavior: workspace injection, reserved params,
/// abort/timeout handling, progress reporting, and metrics.
///
/// Returns a triplet of `(display_string, json_value, success)`.
pub async fn execute_tool_via_core(
    core: &dyn ToolFunnel,
    tool_name: &str,
    params: serde_json::Value,
    workspace: Option<String>,
) -> Result<(String, serde_json::Value, bool)> {
    execute_tool_via_core_with_context(
        core, tool_name, params, workspace, None, None, None, None, None, None,
    )
    .await
}

/// Execute a tool via the [`ToolFunnel`] host surface with agent,
/// session, caller, and principal context.
///
/// `agent_id` / `session_id` drive reserved parameter injection.
/// `acting_for` is the run's stamp (ADR-078); the dispatcher enforces
/// the trust floor against it and audits it.
/// `principal_id` (P2-audit) is threaded into `ToolContext` so
/// extension-scoped tools (e.g. `Skill`) can resolve per-principal
/// state via `ExtensionStateRegistry` at handle time.
/// `principal_name` is the human-readable Principal name used by
/// Principal-scoped tools (e.g. `Cron action create`) to target jobs.
/// `cancel` is the soft-interrupt `CancellationToken` (PR #128). When
/// `Some`, this function bridges the token into a
/// `watch::Receiver<bool>` (`AbortSignal`) via
/// `peko_tools_core::bridge_from_cancellation_token` so the tool body
/// observes `ToolContext::is_aborted()` in production. The bridge task
/// is aborted on drop; callers should not need to await or otherwise
/// manage the returned guard.
///
/// ADR-066 P3: delegates to [`ToolFunnel::execute`] with a packed
/// [`ToolCallSpec`]. The cancel-bridging stays here (only
/// `src/engine/tool_executor.rs` passes a cancel today) so the
/// `'static` factory closures in `AsyncSpawnAction` / `cron_engine` can
/// call `execute` directly without carrying the bridge's lifetime.
#[allow(clippy::too_many_arguments)]
pub async fn execute_tool_via_core_with_context(
    core: &dyn ToolFunnel,
    tool_name: &str,
    params: serde_json::Value,
    workspace: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    acting_for: Option<peko_tools_core::ActingFor>,
    principal_id: Option<String>,
    principal_name: Option<String>,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<(String, serde_json::Value, bool)> {
    execute_tool_via_core_with_invocation(
        core,
        tool_name,
        params,
        workspace,
        agent_id,
        session_id,
        acting_for,
        principal_id,
        principal_name,
        cancel,
        None,
    )
    .await
}

/// Execute with the engine's real run and model tool-call identifiers.
#[allow(clippy::too_many_arguments)]
pub async fn execute_tool_via_core_with_invocation(
    core: &dyn ToolFunnel,
    tool_name: &str,
    params: serde_json::Value,
    workspace: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    acting_for: Option<peko_tools_core::ActingFor>,
    principal_id: Option<String>,
    principal_name: Option<String>,
    cancel: Option<tokio_util::sync::CancellationToken>,
    invocation: Option<(&str, &str)>,
) -> Result<(String, serde_json::Value, bool)> {
    let (abort_signal, _abort_guard) = match cancel {
        Some(token) => {
            let (signal, guard) = bridge_from_cancellation_token(token);
            (Some(signal.subscribe()), guard)
        }
        None => (None, AbortSignalBridgeGuard::noop()),
    };

    let spec = ToolCallSpec {
        run_id: invocation.map(|(run, _)| run.to_string()),
        tool_call_id: invocation.map(|(_, call)| call.to_string()),
        catalog_binding: None,
        tool_name: tool_name.to_string(),
        params,
        workspace,
        agent_id,
        session_id,
        acting_for,
        principal_id,
        principal_name,
        abort_signal,
        background_progress: None,
        parent: None,
    };
    core.execute(spec).await
}
