//! F37 canonical tool-execution funnel.
//!
//! [`execute_tool_via_core`] and [`execute_tool_via_core_with_context`]
//! are the single chokepoint through which every tool invocation in the
//! agentic loop routes. They wrap [`ToolFunnel::execute_tool_via_hook`]
//! with cancel-bridging so a soft-interrupt `CancellationToken` becomes
//! a real `watch::Receiver<bool>` (`AbortSignal`) before reaching the
//! hook chain.
//!
//! Phase 9b.N.2: lifted from `src/engine/tool_runtime.rs`. The
//! surrounding `ToolRuntime` struct + `register_builtins` stay in root
//! because the concrete `BashTool` registration still references
//! `src/tools/builtin/bash.rs` (Phase 10 didn't move BashTool);
//! lifting the whole file would require lifting BashTool into
//! `peko-tools-builtin` first. The two pure helper functions are
//! lifted on their own because they have no BashTool coupling.
//!
//! The receiver is `&dyn ToolFunnel` — a narrow trait port added in
//! Phase 9b.N.2 to break the `peko-engine ↔ root::ExtensionCore`
//! coupling without lifting `ExtensionCore` to `peko-extension-host`
//! (Phase 8 bulk move). Root's `ExtensionCore` impls `ToolFunnel` via
//! `src/engine/extension_core_funnel_compat.rs`. When Phase 8
//! eventually moves `ExtensionCore` into `peko-extension-host`, the
//! trait can be removed.

use anyhow::Result;
use peko_extension_host::ToolFunnel;
use peko_tools_core::{bridge_from_cancellation_token, AbortSignalBridgeGuard};

/// Canonical tool execution via the [`ToolFunnel`] host surface.
///
/// All production code should call this (or `ToolRuntime::execute_tool`)
/// to ensure consistent behavior: workspace injection, reserved params,
/// permission checks, abort/timeout handling, progress reporting, and
/// metrics.
///
/// Returns a triplet of `(display_string, json_value, success)`.
pub async fn execute_tool_via_core(
    core: &dyn ToolFunnel,
    tool_name: &str,
    params: serde_json::Value,
    workspace: Option<String>,
) -> Result<(String, serde_json::Value, bool)> {
    execute_tool_via_core_with_context(
        core, tool_name, params, workspace, None, None, None, None, None, None, None, None,
    )
    .await
}

/// Execute a tool via the [`ToolFunnel`] host surface with agent,
/// session, caller, principal, and per-call allowlist context.
///
/// `agent_id` / `session_id` drive reserved parameter injection.
/// `caller_id` drives per-user permission checks and audit logging
/// (issue #17).
/// `principal_id` (P2-audit) is threaded into `ToolContext` so
/// extension-scoped tools (e.g. `Skill`) can resolve per-principal
/// state via `ExtensionStateRegistry` at handle time.
/// `principal_name` is the human-readable Principal name used by
/// Principal-scoped tools (e.g. `CronCreate`) to target jobs.
/// `capabilities` is the principal/agent capability set used by the
/// execution gate instead of the mutable global `tool_config`.
/// `active_extensions` is the set of extension IDs that are active
/// for the current Principal; when present, the gate also verifies
/// the tool's owner is active.
/// `cancel` is the soft-interrupt `CancellationToken` (PR #128). When
/// `Some`, this function bridges the token into a
/// `watch::Receiver<bool>` (`AbortSignal`) via
/// `peko_tools_core::bridge_from_cancellation_token` so
/// `BuiltinToolAdapter` can plumb a real receiver into
/// `ToolContext::for_hook_run_with_abort`, making the trait-default
/// `ctx.is_aborted()` check in `peko_tools_core::traits` meaningful
/// in production. The bridge task is aborted on drop; callers should
/// not need to await or otherwise manage the returned guard.
///
/// F37: now delegates to [`ToolFunnel::execute_tool_via_hook`] —
/// the canonical funnel method. The cancel-bridging stays here (only
/// `src/engine/tool_executor.rs:183` passes a cancel today) so the
/// `'static` factory closures in `AsyncSpawnTool` / `cron_engine` can
/// call `execute_tool_via_hook` directly without carrying the
/// bridge's lifetime.
pub async fn execute_tool_via_core_with_context(
    core: &dyn ToolFunnel,
    tool_name: &str,
    params: serde_json::Value,
    workspace: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    caller_id: Option<String>,
    principal_id: Option<String>,
    principal_name: Option<String>,
    capabilities: Option<Vec<String>>,
    active_extensions: Option<Vec<String>>,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<(String, serde_json::Value, bool)> {
    let (abort_signal, _abort_guard) = match cancel {
        Some(token) => {
            let (signal, guard) = bridge_from_cancellation_token(token);
            (Some(signal.subscribe()), guard)
        }
        None => (None, AbortSignalBridgeGuard::noop()),
    };

    // F37: build the ToolCall input here (with the bridged abort_signal)
    // and route through the canonical funnel. The trait method has the
    // same 11-arg + abort-signal shape as ExtensionCore's inherent
    // method.
    let (text, json, success) = core
        .execute_tool_via_hook(
            tool_name,
            params,
            workspace,
            agent_id,
            session_id,
            caller_id,
            principal_id,
            principal_name,
            capabilities,
            active_extensions,
            abort_signal,
        )
        .await?;
    Ok((text, json, success))
}
