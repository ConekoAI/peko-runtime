//! Compatibility shim: implements `peko_extension_host::ToolFunnel`
//! for the legacy root-owned `ExtensionCore` so the F37 funnel
//! functions in `peko-engine::funnel` can call through the trait port
//! without holding a direct borrow of root `ExtensionCore`.
//!
//! Phase 9b.N.2: trait-port pattern, matching the `AsyncCompletionLike`
//! bridge introduced in Phase 9b.N.1 (PR #265). The trait is transient
//! scaffolding — once Phase 8 bulk-moves `ExtensionCore` into
//! `peko-extension-host`, this shim disappears and the trait itself
//! can be removed (or replaced by an inherent method).
//!
//! Module location: rooted at `src/engine/extension_core_funnel_compat.rs`
//! so `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/async_completion_compat.rs` pattern.

use crate::extensions::framework::core::ExtensionCore;
use peko_extension_host::ToolFunnel;

#[async_trait::async_trait]
impl ToolFunnel for ExtensionCore {
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
    ) -> anyhow::Result<(String, serde_json::Value, bool)> {
        // Delegate to the existing canonical F37 method on
        // ExtensionCore. The impl preserves the 11-arg + abort-signal
        // shape exactly; the trait simply abstracts the type.
        ExtensionCore::execute_tool_via_hook(
            self,
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
        .await
    }
}
