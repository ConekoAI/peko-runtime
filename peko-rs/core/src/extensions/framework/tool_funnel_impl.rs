//! `ToolFunnel` impl for `crate::extensions::framework::core::ExtensionCore`.
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

use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::core::ExtensionCore;
use crate::extensions::framework::types::HookInput;
use peko_extension_api::hook_io::{
    CompactionPreparationPayload, CompactionResultPayload, HookDecision,
};
use peko_extension_api::session::SessionSnapshot;
use peko_extension_api::ToolFunnel;
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

    async fn registered_prompt_sections(
        &self,
        principal_id: Option<&str>,
        _active_extensions: Option<Vec<String>>,
    ) -> Vec<String> {
        // ADR-052 D6: scan the hook registry for `PromptSystemSection`
        // points so workspace hooks (`principal:<pid>/hook:<id>`) can
        // contribute named tail sections. Scoping mirrors the
        // principal-visibility rule used for workspace-hook
        // registration: a principal-scoped extension id is visible only
        // to its own principal; every other id is system scope and
        // visible to all. (`active_extensions` is accepted for parity
        // with the invoke path's context but not consulted — the hook
        // invoke path doesn't filter by it either.)
        let hooks = self.get_all_hooks().await;
        let mut sections: Vec<String> = hooks
            .iter()
            .filter(|hook| hook.enabled)
            .filter_map(|hook| match &hook.point {
                HookPoint::PromptSystemSection { section, .. } => {
                    Some((section.clone(), hook.extension_id.0.as_str()))
                }
                _ => None,
            })
            .filter(|(_section, ext_id)| prompt_section_visible_to(ext_id, principal_id))
            .map(|(section, _ext_id)| section)
            .collect();
        sections.sort();
        sections.dedup();
        sections
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

/// ADR-052 D6 scope rule for [`ToolFunnel::registered_prompt_sections`]:
/// a hook registered under a principal-scoped extension id
/// (`principal:<pid>/...`, e.g. workspace hooks, or the tool registry's
/// `principal:<pid>:<name>` form) is visible only to that principal;
/// every other extension id is system scope, visible to all. A `None`
/// principal (system scope) sees only non-principal-scoped hooks.
fn prompt_section_visible_to(extension_id: &str, principal_id: Option<&str>) -> bool {
    match extension_id.strip_prefix("principal:") {
        Some(rest) => {
            let owner = rest.split(['/', ':']).next().unwrap_or(rest);
            Some(owner) == principal_id
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::prompt_section_visible_to;

    #[test]
    fn prompt_section_visibility_scoping() {
        // System-scope ids are visible to everyone, including `None`.
        assert!(prompt_section_visible_to(
            "builtin:tool:Bash",
            Some("alice")
        ));
        assert!(prompt_section_visible_to("builtin:tool:Bash", None));
        // Workspace-hook ids are visible only to their principal.
        assert!(prompt_section_visible_to(
            "principal:alice/hook:weather",
            Some("alice")
        ));
        assert!(!prompt_section_visible_to(
            "principal:alice/hook:weather",
            Some("bob")
        ));
        assert!(!prompt_section_visible_to(
            "principal:alice/hook:weather",
            None
        ));
        // The tool-registry `principal:<pid>:<name>` form scopes the same.
        assert!(prompt_section_visible_to(
            "principal:alice:customskill",
            Some("alice")
        ));
        assert!(!prompt_section_visible_to(
            "principal:alice:customskill",
            Some("bob")
        ));
    }
}
