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

type RunBindings =
    std::collections::HashMap<(PrincipalId, String), std::sync::Weak<ToolingRuntime>>;

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
    run_bindings: Arc<std::sync::RwLock<RunBindings>>,
    run_binding: Option<std::sync::Weak<ToolingRuntime>>,
    /// One task registry per principal: every executor doing that
    /// principal's background work registers here, so the principal's
    /// tasks live in one place (and the janitor can find them all).
    task_registries: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                PrincipalId,
                crate::async_exec::executor::SharedAsyncTaskRegistry,
            >,
        >,
    >,
    async_executors: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<PrincipalId, Arc<crate::async_exec::executor::AsyncExecutor>>,
        >,
    >,
    agent_runs: Arc<crate::agents::run_limits::AgentRunLimits>,
    catalog: Arc<ToolCatalog>,
    dispatcher: Arc<ToolDispatcher>,
    hooks: Arc<WorkspaceHookDispatcher>,
    session_keys: SessionKeys,
    services: Arc<ExtensionServices>,
    prompt_providers: Arc<std::sync::RwLock<Vec<Arc<dyn PromptSectionProvider>>>>,
    /// Set once the shared prompt-section providers are installed.
    /// Principal-owned tools and hooks have a separate per-principal
    /// guard below so a second principal gets its own workspace bag.
    tool_bag_installed: Arc<AtomicBool>,
    pub(crate) installed_principals:
        Arc<tokio::sync::Mutex<std::collections::HashSet<PrincipalId>>>,
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
            run_bindings: Default::default(),
            run_binding: None,
            task_registries: Default::default(),
            async_executors: Default::default(),
            agent_runs: Default::default(),
            catalog,
            dispatcher,
            hooks,
            session_keys: SessionKeys::new(),
            services,
            prompt_providers: Arc::new(std::sync::RwLock::new(Vec::new())),
            tool_bag_installed: Arc::new(AtomicBool::new(false)),
            installed_principals: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
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

    /// Bind run-owned tools without mutating the daemon/principal catalog.
    /// Shared services, admission, prompts, hooks, and auditing retain their
    /// runtime lifetime. Only tool bindings and fallback session keys are local.
    #[must_use]
    pub fn for_run(self: &Arc<Self>) -> Arc<Self> {
        let catalog = Arc::new(ToolCatalog::overlay(Arc::clone(&self.catalog)));
        Arc::new_cyclic(|binding| Self {
            run_bindings: Arc::clone(&self.run_bindings),
            run_binding: Some(binding.clone()),
            task_registries: Arc::clone(&self.task_registries),
            async_executors: Arc::clone(&self.async_executors),
            agent_runs: Arc::clone(&self.agent_runs),
            dispatcher: Arc::new(
                self.dispatcher
                    .with_catalog(Arc::clone(&catalog), binding.clone()),
            ),
            catalog,
            hooks: Arc::clone(&self.hooks),
            session_keys: SessionKeys::new(),
            services: Arc::clone(&self.services),
            prompt_providers: Arc::clone(&self.prompt_providers),
            tool_bag_installed: Arc::clone(&self.tool_bag_installed),
            installed_principals: Arc::clone(&self.installed_principals),
        })
    }

    /// Principal-owned async tasks survive individual runs. Caller session
    /// attribution and completion routing are supplied on each invocation.
    pub(crate) async fn async_executor_for(
        &self,
        principal_id: &PrincipalId,
        inbox: Arc<peko_session::InboxRegistry>,
    ) -> Arc<crate::async_exec::executor::AsyncExecutor> {
        Arc::clone(
            self.async_executors
                .lock()
                .await
                .entry(principal_id.clone())
                .or_insert_with(|| {
                    Arc::new(crate::async_exec::executor::AsyncExecutor::with_registries(
                        self.task_registry_for(principal_id),
                        inbox,
                    ))
                }),
        )
    }

    /// The task registry for `principal`'s background work.
    pub(crate) fn task_registry_for(
        &self,
        principal_id: &PrincipalId,
    ) -> crate::async_exec::executor::SharedAsyncTaskRegistry {
        Arc::clone(
            self.task_registries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(principal_id.clone())
                .or_default(),
        )
    }

    /// Drop finished tasks past their retention window from every
    /// principal's registry and from the router's own transport (calls
    /// without a principal). Returns how many entries were removed.
    pub(crate) async fn purge_finished_tasks(&self) -> usize {
        let registries: Vec<_> = self
            .task_registries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        let mut purged = self.dispatcher.router().purge_finished_tasks().await;
        for registry in registries {
            purged += registry.write().await.cleanup_completed();
        }
        purged
    }

    pub(crate) fn execution_binding(
        &self,
        principal: &PrincipalId,
        session: &str,
    ) -> Option<Arc<Self>> {
        self.run_bindings
            .read()
            .expect("run bindings lock poisoned")
            .get(&(principal.clone(), session.to_string()))
            .and_then(std::sync::Weak::upgrade)
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
        // IPC/workflow and Async action spawn callbacks re-enter through the shared
        // runtime. Resolve their explicit caller session to its live binding,
        // rather than selecting whichever agent registered last.
        let key = call
            .principal_id
            .as_ref()
            .zip(call.session_id.as_ref())
            .map(|(principal, session)| (PrincipalId(principal.clone()), session.clone()));
        if let Some(key) = key {
            if let Some(binding) = &self.run_binding {
                let mut bindings = self
                    .run_bindings
                    .write()
                    .expect("run bindings lock poisoned");
                bindings.retain(|_, binding| binding.strong_count() > 0);
                bindings.insert(key, binding.clone());
            } else {
                let runtime = self
                    .run_bindings
                    .read()
                    .expect("run bindings lock poisoned")
                    .get(&key)
                    .and_then(std::sync::Weak::upgrade);
                if let Some(runtime) = runtime {
                    return runtime.dispatcher.execute(call).await;
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::metadata::ToolSource;

    /// Subagent runs register in the spawning principal's registry, keyed
    /// by principal rather than agent name: one principal's executors share
    /// it whatever their agent names, and two principals never do.
    #[tokio::test]
    async fn subagent_executors_share_their_principals_registry() {
        use crate::agents::subagent_executor::SubagentExecutor;
        let tooling = ToolingRuntime::standalone();
        let sessions = Arc::new(tokio::sync::RwLock::new(peko_session::SessionManager::new()));
        let alice = PrincipalId("prin_alice".into());
        let registry = tooling.task_registry_for(&alice);
        let root = SubagentExecutor::new(
            Arc::clone(&sessions),
            "primary",
            alice.clone(),
            Arc::clone(&tooling),
        );
        let reviewer = SubagentExecutor::new(
            Arc::clone(&sessions),
            "reviewer",
            alice.clone(),
            Arc::clone(&tooling),
        )
        .with_inbox_registry(Some(
            crate::async_exec::executor::standalone_inbox_registry(),
        ));
        let bob = SubagentExecutor::new(
            sessions,
            "primary",
            PrincipalId("prin_bob".into()),
            Arc::clone(&tooling),
        );
        assert!(Arc::ptr_eq(root.registry(), &registry));
        assert!(Arc::ptr_eq(reviewer.registry(), &registry));
        assert!(!Arc::ptr_eq(bob.registry(), &registry));
    }

    /// The janitor sees every principal's tasks: a principal's executor is
    /// backed by its registry here, and finished tasks past the retention
    /// window are purged from it.
    #[tokio::test]
    async fn janitor_purges_finished_tasks_from_principal_registries() {
        use crate::async_exec::executor::{standalone_inbox_registry, AsyncToolConfig};
        let tooling = ToolingRuntime::standalone();
        let principal = PrincipalId("prin_janitor".into());
        let executor = tooling
            .async_executor_for(&principal, standalone_inbox_registry())
            .await;
        assert!(Arc::ptr_eq(
            executor.registry(),
            &tooling.task_registry_for(&principal)
        ));
        let task = "tool:finished".to_string();
        executor
            .execute(
                task.clone(),
                "tool",
                serde_json::json!({}),
                "session",
                AsyncToolConfig {
                    principal_id: principal.clone(),
                    ..Default::default()
                },
                || async { Ok(serde_json::json!("done")) },
            )
            .await
            .unwrap();
        executor
            .wait_for_completion(&task, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(tooling.purge_finished_tasks().await, 0, "within retention");

        tooling
            .task_registry_for(&principal)
            .write()
            .await
            .get_mut(&task)
            .unwrap()
            .completed_at = Some(chrono::Utc::now() - chrono::Duration::minutes(10));
        assert_eq!(tooling.purge_finished_tasks().await, 1);
        assert!(tooling
            .task_registry_for(&principal)
            .read()
            .await
            .get(&task)
            .is_none());
    }
    use peko_tools_core::Tool;
    use serde_json::{json, Value};

    struct BoundTool(&'static str);
    #[async_trait::async_trait]
    impl Tool for BoundTool {
        fn name(&self) -> &str {
            "Bound"
        }
        fn description(&self) -> String {
            "Return the selected execution binding".into()
        }
        fn parameters(&self) -> Value {
            json!({"type":"object"})
        }
        async fn execute(&self, _: Value) -> anyhow::Result<Value> {
            Ok(json!(self.0))
        }
    }

    struct WaitingTool(Arc<tokio::sync::Notify>);
    #[async_trait::async_trait]
    impl Tool for WaitingTool {
        fn name(&self) -> &str {
            "Wait"
        }
        fn description(&self) -> String {
            "Wait for test completion".into()
        }
        fn parameters(&self) -> Value {
            json!({"type":"object"})
        }
        async fn execute(&self, _: Value) -> anyhow::Result<Value> {
            self.0.notified().await;
            Ok(json!("done"))
        }
    }

    struct CallerContextTool;
    #[async_trait::async_trait]
    impl Tool for CallerContextTool {
        fn name(&self) -> &str {
            "CallerContext"
        }
        fn description(&self) -> String {
            "Report task caller context".into()
        }
        fn parameters(&self) -> Value {
            json!({"type":"object"})
        }
        async fn execute(&self, _: Value) -> anyhow::Result<Value> {
            anyhow::bail!("requires context")
        }
        async fn execute_with_context(
            &self,
            _: Value,
            ctx: &peko_tools_core::ToolContext,
        ) -> anyhow::Result<Value> {
            Ok(
                json!({"session":ctx.session_id, "workspace":ctx.workspace, "principal_name":ctx.principal_name, "agent":ctx.agent_id}),
            )
        }
    }

    fn call(principal: &PrincipalId, session: &str, tool: &str, params: Value) -> ToolCallSpec {
        let mut call = ToolCallSpec::new(tool, params);
        call.principal_id = Some(principal.0.clone());
        call.session_id = Some(session.into());
        call
    }

    #[tokio::test]
    async fn concurrent_runs_and_workflow_callbacks_keep_their_own_binding() {
        let shared = ToolingRuntime::standalone();
        let principal = PrincipalId::generate();
        let first = shared.for_run();
        let second = shared.for_run();
        for (runtime, value) in [(&first, "first"), (&second, "second")] {
            runtime
                .catalog()
                .register(Arc::new(BoundTool(value)), ToolSource::BuiltIn, &principal)
                .await;
        }
        // Both live runs have identical tool names and principal scope.
        let (a, b) = tokio::join!(
            first.execute(call(&principal, "session-a", "Bound", json!({}))),
            second.execute(call(&principal, "session-b", "Bound", json!({}))),
        );
        assert_eq!(a.unwrap().1, "first");
        assert_eq!(b.unwrap().1, "second");
        for (session, expected) in [("session-a", "first"), ("session-b", "second")] {
            let (_, value, success) = shared
                .execute(call(&principal, session, "Bound", json!({})))
                .await
                .unwrap();
            assert!(success);
            assert_eq!(value, expected);
        }
        // ModelList is a shared principal service, available to every run.
        let catalog_dir = tempfile::tempdir().unwrap();
        let models = peko_providers::catalog::ModelCatalog::load_or_init(
            catalog_dir.path().join("models.toml"),
        )
        .await
        .unwrap();
        crate::tools::installation::install_principal_services(
            &shared,
            &principal,
            crate::tools::installation::PrincipalBindings {
                sessions_dir: None,
                plan: None,
                model_catalog: Some(Arc::clone(&models)),
                caller_did: None,
            },
        )
        .await
        .unwrap();
        for runtime in [&first, &second, &shared] {
            assert!(runtime
                .catalog()
                .get("ModelList", &principal)
                .await
                .is_some());
        }
        assert!(shared.catalog().get("Bound", &principal).await.is_none());
        let foreign = PrincipalId::generate();
        assert!(
            !shared
                .execute(call(&foreign, "session-a", "Bound", json!({})))
                .await
                .unwrap()
                .2
        );
        // Run overlays inherit new workspace tools; they share admission and
        // the dispatcher services, but do not advertise sibling bindings.
        shared
            .catalog()
            .register_system(Arc::new(BoundTool("fallback")), ToolSource::BuiltIn)
            .await;
        assert!(Arc::ptr_eq(first.hooks(), shared.hooks()));
        assert!(Arc::ptr_eq(
            first.dispatcher().router(),
            shared.dispatcher().router()
        ));
        assert!(Arc::ptr_eq(
            &first.agent_runs().for_principal(&principal),
            &second.agent_runs().for_principal(&principal)
        ));
        let third = shared.for_run();
        assert_eq!(
            third
                .catalog()
                .get("Bound", &principal)
                .await
                .unwrap()
                .0
                .execute(json!({}))
                .await
                .unwrap(),
            "fallback"
        );
        drop(first);
        assert_eq!(
            shared
                .execute(call(&principal, "session-a", "Bound", json!({})))
                .await
                .unwrap()
                .1,
            "fallback"
        );
        assert_eq!(
            shared
                .execute(call(&principal, "session-b", "Bound", json!({})))
                .await
                .unwrap()
                .1,
            "second"
        );
    }

    #[tokio::test]
    async fn detached_tools_keep_workflow_callback_bindings_until_completion() {
        let shared = Arc::new(ToolingRuntime::new(
            Arc::new(ToolCatalog::new()),
            Arc::new(WorkspaceHookDispatcher::new()),
            Arc::new(ExtensionServices::new()),
            Arc::new(AsyncExecutionRouter::with_default_tool_timeout(1)),
            None,
        ));
        let principal = PrincipalId::generate();
        let run = shared.for_run();
        let done = Arc::new(tokio::sync::Notify::new());
        run.catalog()
            .register(
                Arc::new(WaitingTool(Arc::clone(&done))),
                ToolSource::BuiltIn,
                &principal,
            )
            .await;
        run.catalog()
            .register(Arc::new(BoundTool("run")), ToolSource::BuiltIn, &principal)
            .await;
        let (_, receipt, success) = run
            .execute(call(&principal, "caller", "Wait", json!({})))
            .await
            .unwrap();
        assert!(success, "{receipt}");
        drop(run);
        // A workflow callback arriving after foreground completion still
        // selects its original configuration while the detached tool lives.
        assert_eq!(
            shared
                .execute(call(&principal, "caller", "Bound", json!({})))
                .await
                .unwrap()
                .1,
            "run"
        );
        done.notify_one();
        shared
            .wait_for_async_tasks(std::time::Duration::from_secs(5))
            .await;
        assert!(shared.execution_binding(&principal, "caller").is_none());
    }

    #[tokio::test]
    async fn async_receipts_survive_runs_and_preserve_caller_session() {
        let shared = ToolingRuntime::standalone();
        let inbox = crate::async_exec::executor::standalone_inbox_registry();
        let principal = PrincipalId::generate();
        let task_files = tempfile::tempdir().unwrap();
        let executor = Arc::new(
            crate::async_exec::executor::AsyncExecutor::new(Arc::clone(&inbox))
                .with_task_file_writer(crate::async_exec::executor::TaskFileWriter::new(
                    task_files.path().to_path_buf(),
                )),
        );
        shared
            .async_executors
            .lock()
            .await
            .insert(principal.clone(), executor);
        crate::tools::installation::install_async(&shared, &principal, Arc::clone(&inbox))
            .await
            .unwrap();
        let first = shared.for_run();
        first
            .catalog()
            .register(
                Arc::new(BoundTool("first")),
                ToolSource::BuiltIn,
                &principal,
            )
            .await;
        let (_, receipt, success) = first
            .execute(call(
                &principal,
                "session-a",
                "Async",
                json!({"action":"spawn", "tool":"Bound", "params":{}}),
            ))
            .await
            .unwrap();
        assert!(success, "{receipt}");
        let task_id = receipt["task_id"].as_str().unwrap();
        let (_, output, success) = first
            .execute(call(
                &principal,
                "session-a",
                "Async",
                json!({"action":"output", "task_id":task_id, "block":true, "timeout":5000}),
            ))
            .await
            .unwrap();
        assert!(success, "{output}");
        assert!(output["is_terminal"].as_bool().unwrap(), "{output}");
        assert!(output["result"].to_string().contains("first"), "{output}");
        let executor = shared
            .async_executor_for(&principal, Arc::clone(&inbox))
            .await;
        let registry = executor.registry();
        assert_eq!(
            registry
                .read()
                .await
                .get(&task_id.to_string())
                .unwrap()
                .parent_session_key,
            "session-a"
        );
        first
            .catalog()
            .register(Arc::new(CallerContextTool), ToolSource::BuiltIn, &principal)
            .await;
        let mut context_call = call(
            &principal,
            "session-a",
            "Async",
            json!({"action":"spawn", "tool":"CallerContext", "params":{}}),
        );
        context_call.workspace = Some("/caller/workspace".into());
        context_call.principal_name = Some("caller-principal".into());
        context_call.agent_id = Some("caller-agent".into());
        let (_, context_receipt, success) = first.execute(context_call).await.unwrap();
        assert!(success, "{context_receipt}");
        let (_, context_output, success) = first
            .execute(call(
                &principal,
                "session-a",
                "Async", json!({"action":"output", "task_id":context_receipt["task_id"], "block":true, "timeout":5000}),
            ))
            .await
            .unwrap();
        assert!(success, "{context_output}");
        let context_result = &context_output["result"];
        assert_eq!(context_result["session"], "session-a");
        assert_eq!(context_result["workspace"], "/caller/workspace");
        assert_eq!(context_result["principal_name"], "caller-principal");
        assert_eq!(context_result["agent"], "caller-agent");
        drop(first);
        let second = shared.for_run();
        crate::tools::installation::install_async(&shared, &principal, inbox)
            .await
            .unwrap();
        let (_, later, success) = second
            .execute(call(
                &principal,
                "session-b",
                "Async",
                json!({"action":"output", "task_id":task_id}),
            ))
            .await
            .unwrap();
        assert!(success, "{later}");
        assert_eq!(later["result"], output["result"]);
        let foreign = PrincipalId::generate();
        assert!(
            !second
                .execute(call(
                    &foreign,
                    "session-b",
                    "Async",
                    json!({"action":"output", "task_id":task_id})
                ))
                .await
                .unwrap()
                .2
        );
    }
}
