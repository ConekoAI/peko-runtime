//! `ToolingRuntime` — the daemon's shared tooling runtime (ADR-066 D2).
//!
//! This is the named-pieces replacement for the retired
//! `ExtensionCore` singleton: one composition of
//!
//! - [`ToolCatalog`] — tool registration + the wire catalog;
//! - [`ToolDispatcher`] — the single execution point;
//! - `WorkspaceHookDispatcher` — the surviving hook path (workspace hooks:
//!   PreToolUse / PostToolUse / Stop / AfterAgent / PromptSection /
//!   SessionContextBuild, executed in registration order);
//! - [`SessionKeys`] — per-agent session-key bookkeeping;
//! - `AgentRunLimits` — principal-wide live-run admission (ADR-067);
//! - the prompt-section providers (`PromptSectionProvider`).
//!
//! Built once in `daemon::state` and threaded explicitly through
//! `PrincipalManager` → `PrincipalContext` → `Agent` → the engine's
//! `ToolFunnel` / `EngineHooks` seams. There is no process-global
//! accessor — tests construct their own.

use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use peko_engine::{EngineHooks, PromptSectionRequest, PromptSections, ToolCallSpec, ToolFunnel};
use peko_session::SessionSnapshot;
use peko_subject::PrincipalId;

use crate::extensions::framework::core::ExtensionServices;
use crate::extensions::framework::transport::async_router::AsyncExecutionRouter;
use crate::extensions::workspace_dispatcher::{
    WorkspaceHookContext, WorkspaceHookDispatcher, WorkspaceHookPoint,
};
use crate::extensions::workspace_io::HookInput;
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
    agent_runs: crate::agents::run_limits::AgentRunLimits,
    catalog: Arc<ToolCatalog>,
    dispatcher: Arc<ToolDispatcher>,
    hooks: Arc<WorkspaceHookDispatcher>,
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
            .field("hooks", &"<WorkspaceHookDispatcher>")
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
        hooks: Arc<WorkspaceHookDispatcher>,
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
            agent_runs: Default::default(),
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
        let hooks = Arc::new(WorkspaceHookDispatcher::new());
        Arc::new(Self::new(
            Arc::new(ToolCatalog::new()),
            hooks,
            services,
            Arc::new(AsyncExecutionRouter::new()),
            None,
        ))
    }

    /// Principal-scoped admission shared by root, peer, cron and child runs.
    pub fn agent_runs(&self) -> &crate::agents::run_limits::AgentRunLimits {
        &self.agent_runs
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

    /// The principal-owned workspace hook dispatcher.
    #[must_use]
    pub fn hooks(&self) -> &Arc<WorkspaceHookDispatcher> {
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
    async fn fire_observe(&self, point: WorkspaceHookPoint, mut input: HookInput) {
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
        let _ = self.hooks.invoke_hook(point, input).await;
    }

    /// Aggregate the per-turn prompt sections: built-in providers +
    /// workspace-hook `PromptSection` binds (custom sections and
    /// built-in-name augmentations fire in registration order).
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
        let mut custom_names = self
            .hooks
            .prompt_sections(&PrincipalId(request.principal_id.clone()))
            .await;
        custom_names
            .retain(|name| !BUILTIN_PROMPT_SECTIONS.contains(&name.as_str()) && name != "tools");
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

    /// Fire workspace handlers for one prompt section with
    /// the 2s soft-fail budget; returns the combined text.
    async fn fire_section_hook(
        &self,
        name: &str,
        request: &PromptSectionRequest,
    ) -> Option<String> {
        let point = if name == "session_context" {
            WorkspaceHookPoint::SessionContextBuild
        } else {
            WorkspaceHookPoint::PromptSection {
                section: name.to_string(),
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
        let mut ctx = WorkspaceHookContext::new(point, input);
        ctx.runtime = crate::extensions::workspace_io::ToolRuntimeContext::new()
            .with_principal_id(request.principal_id.clone())
            .with_workspace(request.workspace.clone())
            .with_session_id(request.session_id.clone());
        let result = self.hooks.invoke_hook_with_context(ctx).await;
        match result {
            crate::extensions::workspace_io::HookResult::Continue(output) => {
                output.as_text().map(str::to_owned).or_else(|| {
                    if let crate::extensions::workspace_io::HookOutput::Vec(outputs) = output {
                        Some(
                            outputs
                                .iter()
                                .filter_map(|o| o.as_text())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    } else {
                        None
                    }
                })
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
        self.fire_observe(WorkspaceHookPoint::Stop, HookInput::Json(payload))
            .await;
    }

    async fn fire_after_agent_hook(&self, payload: serde_json::Value) {
        self.fire_observe(WorkspaceHookPoint::AfterAgent, HookInput::Json(payload))
            .await;
    }

    async fn set_session_key(&self, agent_id: &str, key: Option<String>) {
        self.session_keys.set(agent_id, key);
    }
}
