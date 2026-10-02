//! `ToolingRuntime` — the daemon's shared tooling runtime (ADR-066 D2).
//!
//! This is the named-pieces replacement for the retired
//! `ExtensionCore` singleton: one composition of
//!
//! - [`ToolCatalog`] — tool registration + the wire catalog;
//! - [`ToolDispatcher`] — the single execution point;
//! - `HookRegistry` — the surviving hook path (workspace hooks:
//!   PreToolUse / PostToolUse / Stop / AfterAgent / PromptSection /
//!   SessionContextBuild — P4 re-homes them onto the minimal
//!   dispatcher);
//! - [`SessionKeys`] — per-agent session-key bookkeeping;
//! - the prompt-section providers (`PromptSectionProvider`).
//!
//! Built once in `daemon::state` and threaded explicitly through
//! `PrincipalManager` → `PrincipalContext` → `Agent` → the engine's
//! `ToolFunnel` / `EngineHooks` seams. There is no process-global
//! accessor — tests construct their own.

use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use peko_extension_api::hook_io::{
    CompactionPreparationPayload, CompactionResultPayload, HookDecision,
};
use peko_extension_api::session::SessionSnapshot;
use peko_extension_api::{
    EngineHooks, PromptSectionRequest, PromptSections, ToolCallSpec, ToolFunnel,
};
use peko_subject::PrincipalId;

use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::core::hook_registry::HookRegistry;
use crate::extensions::framework::core::ExtensionServices;
use crate::extensions::framework::transport::async_router::AsyncExecutionRouter;
use crate::extensions::framework::types::HookInput;
use crate::tools::catalog::ToolCatalog;
use crate::tools::dispatcher::ToolDispatcher;
use crate::tools::prompt_sections::{PromptSectionInput, PromptSectionProvider};
use crate::tools::session_keys::SessionKeys;

/// The built-in prompt-section names, in render order. Workspace-hook
/// `PromptSection` binds with one of these names augment the built-in
/// section instead of producing a second dispatch (ADR-052 D6).
const BUILTIN_PROMPT_SECTIONS: [&str; 5] = [
    "identity",
    "roles",
    "skills",
    "workflows",
    "session_context",
];

/// The daemon's shared tooling runtime — see the module doc.
pub struct ToolingRuntime {
    catalog: Arc<ToolCatalog>,
    dispatcher: Arc<ToolDispatcher>,
    hooks: Arc<HookRegistry>,
    session_keys: SessionKeys,
    services: Arc<ExtensionServices>,
    prompt_providers: std::sync::RwLock<Vec<Arc<dyn PromptSectionProvider>>>,
    /// Set once the shared prompt-section providers are installed.
    /// Principal-owned tools and hooks have a separate per-principal
    /// guard below so a second principal gets its own workspace bag.
    tool_bag_installed: Arc<AtomicBool>,
    pub(crate) installed_principals: tokio::sync::Mutex<std::collections::HashSet<PrincipalId>>,
}

impl std::fmt::Debug for ToolingRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolingRuntime")
            .field("catalog", &self.catalog)
            .field("dispatcher", &"<ToolDispatcher>")
            .field("hooks", &"<HookRegistry>")
            .finish()
    }
}

impl ToolingRuntime {
    /// Compose the runtime. `router` is the timeout/detach machinery
    /// the dispatcher routes through; `audit` receives the `tool.call`
    /// audit events (`None` for tests / standalone contexts).
    #[must_use]
    pub fn new(
        catalog: Arc<ToolCatalog>,
        hooks: Arc<HookRegistry>,
        services: Arc<ExtensionServices>,
        router: Arc<AsyncExecutionRouter>,
        audit: Option<Arc<peko_observability::Observability>>,
    ) -> Self {
        let dispatcher = Arc::new(ToolDispatcher::new(
            Arc::clone(&catalog),
            Arc::clone(&hooks),
            router,
            audit,
        ));
        Self {
            catalog,
            dispatcher,
            hooks,
            session_keys: SessionKeys::new(),
            services,
            prompt_providers: std::sync::RwLock::new(Vec::new()),
            tool_bag_installed: Arc::new(AtomicBool::new(false)),
            installed_principals: tokio::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// A standalone runtime for tests / offline contexts: empty
    /// catalog, fresh hook registry, default services, a local router,
    /// no audit sink. Tools are registered explicitly by the caller
    /// (`ToolRuntime::register_builtins` or `catalog().register*`).
    #[must_use]
    pub fn standalone() -> Arc<Self> {
        let services = Arc::new(ExtensionServices::new());
        let hooks = Arc::new(HookRegistry::with_services(Arc::clone(&services)));
        Arc::new(Self::new(
            Arc::new(ToolCatalog::new()),
            hooks,
            services,
            Arc::new(AsyncExecutionRouter::new()),
            None,
        ))
    }

    /// The tool catalog.
    #[must_use]
    pub fn catalog(&self) -> &Arc<ToolCatalog> {
        &self.catalog
    }

    /// The tool dispatcher.
    #[must_use]
    pub fn dispatcher(&self) -> &Arc<ToolDispatcher> {
        &self.dispatcher
    }

    /// The surviving hook registry (workspace hooks — P4 re-homes).
    #[must_use]
    pub fn hooks(&self) -> &Arc<HookRegistry> {
        &self.hooks
    }

    /// The per-agent session-key table.
    #[must_use]
    pub fn session_keys(&self) -> &SessionKeys {
        &self.session_keys
    }

    /// The extension services (channel port / cross-runtime context)
    /// hook contexts still carry.
    #[must_use]
    pub fn services(&self) -> &Arc<ExtensionServices> {
        &self.services
    }

    /// Whether the shared prompt-section providers are installed.
    #[must_use]
    pub fn tool_bag_installed(&self) -> bool {
        self.tool_bag_installed.load(AtomicOrdering::Acquire)
    }

    /// Mark the shared prompt-section providers as installed.
    pub fn mark_tool_bag_installed(&self) {
        self.tool_bag_installed.store(true, AtomicOrdering::Release);
    }

    /// Wait for async tasks to complete (delegates to the
    /// dispatcher's router).
    pub async fn wait_for_async_tasks(&self, timeout: std::time::Duration) {
        self.dispatcher.router().wait_for_all_tasks(timeout).await;
    }

    /// Register a prompt-section provider. Registration order +
    /// `priority` (desc) set the aggregation order within a section.
    pub fn register_prompt_section(&self, provider: Arc<dyn PromptSectionProvider>) {
        self.prompt_providers
            .write()
            .expect("prompt providers lock poisoned")
            .push(provider);
    }

    /// Fire an observe-only hook point with the 2s soft-fail budget.
    async fn fire_observe(&self, point: HookPoint, mut input: HookInput) {
        if let HookInput::Json(ref mut payload) = input {
            if let Some(session_id) = payload["agent_did"]
                .as_str()
                .and_then(|agent_id| self.session_keys.get(agent_id))
            {
                if let Some(object) = payload.as_object_mut() {
                    object.insert("session_id".into(), session_id.into());
                }
            }
        }
        let _ = tokio::time::timeout(
            peko_tools_core::HOOK_TIMEOUT,
            self.hooks.invoke_hook(point, input),
        )
        .await;
    }

    /// Aggregate the per-turn prompt sections: built-in providers +
    /// workspace-hook `PromptSection` binds (custom sections and
    /// built-in-name augmentations both fire through the hook registry
    /// until P4).
    async fn render_prompt_sections(&self, request: &PromptSectionRequest) -> PromptSections {
        let input = PromptSectionInput {
            principal_id: request.principal_id.clone(),
            workspace: std::path::PathBuf::from(&request.workspace),
            session_id: request.session_id.clone(),
            channel_port: self.services.channel_port(),
        };
        let providers: Vec<Arc<dyn PromptSectionProvider>> = self
            .prompt_providers
            .read()
            .expect("prompt providers lock poisoned")
            .clone();

        let mut names: Vec<String> = BUILTIN_PROMPT_SECTIONS
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let mut custom_names: Vec<String> = self
            .hooks
            .get_all_hooks()
            .await
            .iter()
            .filter(|hook| hook.enabled)
            .filter(|hook| {
                prompt_section_visible_to(&hook.extension_id.0, Some(&request.principal_id))
            })
            .filter_map(|hook| match &hook.point {
                HookPoint::PromptSystemSection { section, .. }
                    if !BUILTIN_PROMPT_SECTIONS.contains(&section.as_str())
                        && section != "tools" =>
                {
                    Some(section.clone())
                }
                _ => None,
            })
            .collect();
        custom_names.sort();
        custom_names.dedup();
        names.extend(custom_names);
        // Independent sections preserve the renderer's parallel, soft-fail contract.
        let providers = &providers;
        let input = &input;
        let sections = futures::future::join_all(names.iter().map(|name| async move {
            let mut texts = Vec::new();
            let mut section_providers: Vec<_> = providers
                .iter()
                .filter(|provider| provider.section() == name)
                .collect();
            section_providers.sort_by_key(|provider| std::cmp::Reverse(provider.priority()));
            for provider in section_providers {
                if let Ok(Some(text)) =
                    tokio::time::timeout(peko_tools_core::HOOK_TIMEOUT, provider.render(input))
                        .await
                {
                    if !text.is_empty() {
                        texts.push(text);
                    }
                }
            }
            if let Some(text) = self.fire_section_hook(name, request).await {
                if !text.is_empty() {
                    texts.push(text);
                }
            }
            (name.clone(), texts.join("\n"))
        }))
        .await;

        PromptSections { sections }
    }

    /// Fire the hook registry's handlers for one prompt section with
    /// the 2s soft-fail budget; returns the combined text.
    async fn fire_section_hook(
        &self,
        name: &str,
        request: &PromptSectionRequest,
    ) -> Option<String> {
        let point = if name == "session_context" {
            HookPoint::SessionContextBuild
        } else {
            HookPoint::PromptSystemSection {
                section: name.to_string(),
                priority: 100,
            }
        };
        let input = if name == "session_context" {
            HookInput::SessionState(SessionSnapshot {
                session_id: request.session_id.clone(),
                message_count: 0,
                context_tokens: 0,
                metadata: std::collections::HashMap::new(),
            })
        } else {
            HookInput::Unit
        };
        let mut ctx = crate::extensions::framework::core::HookContext::new(
            point,
            input,
            Arc::clone(&self.services),
        );
        ctx.set_state(
            "tool_context",
            crate::extensions::framework::types::ToolRuntimeContext::new()
                .with_principal_id(request.principal_id.clone())
                .with_workspace(request.workspace.clone())
                .with_session_id(request.session_id.clone()),
        );
        let result = tokio::time::timeout(
            peko_tools_core::HOOK_TIMEOUT,
            self.hooks.invoke_hook_with_context(ctx),
        )
        .await;
        match result {
            Ok(
                crate::extensions::framework::types::HookResult::Continue(
                    crate::extensions::framework::types::HookOutput::Text(text),
                )
                | crate::extensions::framework::types::HookResult::Replace(
                    crate::extensions::framework::types::HookOutput::Text(text),
                ),
            ) => Some(text),
            Ok(crate::extensions::framework::types::HookResult::Continue(
                crate::extensions::framework::types::HookOutput::Vec(outputs),
            )) => {
                let texts: Vec<&str> = outputs.iter().filter_map(|o| o.as_text()).collect();
                if texts.is_empty() {
                    None
                } else {
                    Some(texts.join("\n"))
                }
            }
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl ToolFunnel for ToolingRuntime {
    async fn execute(
        &self,
        call: ToolCallSpec,
    ) -> anyhow::Result<(String, serde_json::Value, bool)> {
        self.dispatcher.execute(call).await
    }

    async fn list_tool_definitions(
        &self,
        principal_id: &PrincipalId,
    ) -> Vec<peko_provider_api::ToolDefinition> {
        self.catalog.tool_definitions(principal_id).await
    }

    async fn render_prompt_sections(&self, request: &PromptSectionRequest) -> PromptSections {
        ToolingRuntime::render_prompt_sections(self, request).await
    }
}

#[async_trait::async_trait]
impl EngineHooks for ToolingRuntime {
    async fn is_parallelizable(&self, tool_name: &str, principal_id: &PrincipalId) -> bool {
        self.catalog
            .is_parallelizable(tool_name, principal_id)
            .await
    }

    async fn fire_stop_hook(&self, payload: serde_json::Value) {
        self.fire_observe(HookPoint::Stop, HookInput::Json(payload))
            .await;
    }

    async fn fire_after_agent_hook(&self, payload: serde_json::Value) {
        self.fire_observe(HookPoint::AfterAgent, HookInput::Json(payload))
            .await;
    }

    async fn session_compaction_pre_hook(
        &self,
        payload: CompactionPreparationPayload,
    ) -> HookDecision {
        let input = payload.into_hook_input();
        let result = self
            .hooks
            .invoke_hook(HookPoint::SessionCompaction, input)
            .await;
        HookDecision::from_hook_result(result)
    }

    async fn session_compaction_post_hook(&self, payload: CompactionResultPayload) -> HookDecision {
        let input = payload.into_hook_input();
        let result = self
            .hooks
            .invoke_hook(HookPoint::SessionCompactionPost, input)
            .await;
        HookDecision::from_hook_result(result)
    }

    async fn session_state_change_hook(&self, snapshot: SessionSnapshot) -> HookDecision {
        let input = HookInput::SessionState(snapshot);
        let result = self
            .hooks
            .invoke_hook(HookPoint::SessionStateChange, input)
            .await;
        HookDecision::from_hook_result(result)
    }

    async fn set_session_key(&self, agent_id: &str, key: Option<String>) {
        self.session_keys.set(agent_id, key);
    }

    async fn has_deferred_tools(&self, principal_id: &PrincipalId) -> bool {
        self.catalog.has_deferred_tools_for(principal_id).await
    }
}

/// ADR-052 D6 scope rule for custom prompt sections: a hook registered
/// under a principal-scoped extension id (`principal:<pid>/...`) is
/// visible only to that principal; every other extension id is system
/// scope, visible to all. A `None` principal (system scope) sees only
/// non-principal-scoped hooks.
fn prompt_section_visible_to(extension_id: &str, principal_id: Option<&str>) -> bool {
    match extension_id.strip_prefix("principal:") {
        Some(rest) => {
            let owner = rest.split('/').next().unwrap_or(rest);
            Some(owner) == principal_id
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::{HookContext, HookHandler};
    use crate::extensions::framework::types::{ExtensionId, HookResult, ToolRuntimeContext};

    #[derive(Debug)]
    struct ScopedRecorder {
        point: HookPoint,
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl HookHandler for ScopedRecorder {
        fn hook_point(&self) -> HookPoint {
            self.point.clone()
        }
        async fn handle(&self, ctx: HookContext) -> HookResult {
            let identity = ctx.get_state::<ToolRuntimeContext>("tool_context").unwrap();
            self.seen
                .lock()
                .unwrap()
                .push(identity.principal_id.clone().unwrap());
            HookResult::Handled // Observers cannot veto execution or lifecycle completion.
        }
    }

    #[tokio::test]
    async fn workspace_observers_receive_owner_context_for_tool_and_lifecycle_calls() {
        let runtime = ToolingRuntime::standalone();
        let p1 = PrincipalId::generate().to_string();
        let p2 = PrincipalId::generate().to_string();
        let first = Arc::new(std::sync::Mutex::new(Vec::new()));
        let second = Arc::new(std::sync::Mutex::new(Vec::new()));
        for (pid, seen) in [(&p1, &first), (&p2, &second)] {
            for point in [
                HookPoint::Stop,
                HookPoint::AfterAgent,
                HookPoint::PreToolUse {
                    tool_name: "*".into(),
                },
                HookPoint::PostToolUse {
                    tool_name: "*".into(),
                },
            ] {
                runtime
                    .hooks()
                    .register_hook(
                        point.clone(),
                        Arc::new(ScopedRecorder {
                            point,
                            seen: seen.clone(),
                        }),
                        &ExtensionId::new(format!("principal:{pid}/hook:recorder")),
                    )
                    .await
                    .unwrap();
            }
        }
        let mut call = ToolCallSpec::new("unavailable", serde_json::json!({}));
        call.principal_id = Some(p1.clone());
        assert!(!runtime.execute(call).await.unwrap().2);
        runtime
            .fire_stop_hook(serde_json::json!({"principal_id": p1, "reason":"end"}))
            .await;
        runtime
            .fire_after_agent_hook(serde_json::json!({"principal_id": p1, "reason":"end"}))
            .await;
        assert_eq!(*first.lock().unwrap(), vec![p1; 4]);
        assert!(second.lock().unwrap().is_empty());
    }
}
