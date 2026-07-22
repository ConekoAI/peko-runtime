//! `ToolFunnel` — the engine-facing surface of root's `ExtensionCore`.
//!
//! Phase 9b.N.2: F37's `execute_tool_via_core_with_context` funnel
//! lives in `peko_engine::funnel`. It depends on the canonical
//! `ExtensionCore::execute_tool_via_hook` method, but `ExtensionCore`
//! itself is still in root (`src/extensions/framework/core/registry.rs`)
//! — its Phase 8 bulk move into `peko-extension-host` is deferred
//! (per [[workspace-phase8-commit2-scope-down]]).
//!
//! To unblock the funnel lift without a cycle or a big-bang move, the
//! trait abstracts the engine-facing surface of `ExtensionCore`. It
//! follows the existing `peko-extension-host` pattern of narrow,
//! real-consumer view traits (`PathResolver`, `SessionInboxSink`,
//! `InboxSinkProvider`, `DaemonTransport`, `VaultAccess`,
//! `PrincipalMessageService`).
//!
//! **Transient scaffolding.** When the Phase 8 bulk move eventually
//! lifts `ExtensionCore` into `peko-extension-host`, this trait can be
//! removed (its methods become direct impls on the real type). Until
//! then it lets the four Phase 9b.N residuals that read `ExtensionCore`
//! (9b.N.2 funnel, 9b.N.3 tool_executor, 9b.N.4
//! compaction_orchestrator, 9b.N.5+ agentic_loop) consume the engine's
//! host contract through one trait port.
//!
//! Phase 9b.N.3 widened the trait to cover the full surface the engine
//! needs from `ExtensionCore`: `is_parallelizable(name)` (F33 gate
//! probe), `pre_tool_use(...)` / `post_tool_use(...)` (F31x observe-only
//! hook firing), and `execute_tool_via_hook(...)` (F37 funnel). Hiding
//! `HookPoint` / `HookInput` construction inside the impl keeps the
//! trait free of root-only type dependencies — `HookPoint` (865 lines
//! in `src/extensions/framework/core/hook_points.rs`) hasn't been
//! lifted into `peko-extension-api` yet, so re-exporting it from the
//! trait would defeat the move. Each hook method takes the raw
//! fields the executor already has in scope.

use anyhow::Result;

/// The engine-facing surface of root's `ExtensionCore`.
///
/// Implemented by root's `crate::extensions::framework::core::ExtensionCore`
/// via `src/engine/extension_core_funnel_compat.rs`. The trait is
/// object-safe because `async-trait` is used in production via
/// `#[async_trait]` — see F37's `ExtensionCore::execute_tool_via_hook`
/// signature for the canonical 11-arg + abort-signal shape.
///
/// # Why these methods
///
/// The 4 Phase 9b.N residuals each need access to `ExtensionCore`'s
/// state. The trait exposes exactly the engine-facing surface
/// `peko-engine` consumes today:
/// - `is_parallelizable` — F33 gate probe at the start of `ToolExecutor::execute`.
/// - `pre_tool_use` / `post_tool_use` — F31x observe-only hook firing
///   around the dispatch (observe-only → return is ignored, but the
///   impl still respects the 2s `HOOK_TIMEOUT` soft-fail).
/// - `execute_tool_via_hook` — F37 canonical funnel for the actual
///   tool dispatch.
///
/// Each additional method has a single real consumer today (the
/// tool executor). The trait surface stays narrow until a second
/// consumer appears — matching the F6/F7 lesson in
/// [[prefer-concrete-over-speculative-abstraction]].
#[async_trait::async_trait]
pub trait ToolFunnel: Send + Sync + 'static {
    /// F33 gate probe: is the named tool parallelizable?
    ///
    /// Returns `true` if the tool isn't registered — the dispatch will
    /// fail anyway, and admitting without serializing is the right
    /// "no-op" fallback (matches the pre-F37 behavior at
    /// `tool_executor.rs:147`). See `ExtensionCore::is_parallelizable`
    /// (root) for the canonical implementation.
    async fn is_parallelizable(&self, tool_name: &str) -> bool;

    /// F31x observe-only `PreToolUse` hook firing.
    ///
    /// Builds the `HookInput::ToolCall` + `HookPoint::PreToolUse`
    /// payload internally and invokes the hook chain with the shared
    /// `HOOK_TIMEOUT` budget. Handlers see the same `ToolCall` payload
    /// the dispatcher will use, but the return value is intentionally
    /// discarded — observe-only in v1 (the loop always continues to
    /// `ToolExecute`). Soft-fails on timeout (mirrors
    /// `loop_per_hook_timeout_fails_open`).
    #[allow(clippy::too_many_arguments)]
    async fn pre_tool_use(
        &self,
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
    );

    /// F31x observe-only `PostToolUse` hook firing.
    ///
    /// Symmetric with `pre_tool_use` — handlers see the executed
    /// result's *context* but their return value is ignored. The
    /// hook fires regardless of dispatch outcome so handlers see
    /// both successes and failures.
    #[allow(clippy::too_many_arguments)]
    async fn post_tool_use(
        &self,
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
    );

    /// Canonical funnel method — routes through `invoke_hook` so every
    /// tool call flows through PreToolUse / ToolExecute / PostToolUse
    /// hook chain + capability gate + reserved-params injection + abort
    /// handling. See `ExtensionCore::execute_tool_via_hook` (root) for
    /// the canonical implementation; the trait method has the same
    /// 11-arg + abort-signal shape.
    ///
    /// F37/F38 notes: the cancel-bridging lives in `peko_engine::funnel`
    /// (not in the trait); only `src/engine/tool_executor.rs` currently
    /// passes a cancel today.
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_via_hook(
        &self,
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
        abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<(String, serde_json::Value, bool)>;
}
