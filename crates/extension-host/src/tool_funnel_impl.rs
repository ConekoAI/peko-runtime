//! `ToolFunnel` impl for `peko_extension_host::ExtensionCore`.
//!
//! Phase 8a moved `ExtensionCore` from root `src/extensions/framework/`
//! into `peko_extension_host`. The `impl ToolFunnel for ExtensionCore`
//! that lived in `src/engine/extension_core_funnel_compat.rs` is now
//! a foreign-trait-impl-on-foreign-type (orphan rule violation), so
//! the impl relocates next to the type in this crate.
//!
//! The behavior is unchanged from the root-side compat file: every
//! method delegates to the canonical implementation on `ExtensionCore`
//! (same calls, same args, same timeout, same observe-only hook
//! semantics). The only change is the import path
//! (`crate::extensions::framework::X` → `crate::X`).

use crate::core::hook_points::HookPoint;
use crate::core::ExtensionCore;
use crate::tool_funnel::ToolFunnel;
use crate::types::HookInput;
use peko_extension_api::hook_io::{
    CompactionPreparationPayload, CompactionResultPayload, HookDecision,
};
use peko_extension_api::session::SessionSnapshot;
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
        let point = HookPoint::Stop;
        let input = HookInput::Json(merged);
        let _ = tokio::time::timeout(HOOK_TIMEOUT, self.invoke_hook(point, input)).await;
    }

    async fn invoke_after_agent_hook(&self, merged: serde_json::Value) {
        let point = HookPoint::AfterAgent;
        let input = HookInput::Json(merged);
        let _ = tokio::time::timeout(HOOK_TIMEOUT, self.invoke_hook(point, input)).await;
    }

    async fn set_session_key(&self, agent_id: &str, key: Option<String>) {
        ExtensionCore::set_session_key(self, agent_id, key).await;
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
        ExtensionCore::has_deferred_tools_for(self, principal_id).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn invoke_prompt_section_hook(
        &self,
        section: &str,
        priority: i32,
        principal_id: Option<&str>,
        capabilities: Option<Vec<String>>,
        active_extensions: Option<Vec<String>>,
        workspace: Option<String>,
    ) -> Option<String> {
        // Phase 9b.N.5b.4: lifted PromptRenderer::dispatch_text's
        // hook firing into the trait. Delegates to
        // ExtensionCore::invoke_hook_text_with_principal (the
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
        // Phase 9b.N.5b.4: lifted PromptRenderer::dispatch_session_context's
        // hook firing into the trait.
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
