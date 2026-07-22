//! `ToolFunnel` — the engine-facing surface of `ExtensionCore::execute_tool_via_hook`.
//!
//! Phase 9b.N.2: F37's `execute_tool_via_core_with_context` funnel
//! lives in `peko_engine::funnel`. It depends on the canonical
//! `ExtensionCore::execute_tool_via_hook` method, but `ExtensionCore`
//! itself is still in root (`src/extensions/framework/core/registry.rs`)
//! — its Phase 8 bulk move into `peko-extension-host` is deferred
//! (per [[workspace-phase8-commit2-scope-down]]).
//!
//! To unblock the funnel lift without a cycle or a big-bang move, the
//! trait abstracts the one method `peko-engine` needs. It follows the
//! existing `peko-extension-host` pattern of narrow, real-consumer
//! view traits (`PathResolver`, `SessionInboxSink`, `InboxSinkProvider`,
//! `DaemonTransport`, `VaultAccess`, `PrincipalMessageService`).
//!
//! **Transient scaffolding.** When the Phase 8 bulk move eventually
//! lifts `ExtensionCore` into `peko-extension-host`, this trait can be
//! removed (its single method becomes a direct impl on the real
//! type). Until then it lets the four Phase 9b.N residuals that read
//! `ExtensionCore` (9b.N.2 funnel, 9b.N.3 tool_executor, 9b.N.4
//! compaction_orchestrator, 9b.N.5+ agentic_loop) consume the engine's
//! host contract through one trait port.

use anyhow::Result;

/// The single engine-facing surface of `ExtensionCore::execute_tool_via_hook`.
///
/// Implemented by root's `crate::extensions::framework::core::ExtensionCore`
/// via `src/engine/extension_core_funnel_compat.rs`. The trait is
/// object-safe because async-trait is used in production via
/// `#[async_trait]` — see F37's `ExtensionCore::execute_tool_via_hook`
/// signature for the canonical 11-arg + abort-signal shape.
///
/// # Why one method
///
/// The 4 Phase 9b.N residuals each need access to `ExtensionCore`'s
/// state, but the only one cleanly extractable as a single trait method
/// is the `execute_tool_via_hook` funnel that F37 made canonical. The
/// other engine→host surfaces (registry lookups, hook invocation) can
/// be added to this trait when their respective lifts need them —
/// keeping the trait surface narrow until a second consumer appears.
#[async_trait::async_trait]
pub trait ToolFunnel: Send + Sync + 'static {
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
