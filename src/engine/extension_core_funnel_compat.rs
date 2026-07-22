//! Compatibility shim: implements `peko_extension_host::ToolFunnel`
//! for the legacy root-owned `ExtensionCore` so the engine-facing
//! surface in `peko-engine` (funnel + tool_executor + compaction
//! orchestrator) can call through the trait port without holding a
//! direct borrow of root `ExtensionCore`.
//!
//! Phase 9b.N.2: trait-port pattern, matching the `AsyncCompletionLike`
//! bridge introduced in Phase 9b.N.1 (PR #265). The trait is transient
//! scaffolding — once Phase 8 bulk-moves `ExtensionCore` into
//! `peko-extension-host`, this shim disappears and the trait itself
//! can be removed (or replaced by an inherent method).
//!
//! Phase 9b.N.3 widened the trait to add `is_parallelizable` (F33 gate
//! probe) + `pre_tool_use` / `post_tool_use` (F31x observe-only hook
//! firing) so `tool_executor.rs` could lift into `peko-engine`. The
//! trait hides `HookPoint` / `HookInput` construction inside this impl
//! — both types still live in root (`src/extensions/framework/core/`),
//! and lifting them into `peko-extension-api` is out of scope for
//! Phase 9b.N.3. The impl preserves the original timing + hook payload
//! semantics exactly (2s `HOOK_TIMEOUT` soft-fail, observe-only
//! `HookResult` discard) so lifted code is behaviour-equivalent.
//!
//! Phase 9b.N.4 added `invoke_session_compaction_pre_hook`,
//! `invoke_session_compaction_post_hook`, and
//! `invoke_session_state_change_hook` so the lifted
//! `CompactionOrchestrator` can fire the three compaction /
//! session-state hooks without touching `HookPoint` / `HookInput`
//! directly. Returns a trimmed [`peko_extension_api::HookDecision`]
//! (3 variants) instead of the full `HookResult` (5 variants +
//! `HookOutput`) so the trait stays free of root-only dependencies.
//!
//! Module location: rooted at `src/engine/extension_core_funnel_compat.rs`
//! so `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/async_completion_compat.rs` pattern.

use crate::extensions::framework::core::ExtensionCore;
use crate::extensions::framework::types::HookInput;
use crate::extensions::framework::HookPoint;
use peko_extension_api::hook_io::{
    CompactionPreparationPayload, CompactionResultPayload, HookDecision,
};
use peko_extension_api::session::SessionSnapshot;
use peko_extension_host::ToolFunnel;
use peko_tools_core::HOOK_TIMEOUT;

#[async_trait::async_trait]
impl ToolFunnel for ExtensionCore {
    async fn is_parallelizable(&self, tool_name: &str) -> bool {
        ExtensionCore::is_parallelizable(self, tool_name).await
    }

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
    ) {
        let input = HookInput::ToolCall {
            tool_name: tool_name.to_string(),
            params,
            workspace,
            agent_id,
            session_id,
            caller_id,
            principal_id,
            principal_name,
            capabilities,
            active_extensions,
            abort_signal: None,
        };
        let point = HookPoint::PreToolUse {
            tool_name: tool_name.to_string(),
        };
        let _ = tokio::time::timeout(HOOK_TIMEOUT, self.invoke_hook(point, input)).await;
    }

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
    ) {
        let input = HookInput::ToolCall {
            tool_name: tool_name.to_string(),
            params,
            workspace,
            agent_id,
            session_id,
            caller_id,
            principal_id,
            principal_name,
            capabilities,
            active_extensions,
            abort_signal: None,
        };
        let point = HookPoint::PostToolUse {
            tool_name: tool_name.to_string(),
        };
        let _ = tokio::time::timeout(HOOK_TIMEOUT, self.invoke_hook(point, input)).await;
    }

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

    async fn invoke_session_compaction_pre_hook(
        &self,
        payload: CompactionPreparationPayload,
    ) -> HookDecision {
        let input = payload.into_hook_input();
        let point = HookPoint::SessionCompaction;
        let result = self.invoke_hook(point, input).await;
        HookDecision::from_hook_result(result)
    }

    async fn invoke_session_compaction_post_hook(
        &self,
        payload: CompactionResultPayload,
    ) -> HookDecision {
        let input = payload.into_hook_input();
        let point = HookPoint::SessionCompactionPost;
        let result = self.invoke_hook(point, input).await;
        HookDecision::from_hook_result(result)
    }

    async fn invoke_session_state_change_hook(&self, snapshot: SessionSnapshot) -> HookDecision {
        let input = HookInput::SessionState(snapshot);
        let point = HookPoint::SessionStateChange;
        let result = self.invoke_hook(point, input).await;
        HookDecision::from_hook_result(result)
    }

    async fn invoke_stop_hook(&self, merged: serde_json::Value) {
        // F31x observe-only `Stop` hook firing.
        //
        // Phase 9b.N.5a moved the `HookInput::Json(merged.clone())` +
        // `HookPoint::Stop` construction from
        // `agentic_loop.rs:669-670` into this impl so the lifted
        // `agentic_loop.rs` (Phase 9b.N.5b) never imports
        // `HookPoint` / `HookInput` directly. The `merged` payload
        // shape is whatever the loop produced — typically the final
        // summary + metadata as a JSON blob — and is passed through
        // verbatim. The hook chain's return value is discarded
        // (observe-only in v1).
        let input = HookInput::Json(merged);
        let point = HookPoint::Stop;
        let _ = tokio::time::timeout(HOOK_TIMEOUT, self.invoke_hook(point, input)).await;
    }

    async fn invoke_after_agent_hook(&self, merged: serde_json::Value) {
        // F31x observe-only `AfterAgent` hook firing.
        //
        // Phase 9b.N.5a moved the `HookInput::Json(merged)` +
        // `HookPoint::AfterAgent` construction from
        // `agentic_loop.rs:683-684` into this impl (symmetric with
        // `invoke_stop_hook`). Observe-only.
        let input = HookInput::Json(merged);
        let point = HookPoint::AfterAgent;
        let _ = tokio::time::timeout(HOOK_TIMEOUT, self.invoke_hook(point, input)).await;
    }

    async fn set_session_key(&self, agent_id: &str, key: Option<String>) {
        ExtensionCore::set_session_key(self, agent_id, key).await
    }

    async fn list_tool_definitions_with_allowlist(
        &self,
        capabilities: &peko_extension_api::Capabilities,
        active_extensions: Option<&peko_extension_api::ActiveExtensionSet>,
        principal_id: &peko_subject::PrincipalId,
    ) -> Vec<peko_provider_api::ToolDefinition> {
        ExtensionCore::list_tool_definitions_with_allowlist(
            self,
            capabilities,
            active_extensions,
            principal_id,
        )
        .await
    }

    async fn has_deferred_tools_for(&self, principal_id: &peko_subject::PrincipalId) -> bool {
        // F34 `ToolExposure::Deferred` probe — used by
        // `AgenticLoop::build_tool_definitions` to gate the synthetic
        // F35 `__tool_search` stub. We walk the unfiltered list and
        // check exposure rather than fetching full metadata to keep
        // the trait free of root-only `ToolMetadata` deps.
        use crate::extensions::framework::types::ToolExposure;
        self.list_tools(principal_id)
            .await
            .iter()
            .any(|m| matches!(m.exposure, ToolExposure::Deferred))
    }

    async fn invoke_prompt_section_hook(
        &self,
        section: &str,
        priority: i32,
        principal_id: Option<&str>,
        capabilities: Option<Vec<String>>,
        active_extensions: Option<Vec<String>>,
        workspace: Option<String>,
    ) -> Option<String> {
        // Phase 9b.N.5b.4: lifted `PromptRenderer::dispatch_text`'s
        // hook firing into the trait. Builds `HookPoint::PromptSystemSection`
        // + `HookInput::Unit` internally and delegates to
        // `ExtensionCore::invoke_hook_text_with_principal` (the
        // canonical 7-arg principal-context-aware method).
        self.invoke_hook_text_with_principal(
            HookPoint::PromptSystemSection {
                section: section.to_string(),
                priority,
            },
            HookInput::Unit,
            principal_id,
            capabilities,
            active_extensions,
            workspace,
        )
        .await
    }

    async fn invoke_session_context_build_hook(
        &self,
        snapshot: SessionSnapshot,
        principal_id: Option<&str>,
        capabilities: Option<Vec<String>>,
        active_extensions: Option<Vec<String>>,
        workspace: Option<String>,
    ) -> Option<String> {
        // Phase 9b.N.5b.4: lifted `PromptRenderer::dispatch_session_context`'s
        // hook firing into the trait. Builds `HookPoint::SessionContextBuild`
        // + `HookInput::SessionState(snapshot)` internally and
        // delegates to `ExtensionCore::invoke_hook_text_with_principal`.
        self.invoke_hook_text_with_principal(
            HookPoint::SessionContextBuild,
            HookInput::SessionState(snapshot),
            principal_id,
            capabilities,
            active_extensions,
            workspace,
        )
        .await
    }
}
