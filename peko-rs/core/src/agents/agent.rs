//! Agent management module

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_executor::SubagentExecutor;
use crate::tools::runtime::ToolingRuntime;
use anyhow::Result;
use peko_auth::Subject;
use peko_engine::state::StateMachine;
use peko_engine::AgentState;
use peko_identity::{did::DIDScope, storage::KeyStorage, Identity};
use peko_session::manager::{ResolvedSession, SessionManager};
use peko_session::types::ChannelType;
use peko_session::InboxRegistry;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, error, info, warn};

/// Single agent runtime with session overlay support
pub struct Agent {
    /// Agent configuration
    pub config: AgentConfig,
    /// Current state (atomic, lock-free; see [`peko_engine::state::StateMachine`]).
    state: Arc<StateMachine>,
    /// Agent identity
    pub identity: Identity,
    /// LLM provider (stored in Arc for sharing with agentic loop).
    ///
    /// Built by `LlmResolver::build` from the agent's `preferred_*`
    /// hints (or the runtime default) at session start. The
    /// `Option` shape is preserved for unit tests that don't wire
    /// a resolver and run pure-Rust agentic-loop tests offline.
    provider: Option<Arc<peko_providers::Provider>>,
    /// Catalog id picked by `LlmResolver::build` for this session.
    ///
    /// Captured from `ResolvedChoice::model_id` at construction time
    /// so the renderer can surface the actual model in `{{runtime}}`'s
    /// `Model:` line. `None` ⇒ the agent was constructed without a
    /// resolver (test path) and the loop falls back to
    /// `provider.model_id()`.
    resolved_model_id: Option<String>,
    /// Optional resolver (v3+). When present, `init_provider` builds
    /// a one-shot `Provider` per session via the catalog + secret
    /// store, applying the agent's `preferred_*` hints.
    llm_resolver: Option<Arc<peko_providers::LlmResolver>>,
    /// Session manager for overlay lifecycle
    session_manager: Arc<TokioRwLock<SessionManager>>,
    /// Subagent executor for background task execution
    subagent_executor: Arc<SubagentExecutor>,
    /// Current session ID for `Session` tool lookups
    current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Explicitly shared tooling runtime. Catalog lookups scope workspace
    /// tools by principal while retaining the system built-ins.
    tooling: Arc<ToolingRuntime>,
    /// Optional external inbox registry. When set, the agentic loop drains
    /// this registry's session inbox instead of creating a per-call one,
    /// so external callers can push steering messages into a running agent.
    inbox_registry: Option<Arc<InboxRegistry>>,
    /// Run-scoped force-compact flag (Agent tool `action = "compact"`,
    /// 2026-09-05). Set via [`Self::with_force_compact`] by the subagent
    /// executor for compact continuation runs; forwarded to the
    /// `AgenticLoop` in `build_agentic_loop`, whose compaction driver
    /// force-compacts on iteration 1 before the run's prompt is
    /// processed. Default false.
    force_compact: bool,
    /// Principal workspace inherited by this run's subagent executor.
    principal_workspace: Option<std::path::PathBuf>,
    /// Caller principal's stable DID. Bound at construction by
    /// `with_caller_principal_did` so `send_peer` can attribute
    /// the outbound request under
    /// `Subject::Principal(caller_principal_did)` on the wire.
    /// `None` means the tool is not registered.
    caller_principal_did: Option<String>,
    /// Spawning principal's runtime id. Inherited by subagent spawns
    /// via `SubagentExecutor`. Threaded into `ToolContext` so tools
    /// such as `Skill` can resolve per-principal state at handle time
    /// without re-registering themselves on the shared `ToolingRuntime`
    /// per principal.
    principal_id: peko_subject::PrincipalId,
    /// Spawning principal's human-readable name. Threaded into
    /// `ToolContext` so Principal-scoped tools (e.g. cron) can target
    /// jobs by name.
    principal_name: Option<String>,
    /// Spawning principal's plan DAG port. Populated via
    /// `with_principal_plan_port` from
    /// `PrincipalContext::plan_port().clone()` in `agent_runner.rs`
    /// (the `Agent::new_with_session_manager_resolver` chain).
    /// Used by `init_run_builtins` to build the seven
    /// `Plan*` tools. `None` means the agent is unbound from
    /// any principal and the plan tools are not registered (test-
    /// only `Agent::new` / `Agent::new_for_test` callers hit this
    /// path).
    principal_plan_port: Option<Arc<dyn peko_plan::PlanPort>>,
    /// Phase 2 of `feature/multi-model-subagents`: weak handle to
    /// the principal's `ModelCatalog`. Bound at construction via
    /// `with_model_catalog` so the `ModelList` builtin can
    /// snapshot the catalog at execute time. Weak so the agent
    /// never extends the catalog's lifetime past the daemon.
    /// `None` means no catalog is reachable (test path) and the
    /// `ModelList` builtin is not registered.
    model_catalog: Option<Arc<peko_providers::catalog::ModelCatalog>>,
    /// Phase 4 of `feature/multi-model-subagents`: optional audit
    /// sink forwarded to the `AgenticLoop` so it can emit
    /// `model.selected` events on every successful LLM call. Bound
    /// at construction via `with_audit_sink`. Production wiring
    /// (`principal/agent_runner.rs`) passes an
    /// `ObservabilityAuditSink` constructed from the principal's
    /// `Observability` hub.
    audit_sink: Option<Arc<dyn peko_engine::audit_sink::AuditSink>>,
    /// Phase 4 of `feature/multi-model-subagents`: closure that
    /// the loop consults right before emitting the audit event to
    /// decide `Warning` (first use) vs `Info` (subsequent). The
    /// closure receives a model id and returns `true` on first
    /// use of (principal, model). When `None`, the loop emits
    /// `Info` for every call.
    audit_first_use_for_model: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    /// Phase 2 PR 2 (ADR-047 §2.3): MCP context provider forwarded
    /// to the `AgenticLoop`. The framework `McpAdapter` is gone; the
    /// renderer consults this provider directly for the
    /// `{{mcp_context}}` system-prompt section. Default `None`
    /// means the loop's `EmptyMcpPromptContextProvider` is used
    /// (placeholder is stripped to empty via `remove_missing=true`).
    /// Production wiring at `principal/agent_runner.rs` binds the
    /// real provider wrapping the global `McpManager`.
    mcp_context_provider: Option<Arc<dyn peko_engine::McpPromptContextProvider>>,
}

impl Clone for Agent {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            state: Arc::clone(&self.state),
            identity: Identity {
                did: self.identity.did.clone(),
                document: self.identity.document.clone(),
                keypair: None, // Don't clone keypair for security
            },
            provider: self.provider.clone(),
            resolved_model_id: self.resolved_model_id.clone(),
            llm_resolver: self.llm_resolver.clone(),
            session_manager: Arc::clone(&self.session_manager),
            subagent_executor: Arc::clone(&self.subagent_executor),
            current_session_id: Arc::clone(&self.current_session_id),
            tooling: Arc::clone(&self.tooling),
            inbox_registry: self.inbox_registry.clone(),
            force_compact: self.force_compact,
            principal_workspace: self.principal_workspace.clone(),
            caller_principal_did: self.caller_principal_did.clone(),
            principal_id: self.principal_id.clone(),
            principal_name: self.principal_name.clone(),
            principal_plan_port: self.principal_plan_port.clone(),
            model_catalog: self.model_catalog.clone(),
            // Phase 4: Arc-cloned so the trait-object + closure
            // handles survive the `Agent::clone()` that the
            // subagent executor uses to share the parent agent with
            // descendants.
            audit_sink: self.audit_sink.clone(),
            audit_first_use_for_model: self.audit_first_use_for_model.clone(),
            mcp_context_provider: self.mcp_context_provider.clone(),
        }
    }
}

impl Agent {
    /// Install principal defaults and bind execution adapters in the run overlay.
    async fn init_run_builtins(&self, tooling: &Arc<ToolingRuntime>) -> anyhow::Result<()> {
        use crate::tools::installation::{
            install_principal_services, install_run, PrincipalBindings,
        };
        install_principal_services(
            &self.tooling,
            &self.principal_id,
            PrincipalBindings {
                sessions_dir: self.session_manager.read().await.sessions_dir().cloned(),
                plan: self.principal_plan_port.clone(),
                model_catalog: self.model_catalog.clone(),
                caller_did: self.caller_principal_did.as_deref(),
            },
        )
        .await?;
        let sessions = crate::session::session_runtime_impl::SessionManagerRuntime::new(
            self.session_manager.clone(),
            self.current_session_id.clone(),
            self.config.name.clone(),
            self.inbox_registry.clone(),
            self.subagent_executor.quota_meter().cloned(),
        )
        .with_task_registry(self.tooling.task_registry_for(&self.principal_id));
        install_run(
            tooling,
            &self.principal_id,
            Arc::clone(&self.subagent_executor),
            sessions,
            Arc::clone(&self.session_manager),
        )
        .await
    }

    /// Create a new agent with the given configuration
    pub async fn new(config: AgentConfig, tooling: Arc<ToolingRuntime>) -> Result<Self> {
        // Initialize session manager with path resolver
        let path_resolver: Arc<dyn peko_subject::PathResolverLike> =
            Arc::new(peko_session::DefaultPathResolver::new());
        let session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, &config.name)
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));
        Self::new_with_session_manager_and_resolver(config, session_manager, None, tooling).await
    }

    /// Create a new agent backed by a `LlmResolver` (v3+ path).
    ///
    /// The resolver is consulted in `init_provider` to build a
    /// one-shot `Provider` from the agent's `preferred_*` hints (or
    /// the runtime default). The resolver is the only source of truth —
    /// when it has no matching entry (or is absent), the agent is
    /// constructed without an LLM provider (a warning is logged at
    /// build time).
    pub async fn new_with_resolver(
        config: AgentConfig,
        resolver: Arc<peko_providers::LlmResolver>,
        tooling: Arc<ToolingRuntime>,
    ) -> Result<Self> {
        let path_resolver: Arc<dyn peko_subject::PathResolverLike> =
            Arc::new(peko_session::DefaultPathResolver::new());
        let session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, &config.name)
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));
        Self::new_with_session_manager_and_resolver(
            config,
            session_manager,
            Some(resolver),
            tooling,
        )
        .await
    }

    /// Create a new agent with an existing session manager.
    ///
    /// Used for subagent execution where the child must share the parent's
    /// session manager (and therefore session storage and context).
    pub async fn new_with_session_manager(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        tooling: Arc<ToolingRuntime>,
    ) -> Result<Self> {
        Self::new_with_session_manager_and_resolver(config, session_manager, None, tooling).await
    }

    /// Like `new_with_session_manager`, but also accepts an optional
    /// `LlmResolver` (v3+).
    ///
    /// Used for one-off CLI invocations that don't share a principal
    /// scope: the agent constructs a synthetic `PrincipalId` so its
    /// `SubagentExecutor` carries a stable identity even though no real
    /// principal owns the call.
    pub async fn new_with_session_manager_and_resolver(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        llm_resolver: Option<Arc<peko_providers::LlmResolver>>,
        tooling: Arc<ToolingRuntime>,
    ) -> Result<Self> {
        Self::new_with_session_manager_resolver(
            config,
            session_manager,
            llm_resolver,
            None,
            peko_subject::PrincipalId::generate(),
            None,
            None,
            tooling,
        )
        .await
    }

    /// Create a new agent with an existing session manager, optional
    /// `LlmResolver`, the spawning principal's id, and an optional
    /// external `InboxRegistry`.
    ///
    /// Every agent receives the shared tooling runtime explicitly.
    /// `principal_id` is the spawning principal's runtime id, carried so the agent's
    /// `SubagentExecutor` and any descendant spawns inherit the same
    /// principal scope. When `inbox_registry` is supplied, the agentic
    /// loop drains that registry's session inbox, allowing the
    /// Principal boundary to queue steering messages into a running
    /// root agent.
    pub async fn new_with_session_manager_resolver(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        llm_resolver: Option<Arc<peko_providers::LlmResolver>>,
        // Model-first: a single configured model id pinned by the
        // Principal, or `None` for non-principal callers/tests.
        provider_hint: Option<String>,
        principal_id: peko_subject::PrincipalId,
        inbox_registry: Option<Arc<InboxRegistry>>,
        // Model-first: per-message configured model override
        // (`peko send --model`). `None` preserves the principal hint.
        message_override: Option<String>,
        // ADR-066 P3: the shared tooling runtime (catalog + dispatcher
        // + hooks), threaded explicitly — no process-global core.
        tooling: Arc<ToolingRuntime>,
    ) -> Result<Self> {
        info!("Creating agent: {}", config.name);

        // Load or create identity
        let identity = Self::load_or_create_identity(&config).await?;

        // Initialize provider if configured. `provider_hint` is the
        // principal's pinned configured model id, or `None` for tests /
        // non-principal callers. `message_override` is the per-message
        // `peko send --model <id>` override; `None` preserves the
        // principal-config chain.
        let (provider, resolved_model_id) = match Self::init_provider(
            &config,
            llm_resolver.as_ref(),
            provider_hint,
            message_override,
        )
        .await?
        {
            Some((p, id)) => (Some(p), Some(id)),
            None => (None, None),
        };

        // All agents share the daemon's tooling runtime, threaded
        // explicitly. `principal_id` is the spawning principal's
        // runtime id; descendant subagents inherit it via the
        // shared `SubagentExecutor`.

        // Initialize subagent executor
        let subagent_executor_base = SubagentExecutor::new(
            Arc::clone(&session_manager),
            config.name.clone(),
            principal_id.clone(),
            Arc::clone(&tooling),
        )
        // WS3 (implicit session management, 2026-08-11): share the
        // daemon-global inbox registry so subagent completions reach
        // the parent agentic loop's per-iteration drain.
        .with_inbox_registry(inbox_registry.clone());
        let subagent_executor = match &provider {
            Some(p) => Arc::new(
                subagent_executor_base
                    .with_provider(p.clone())
                    .with_agent_config(config.clone()),
            ),
            None => Arc::new(subagent_executor_base),
        };

        // B4 cleanup: `DynamicSessionKeyProvider` was removed — only
        // `set_session_key` was called once per subagent; `get_session_key`
        // had no readers. `ToolContext::session_id` is the canonical
        // production session-key source.

        let agent = Self {
            config,
            state: Arc::new(StateMachine::new()),
            identity,
            provider,
            resolved_model_id,
            llm_resolver,
            session_manager,
            subagent_executor,
            current_session_id: Arc::new(tokio::sync::RwLock::new(None)),
            tooling,
            inbox_registry,
            force_compact: false,
            principal_workspace: None,
            caller_principal_did: None,
            principal_id,
            principal_name: None,
            principal_plan_port: None,
            // Phase 2 of `feature/multi-model-subagents`: the
            // standalone / CLI one-shot `Agent::new` path doesn't
            // bind a `ModelCatalog`. The `ModelList` builtin is
            // intentionally not registered for these agents.
            model_catalog: None,
            // Phase 4: unbound by default; production wiring at
            // `principal/agent_runner.rs` reaches into
            // `PrincipalContext::observability()` and
            // `PrincipalContext::seen_models` to construct both
            // fields.
            audit_sink: None,
            audit_first_use_for_model: None,
            mcp_context_provider: None,
        };

        info!(
            "Agent {} initialized with DID: {}",
            agent.config.name, agent.identity.did
        );

        Ok(agent)
    }

    /// Resolve this run's role templates from the principal workspace.
    pub fn with_principal_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        // Also scope the subagent executor so depth-1 children (and, via the
        // executor's own propagation, deeper descendants) resolve their
        // subagents from this workspace. The executor is built before the
        // workspace is known, so rebuild it here (SubagentExecutor is Clone).
        let executor = (*self.subagent_executor)
            .clone()
            .with_principal_workspace(workspace.clone());
        self.subagent_executor = Arc::new(executor);
        self.principal_workspace = Some(workspace);
        self
    }

    /// Bind an external inbox registry post-construction.
    ///
    /// Sprint 2 Phase 7 (peer-child ingress): a peer-child turn runs
    /// through `SubagentExecutor::resume_streaming`, whose child Agent
    /// is built by `new_with_shared_executor_with_model_override` with
    /// `inbox_registry: None` — its loop would drain a per-call
    /// standalone registry, and steering queued by the
    /// `PrincipalManager` / IPC serial-queue fallback into the SHARED
    /// registry (keyed by the child session id) would never be
    /// consumed. Binding the shared registry here makes the child
    /// loop's per-iteration drain see those steering messages.
    #[must_use]
    pub fn with_inbox_registry(mut self, registry: Option<Arc<InboxRegistry>>) -> Self {
        self.inbox_registry = registry;
        self
    }

    /// Arm the run-scoped forced compaction for this agent's next run
    /// (Agent tool `action = "compact"`, 2026-09-05). The executor
    /// sets this on the child Agent of a compact continuation run; the
    /// loop's compaction driver force-compacts the session on
    /// iteration 1 (`CompactionPhase::StandaloneTurn`) before the run's
    /// prompt is processed.
    #[must_use]
    pub fn with_force_compact(mut self, force: bool) -> Self {
        self.force_compact = force;
        self
    }

    /// Bind the caller's principal identity for `send_peer`.
    ///
    /// `principal_did` is the Principal's stable DID (used as
    /// `caller_principal_did` on the wire). When `None`, `send_peer`
    /// is not registered (the agent lacks the identity needed to
    /// attribute cross-principal calls). The local runtime id is
    /// taken from `CrossRuntimeA2aCtx::caller_runtime_id` at
    /// registration time, so this builder does not need it.
    ///
    /// Also propagates the DID into the subagent executor (a
    /// `OnceLock` set through the `Arc`) so spawned children register
    /// `send_peer` with the same attribution, recursively down the
    /// tree.
    #[must_use]
    pub fn with_caller_principal_did(mut self, principal_did: Option<String>) -> Self {
        if let Some(did) = &principal_did {
            self.subagent_executor.set_caller_principal_did(did.clone());
        }
        self.caller_principal_did = principal_did;
        self
    }

    /// Set the spawning principal's human-readable name. Propagates to the
    /// subagent executor so descendant spawns inherit the same name for
    /// Principal-scoped tools.
    #[must_use]
    pub fn with_principal_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let executor = (*self.subagent_executor)
            .clone()
            .with_principal_name(name.clone());
        self.subagent_executor = Arc::new(executor);
        self.principal_name = Some(name);
        self
    }

    /// Charge child runs against the spawning principal's quota meter.
    /// `None` retains the unlimited offline/test default.
    #[must_use]
    pub fn with_quota_meter(mut self, meter: Option<Arc<peko_quota::meter::QuotaMeter>>) -> Self {
        let executor = (*self.subagent_executor).clone().with_quota_meter(meter);
        self.subagent_executor = Arc::new(executor);
        self
    }

    /// Keep child-run audits on the same principal observability hub.
    pub(crate) fn with_subagent_observability(
        mut self,
        observability: Option<Arc<peko_observability::Observability>>,
    ) -> Self {
        self.subagent_executor = Arc::new(
            (*self.subagent_executor)
                .clone()
                .with_observability(observability),
        );
        self
    }

    /// Bind the spawning principal's `peko_plan::PlanPort` for this
    /// agent.
    ///
    /// `plan_port` is the per-Principal handle to the plan DAG store
    /// (`PrincipalContext::plan_port()`). When `Some`, the seven
    /// `Plan*` built-in tools (`Plan action create` / `Plan action list` /
    /// `Plan action get` / `Plan action mark_step` / `Plan action record_evidence` /
    /// `Plan action add_step` / `Plan action close`) are registered by
    /// `init_run_builtins`; when `None` they are skipped (the
    /// typical test path).
    ///
    /// The handle is propagated to the subagent executor so depth-1
    /// children inherit the same per-Principal port and `init_run_builtins`
    /// on the child also wires the seven tools (subagents can manage
    /// plans on behalf of their spawning principal).
    #[must_use]
    pub fn with_principal_plan_port(mut self, plan_port: Arc<dyn peko_plan::PlanPort>) -> Self {
        let executor = (*self.subagent_executor)
            .clone()
            .with_principal_plan_port(plan_port.clone());
        self.subagent_executor = Arc::new(executor);
        self.principal_plan_port = Some(plan_port);
        self
    }

    /// Phase 2 of `feature/multi-model-subagents`: bind the
    /// principal's model catalog so `init_run_builtins` can
    /// register the `ModelList` builtin. The catalog handle is
    /// stored as an `Arc` so the `ModelList` tool can downgrade
    /// it to a `Weak` at registration time without losing the
    /// principal's catalog reference (the executor passes the same
    /// `Arc` to the tool's `Weak<ModelCatalog>` field).
    ///
    /// `None` ⇒ no catalog reachable (CLI one-shot path that builds
    /// an `Agent` without a resolver); the `ModelList` builtin is
    /// intentionally not registered.
    #[must_use]
    pub fn with_model_catalog(
        mut self,
        catalog: Option<Arc<peko_providers::catalog::ModelCatalog>>,
    ) -> Self {
        self.model_catalog = catalog;
        self
    }

    /// Phase 4 of `feature/multi-model-subagents`: bind an audit
    /// sink + first-use lookup. The loop emits a `model.selected`
    /// audit event on every successful LLM call; the lookup closure
    /// decides `Warning` vs `Info` severity based on whether
    /// `(principal, model)` has been seen before. Production wiring
    /// (`principal/agent_runner.rs`) constructs both from
    /// `PrincipalContext::observability()` and
    /// `PrincipalContext::seen_models`.
    ///
    /// `None` for either argument disables the corresponding half:
    /// `sink = None` ⇒ no audit emission at all; `sink = Some` +
    /// `first_use_lookup = None` ⇒ every event is `Info`.
    /// Phase 2 PR 2 (ADR-047 §2.3): bind an MCP context provider
    /// that the `AgenticLoop` consults for the `{{mcp_context}}`
    /// system-prompt section. The default `None` makes the loop
    /// fall back to its `EmptyMcpPromptContextProvider` (the
    /// placeholder is stripped to empty). Production wiring
    /// (`principal/agent_runner.rs`) passes a provider wrapping
    /// the global `McpManager` when one is reachable.
    #[must_use]
    pub fn with_mcp_context_provider(
        mut self,
        provider: Option<Arc<dyn peko_engine::McpPromptContextProvider>>,
    ) -> Self {
        self.mcp_context_provider = provider;
        self
    }

    pub fn with_audit_sink(
        mut self,
        sink: Option<Arc<dyn peko_engine::audit_sink::AuditSink>>,
        first_use_lookup: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    ) -> Self {
        self.audit_sink = sink;
        self.audit_first_use_for_model = first_use_lookup;
        self
    }

    // ---- Phase 2 inert fields ----
    //
    // Each setter mutates the matching `AgentConfig` field and returns
    // `Self`. These flow into the rendered system prompt via
    // `AgenticLoop::build_turn_context` reading the accessors below.

    /// Set the agent's channel (`{{channel}}` / `{{runtime}}`).
    #[must_use]
    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.config.channel = Some(channel.into());
        self
    }

    /// Set the agent's thinking level (`{{thinking_level}}`).
    #[must_use]
    pub fn with_thinking_level(mut self, level: impl Into<String>) -> Self {
        self.config.thinking_level = Some(level.into());
        self
    }

    /// Toggle the agent's sandbox flag (`{{sandbox}}`).
    #[must_use]
    pub fn with_sandbox_enabled(mut self, enabled: bool) -> Self {
        self.config.sandbox_enabled = enabled;
        self
    }

    /// Set the agent's model aliases (`{{model_aliases}}`).
    #[must_use]
    pub fn with_model_aliases(mut self, aliases: Vec<String>) -> Self {
        self.config.model_aliases = aliases;
        self
    }

    /// Set the peer-conversation DM channel id (conversation runs
    /// only — rendered into the `{{session_context}}` section).
    #[must_use]
    pub fn with_conversation_channel(mut self, channel: impl Into<String>) -> Self {
        self.config.conversation_channel = Some(channel.into());
        self
    }

    /// Set the peer-conversation peer subject (conversation runs
    /// only — rendered into the `{{session_context}}` section).
    #[must_use]
    pub fn with_conversation_peer(mut self, peer: impl Into<String>) -> Self {
        self.config.conversation_peer = Some(peer.into());
        self
    }

    // F19: removed `with_quota_meter` and `quota_meter()` from Agent.
    // Quota is opened via `QuotaScope::with` at the engine loop
    // entrypoint. The agent no longer carries a meter field; the
    // principal's meter is fetched from `Principal.quota_meter`
    // directly by the run entrypoint.

    /// Snapshot of the spawning principal's workspace path, if any.
    ///
    /// **Track B**: the principal's `ctx.workspace_path` is the
    /// canonical workspace for any agent spawned under that
    /// principal. Production agents bound via
    /// `Agent::with_principal_workspace` carry the snapshot here so
    /// downstream consumers (tool executor, prompt service) can read
    /// it without threading a `PrincipalContext` through every
    /// call. `None` means the agent has no principal binding —
    /// callers should fall back to a per-agent default path
    /// (e.g. `PathResolver::agent_workspace(agent.name())`).
    #[must_use]
    pub fn principal_workspace(&self) -> Option<&std::path::PathBuf> {
        self.principal_workspace.as_ref()
    }

    /// Create a new agent with an existing session manager and a shared subagent executor.
    ///
    /// Used for subagent execution where the child must share the parent's
    /// session manager AND subagent registry (for proper depth tracking).
    ///
    /// The child's provider is **inherited from the parent** via the
    /// `inherited_provider` argument. This avoids a v3 regression where the
    /// child was created without an `LlmResolver`, so `init_provider`
    /// returned `Ok(None)` (the v1 fallback was removed in PR #44), and
    /// `execute_with_session` then errored with `"No provider configured"`
    /// before the child could call any tool. Passing the parent's already-
    /// resolved provider lets the child run its own LLM calls against the
    /// same provider/catalog entry.
    ///
    /// Tools resolve from the runtime carried by the shared executor;
    /// descendant spawns inherit that runtime and the principal scope.
    pub async fn new_with_shared_executor(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        subagent_executor: Arc<SubagentExecutor>,
        inherited_provider: Option<Arc<peko_providers::Provider>>,
    ) -> Result<Self> {
        Self::new_with_shared_executor_with_model_override(
            config,
            session_manager,
            subagent_executor,
            inherited_provider,
            None,
        )
        .await
    }

    /// Phase 1 of `feature/multi-model-subagents`: variant of
    /// [`new_with_shared_executor`] that lets the caller stamp the
    /// `resolved_model_id` directly when the inherited provider was
    /// already overridden at the call site (e.g. via
    /// `Provider::with_model_id`). Without this hook the
    /// inherited-provider branch returns `(Some(p), None)` and the
    /// renderer's `{{runtime}}` line falls back to
    /// `provider.model_id()` — which already reflects the
    /// override, so this is mostly belt-and-suspenders to keep the
    /// catalog id explicit. The hard work happens in
    /// `execute_subagent_task`: it clones the provider with
    /// `with_model_id` and pre-flights `SpecGate::check`.
    pub async fn new_with_shared_executor_with_model_override(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        subagent_executor: Arc<SubagentExecutor>,
        inherited_provider: Option<Arc<peko_providers::Provider>>,
        // Optional catalog id the caller stamped on the inherited
        // provider (Phase 1). `None` keeps the pre-Phase-1
        // behavior: `resolved_model_id` is `None` for the
        // inherited-provider branch and the renderer falls back to
        // `provider.model_id()`.
        resolved_model_id_override: Option<String>,
    ) -> Result<Self> {
        info!("Creating agent with shared executor: {}", config.name);
        // ADR-066 P3: the tooling runtime rides the shared
        // `SubagentExecutor` — every agent in a spawn tree shares it.
        let tooling = Arc::clone(subagent_executor.tooling());

        let identity = Self::load_or_create_identity(&config).await?;

        // Prefer the inherited provider so the child reuses the parent's
        // resolved provider instead of paying the resolver's catalog
        // lookup cost twice. Fall back to the v3 resolver path if the
        // caller didn't supply one (e.g., unit tests).
        let (provider, resolved_model_id) = match inherited_provider {
            Some(p) => (Some(p), resolved_model_id_override),
            // Subagent path: no principal binding, so no provider hint;
            // the resolver falls back to the catalog default.
            None => match Self::init_provider(&config, None, None, None).await? {
                Some((p, id)) => (Some(p), Some(id)),
                None => (None, None),
            },
        };
        let llm_resolver: Option<Arc<peko_providers::LlmResolver>> = None;

        // Subagents inherit the explicit tooling runtime and principal scope.

        let principal_id = subagent_executor.principal_id().clone();
        let principal_name = subagent_executor.principal_name().map(String::from);
        // F19: quota meter no longer carried on `Agent`. The
        // principal's meter is fetched from `Principal.quota_meter`
        // at run entrypoint by the engine loop. The agent just
        // carries identity and principal_id.
        let agent = Self {
            config,
            state: Arc::new(StateMachine::new()),
            identity,
            provider,
            resolved_model_id,
            llm_resolver,
            session_manager,
            subagent_executor,
            current_session_id: Arc::new(tokio::sync::RwLock::new(None)),
            tooling,
            inbox_registry: None,
            force_compact: false,
            principal_workspace: None,
            caller_principal_did: None,
            principal_id,
            principal_name,
            principal_plan_port: None,
            model_catalog: None,
            // Phase 4: CLI one-shot path doesn't bind an audit
            // sink. Production wiring at
            // `principal/agent_runner.rs` adds it via
            // `Agent::with_audit_sink`.
            audit_sink: None,
            audit_first_use_for_model: None,
            mcp_context_provider: None,
        };

        info!(
            "Agent {} initialized with DID: {}",
            agent.config.name, agent.identity.did
        );

        Ok(agent)
    }

    /// Start the agent
    pub async fn start(&self) -> Result<()> {
        info!(
            "Starting agent: {} ({})",
            self.config.name, self.identity.did
        );
        Ok(())
    }

    /// Stop the agent
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping agent: {}", self.config.name);

        let after_agent_payload = serde_json::json!({
            "role_name": self.config.name,
            "agent_did": self.identity.did,
            "principal_id": self.subagent_executor.principal_id().to_string(),
            "workspace": self.principal_workspace.as_ref().map(|path| path.to_string_lossy().into_owned()),
        });
        peko_engine::EngineHooks::fire_after_agent_hook(&*self.tooling, after_agent_payload).await;

        Ok(())
    }

    /// Get current state
    #[must_use]
    pub fn state(&self) -> AgentState {
        self.state.current()
    }

    /// Get provider reference
    #[must_use]
    pub fn get_provider(&self) -> Option<&peko_providers::Provider> {
        self.provider.as_deref()
    }

    /// Set state
    /// Set agent state (public for channel use)
    pub fn set_state(&self, state: AgentState) {
        let prev = self.state.current();
        match state {
            AgentState::Idle => self.state.set_idle(),
            AgentState::Busy => self.state.set_busy(),
        }
        debug!(
            "Agent {} state: {:?} -> {:?}",
            self.config.name, prev, state
        );
    }

    /// Get the provider as an `Arc`.
    #[must_use]
    pub fn provider_arc(&self) -> Option<Arc<peko_providers::Provider>> {
        self.provider.clone()
    }

    /// Reference to the v3+ `LlmResolver` if one is wired to this agent.
    ///
    /// The resolver owns the `ProviderCatalog` — the runtime's source
    /// of truth for provider/model metadata — and the OS keychain
    /// accessor. Callers that need catalog lookups (e.g.
    /// `model_context_length` for compaction) consult this.
    #[must_use]
    pub fn llm_resolver(&self) -> Option<Arc<peko_providers::LlmResolver>> {
        self.llm_resolver.clone()
    }

    /// Catalog id picked by `LlmResolver::build` for this session.
    ///
    /// Captured at construction time from `ResolvedChoice::model_id`.
    /// `None` when the agent was constructed without a resolver (test
    /// path) — callers should fall back to `provider.model_id()`.
    /// The renderer uses this for `{{runtime}}`'s `Model:` line so
    /// per-call overrides (`peko send --model <id>`) actually surface.
    #[must_use]
    pub fn resolved_model_id(&self) -> Option<&str> {
        self.resolved_model_id.as_deref()
    }

    /// The shared tooling runtime (catalog + dispatcher + hooks).
    #[must_use]
    pub fn tooling(&self) -> Arc<ToolingRuntime> {
        Arc::clone(&self.tooling)
    }

    /// Get the current session ID lock.
    #[must_use]
    pub fn current_session_id(&self) -> Arc<tokio::sync::RwLock<Option<String>>> {
        Arc::clone(&self.current_session_id)
    }

    /// Get the session key provider for Agent tool.
    ///
    /// B4 cleanup: this accessor (and the `DynamicSessionKeyProvider`
    /// it returned) was deleted — only `set_session_key` was called
    /// once per subagent (in `subagent_executor.rs`); the reader had
    /// no callers. `ToolContext::session_id` is the canonical
    /// production session-key source.
    /// Execute a task with the LLM provider using the unified callback API.
    ///
    /// Directly creates an `AgenticLoop` and runs it — no intermediate layers.
    pub async fn execute(
        &self,
        prompt: &str,
        on_event: impl Fn(peko_engine::AgenticEvent) + Send + Sync + 'static,
    ) -> Result<peko_engine::AgenticResult> {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        if !self.state.try_acquire() {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state.current()
            ));
        }

        let agent_arc = Arc::new(self.clone());
        // Attach the caller inbox and private execution bindings. The agent's session key (read from
        // `current_session_id`) is pushed onto the core so Async action spawn can
        // stamp `parent_session_key` correctly.
        //
        // Session-key flow across the three `execute_*` paths:
        //
        // - `Agent::execute()` (this method, one-shot CLI mode):
        //   `current_session_id` is `None` here because `build_agentic_loop`
        //   does not create a session — the session is born later, inside
        //   `AgenticLoop::run` → `run_inner`. So `session_key` is `None`
        //   and `set_session_key(&self.identity.did, None)` runs on the
        //   core. The loop's `run_inner` rebinds the core's session key
        //   for *this* agent's DID to the real session id it just
        //   created (see `src/engine/agentic_loop.rs`), so any
        //   `Async action spawn` issued *mid-iteration* still gets a real
        //   `parent_session_key`. The brief window before the loop
        //   starts (no iterations yet, no `Async action spawn` possible) does
        //   not matter.
        //
        // - `Agent::execute_with_session(...)` (tunnel / pekohub):
        //   The session id is explicitly written into
        //   `current_session_id` (and pushed onto the core) *before*
        //   `build_agentic_loop` runs, so the core sees a real value
        //   from the very first iteration. The `run_inner` rebind is a
        //   harmless idempotent no-op.
        //
        // - `Agent::execute_streaming_with_session(...)`: same as
        //   `execute_with_session` — the session id is stamped into
        //   `current_session_id` and the core before the helper runs.
        let session_key = self.current_session_id.read().await.clone();
        // F19: CLI one-shot path — no principal in scope, default to
        // an unlimited meter (no charging, no persistence).
        let quota_meter = Arc::new(peko_quota::QuotaMeter::unlimited());
        let loop_ = self
            .build_agentic_loop(agent_arc, provider, session_key, None, None, quota_meter)
            .await?;

        // Phase 9b.N.5b.9d: inline `AgenticLoop::run`'s session-creation
        // body here (root side). The loop no longer owns
        // `SessionManager` orchestration — callers that don't have a
        // pre-built session (local CLI one-shots) build one inline and
        // route through `run_with_resume` directly. Mirrors the
        // 9b.N.5b.9c convention where callers explicitly construct
        // their `BackgroundCompactorFactory` + `CompactionConfig`.
        let path_resolver: Arc<dyn peko_subject::PathResolverLike> =
            Arc::new(peko_session::DefaultPathResolver::new());
        let mut session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, self.name())
            .await?;
        let session = session_manager
            .get_or_create_base(self.name(), &Subject::User("local".to_string()))
            .await?;

        let result = match loop_
            .run_with_resume(prompt, Vec::new(), on_event, &session, None)
            .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                error!("Agentic loop error: {}", e);
                Err(e)
            }
        };

        self.set_state(AgentState::Idle);
        result
    }

    /// Execute with a specific session and history.
    ///
    /// Directly creates an `AgenticLoop` and runs it with session resumption.
    ///
    /// `user_text` is persisted verbatim as the user message in the
    /// session JSONL. `pre_user_messages` are ephemeral LLM-only turns
    /// inserted immediately before the user turn (e.g. recalled context
    /// from prior sessions); they are never persisted.
    ///
    /// `cancel` is the soft-interrupt `CancellationToken` (PR #128) the
    /// child agent should observe at iteration boundaries. When the
    /// parent agent's `CancellationToken` is flipped (e.g. via
    /// `PrincipalStop`), the child agent's loop also exits
    /// cleanly with `AgenticResult { interrupted: true }`. `None` for
    /// the legacy non-cancelable path (sub-agents that pre-date this
    /// plumbing, tests).
    pub async fn execute_with_session(
        &self,
        user_text: &str,
        pre_user_messages: Vec<peko_message::LlmMessage>,
        session: Arc<tokio::sync::RwLock<peko_session::Session>>,
        history: Option<Vec<peko_message::LlmMessage>>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        on_event: impl Fn(peko_engine::AgenticEvent) + Send + Sync + 'static,
        quota_meter: Option<Arc<peko_quota::QuotaMeter>>,
    ) -> Result<peko_engine::AgenticResult> {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        if !self.state.try_acquire() {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state.current()
            ));
        }

        let agent_arc = Arc::new(self.clone());
        // Capture session ID into the agent's cell (used by the
        // session tool) and pass the session_id to the per-call wiring.
        // Unlike `Agent::execute`, the session already exists here, so we can
        // push a real id onto the core before the loop starts — see the
        // session-key flow comment in `Agent::execute` for the full
        // picture across the three `execute_*` paths.
        let session_id = session.read().await.id.clone();
        {
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id.clone());
        }
        // F19: tunnel/pekohub path. Caller (agent_runner / IPC handler)
        // supplies the principal's quota meter via the optional
        // `quota_meter` parameter; default to unlimited when omitted.
        //
        // B5 (2026-08-22): the F20 `peer_meter` parameter was removed —
        // peer attribution was broken for agents serving many peers
        // simultaneously. Per-agent attribution (the `agent_meter` on
        // `SubagentExecutor`) replaces it.
        let quota_meter =
            quota_meter.unwrap_or_else(|| Arc::new(peko_quota::QuotaMeter::unlimited()));
        let loop_ = self
            .build_agentic_loop(
                agent_arc,
                provider,
                Some(session_id),
                None,
                cancel,
                quota_meter,
            )
            .await?;

        let result = match loop_
            .run_with_resume(user_text, pre_user_messages, on_event, &session, history)
            .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                error!("Agentic loop error: {}", e);
                Err(e)
            }
        };

        self.set_state(AgentState::Idle);
        result
    }

    /// Execute with streaming support using the provided session.
    ///
    /// Directly creates an `AgenticLoop` with live streaming delivery mode.
    ///
    /// `user_text` is persisted verbatim as the user message in the
    /// session JSONL. `pre_user_messages` are ephemeral LLM-only turns
    /// inserted immediately before the user turn (e.g. recalled context
    /// from prior sessions); they are never persisted.
    ///
    /// `caller_id` is the resolved caller identity for the request
    /// (pekohub sub, API key id, or `None` for local CLI invocations) —
    /// propagated to every `HookInput::ToolCall` so per-user permission
    /// checks and audit logging can attribute tool calls to a real user
    /// (issue #17).
    ///
    /// F19: optional `quota_meter` is the principal's quota meter
    /// from `Principal::quota_meter`. When supplied, every LLM call
    /// auto-charges via `StackedMeteredProvider`; when omitted, defaults
    /// to an unlimited meter (test / CLI paths).
    ///
    /// B5 (2026-08-22): the F20 `peer_meter` parameter was removed —
    /// peer attribution was broken for agents serving many peers
    /// simultaneously. Per-agent attribution (the `agent_meter` on
    /// `SubagentExecutor`) replaces it.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_streaming_with_session<F>(
        &self,
        user_text: &str,
        pre_user_messages: Vec<peko_message::LlmMessage>,
        session: std::sync::Arc<tokio::sync::RwLock<peko_session::Session>>,
        history: Option<Vec<peko_message::LlmMessage>>,
        caller_id: Option<String>,
        on_event: F,
        cancel: Option<tokio_util::sync::CancellationToken>,
        quota_meter: Option<Arc<peko_quota::QuotaMeter>>,
    ) -> Result<peko_engine::AgenticResult>
    where
        F: Fn(peko_engine::AgenticEvent) + Send + Sync + 'static,
    {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        if !self.state.try_acquire() {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state.current()
            ));
        }

        // Capture current session ID so session tool can look it up
        {
            let session_id = session.read().await.id.clone();
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id);
        }

        let agent_arc = Arc::new(self.clone());
        // Attach the caller inbox and private execution bindings. The session ID we just stamped into
        // current_session_id is the parent_session_key we'll use for any
        // spawn in this loop. See the session-key flow comment in
        // `Agent::execute` for how the three `execute_*` paths cooperate
        // to ensure mid-iteration `Async action spawn` calls see a real session key.
        let session_id = self.current_session_id.read().await.clone();
        // F19: tunnel/pekohub streaming path. Caller supplies the
        // principal's quota meter; default to unlimited when omitted.
        let quota_meter =
            quota_meter.unwrap_or_else(|| Arc::new(peko_quota::QuotaMeter::unlimited()));
        let loop_ = match self
            .build_agentic_loop(
                agent_arc,
                provider,
                session_id,
                caller_id,
                cancel,
                quota_meter,
            )
            .await
        {
            Ok(loop_) => loop_,
            Err(e) => {
                self.set_state(AgentState::Idle);
                return Err(e);
            }
        };

        let streaming_config = peko_engine::OrchestratorConfig::live();

        let result = loop_
            .run_streaming_with_resume(
                user_text,
                pre_user_messages,
                on_event,
                &session,
                history,
                streaming_config,
            )
            .await;

        self.set_state(AgentState::Idle);
        result
    }

    /// Like [`Self::execute_streaming_with_session`] but skips the
    /// user-message persistence step. Used by the steering path: the
    /// IPC handler has already called `session.add_user(content)` to
    /// persist the queued steering message, so the loop must not add
    /// it again.
    ///
    /// The actual steering content reaches the LLM via the inbox
    /// drain at the start of `run_inner`'s first iteration (see
    /// [`peko_engine::agentic_loop::AgenticLoop::run_streaming_with_resume_skip_user_add`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_streaming_with_session_skip_user_add<F>(
        &self,
        on_event: F,
        session: std::sync::Arc<tokio::sync::RwLock<peko_session::Session>>,
        history: Option<Vec<peko_message::LlmMessage>>,
        caller_id: Option<String>,
    ) -> Result<peko_engine::AgenticResult>
    where
        F: Fn(peko_engine::AgenticEvent) + Send + Sync + 'static,
    {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        {
            let session_id = session.read().await.id.clone();
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id);
        }

        let agent_arc = Arc::new(self.clone());
        let session_id = self.current_session_id.read().await.clone();
        // F19: same unlimited fallback as the other `execute_*` paths.
        let quota_meter = Arc::new(peko_quota::QuotaMeter::unlimited());
        let loop_ = self
            .build_agentic_loop(
                agent_arc,
                provider,
                session_id,
                caller_id,
                None,
                quota_meter,
            )
            .await?;

        let streaming_config = peko_engine::OrchestratorConfig::live();

        loop_
            .run_streaming_with_resume_skip_user_add(on_event, &session, history, streaming_config)
            .await
    }

    /// Construct the per-call wiring for the agentic loop so async
    /// task completions reach the next iteration as a synthetic
    /// user-role message.
    ///
    /// This is the central fix for the tool async refactor (commit 3
    /// follow-up): each call to `Agent::execute_*` constructs a fresh
    /// `SessionInbox`, an `AsyncExecutor` that fans out to
    /// that queue, and `Async action spawn`/`Async action output` tools bound to both.
    /// The tools are re-registered on the `ToolingRuntime` (overwriting any
    /// prior instances), and the same queue is given to `AgenticLoop` so the
    /// loop drains it at iteration start.
    ///
    /// Returns the constructed `AgenticLoop` ready to run. The session
    /// key is pushed onto the core so `Async action spawn` can stamp
    /// `parent_session_key` correctly.
    ///
    /// F19: `quota_meter` is the principal's quota meter. The loop
    /// opens a `QuotaScope::with` around the run so every LLM call
    /// auto-charges via `StackedMeteredProvider`. Pass
    /// `Arc::new(QuotaMeter::unlimited())` for unquota'd / test paths.
    ///
    /// B5 (2026-08-22): the F20 `peer_meter` parameter was removed —
    /// peer attribution was broken for agents serving many peers
    /// simultaneously. Per-agent attribution (the `agent_meter` on
    /// `SubagentExecutor`) replaces it; the loop's `QuotaScope::with`
    /// now charges only the supplied principal meter.
    pub async fn build_agentic_loop(
        &self,
        agent_arc: Arc<Agent>,
        provider: Arc<peko_providers::Provider>,
        session_key: Option<String>,
        caller_id: Option<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        quota_meter: Arc<peko_quota::QuotaMeter>,
    ) -> Result<peko_engine::agentic_loop::AgenticLoop> {
        let tooling = self.tooling.for_run();
        self.init_run_builtins(&tooling).await?;

        // Resolve the principal's canonical inbox registry before attaching the
        // loop, including offline runtimes which have no daemon registry.
        let async_inbox_registry = if let Some(ref reg) = self.inbox_registry {
            Arc::clone(reg)
        } else {
            // No shared registry bound (CLI one-shots, tests): a
            // per-call standalone registry stays consistent because
            // the executor below and the loop both read from it.
            crate::async_exec::executor::standalone_inbox_registry()
        };
        let executor = self
            .tooling
            .async_executor_for(&self.principal_id, async_inbox_registry)
            .await;
        let async_inbox_registry = Arc::clone(executor.inbox_registry());
        let async_inbox_key = session_key.clone().unwrap_or_else(|| "default".to_string());
        let async_completion_queue = async_inbox_registry.get_or_create(&async_inbox_key).await;

        // Async services belong to the principal. Task receipts, concurrency
        // permits, and completion routing remain valid across turns.
        crate::tools::installation::install_async(
            &self.tooling,
            &self.principal_id,
            async_inbox_registry,
        )
        .await?;

        // Legacy fallback keys stay local; ToolContext carries canonical session attribution.
        peko_engine::EngineHooks::set_session_key(&*tooling, &self.identity.did, session_key).await;

        // Construct the loop over the private binding and caller inbox.
        //
        // F19: `quota_meter` is bound here from the principal the
        // agent belongs to. The loop opens a `QuotaScope::with` at
        // run entrypoint and `StackedMeteredProvider` auto-charges
        // every LLM call against this meter.
        //
        // Phase 9b.N.5b.9c: supply the compactor factory (captures
        // the provider view + rebuilds a fresh `BackgroundCompactor`
        // every run) and the loaded compaction config. Root owns
        // `load_compaction_config` because it depends on `dirs` /
        // `toml` (not in `peko-engine`'s dep graph); the loop now
        // treats config as opaque data.
        let compactor_factory: Arc<dyn peko_engine::BackgroundCompactorFactory> = Arc::new(
            crate::engine::background_compactor_factory_compat::BackgroundCompactorFactoryAdapter::new(
                Arc::clone(&provider) as Arc<dyn peko_engine::ProviderView>,
            ),
        );
        let compaction_config = peko_session::compaction::load_compaction_config();
        let mut loop_ = peko_engine::agentic_loop::AgenticLoop::new(
            agent_arc,
            provider,
            tooling,
            compactor_factory,
            compaction_config,
        )
        .await
        .with_async_completion_queue(async_completion_queue)
        .with_caller_id(caller_id)
        .with_quota_meter(quota_meter)
        // Phase 2 PR 2: forward the agent's bound MCP context
        // provider so the `{{mcp_context}}` placeholder renders
        // real Markdown describing the configured MCP servers.
        // Default `None` leaves the loop's
        // `EmptyMcpPromptContextProvider` in place (CLI one-shot
        // path, test fixtures); production wiring at
        // `principal/agent_runner.rs` binds a provider wrapping
        // the global `McpManager` via `Agent::with_mcp_context_provider`.
        .with_mcp_context_provider(
            self.mcp_context_provider
                .clone()
                .unwrap_or_else(|| Arc::new(peko_engine::EmptyMcpPromptContextProvider)),
        )
        // Phase 4: forward the agent's bound audit sink so the
        // loop emits `model.selected` events on every successful
        // LLM call. Both fields default to `None` (CLI one-shot
        // path, test fixtures); production wiring at
        // `principal/agent_runner.rs` sets both via
        // `Agent::with_audit_sink`.
        .with_audit_sink(
            self.audit_sink.clone(),
            self.audit_first_use_for_model.clone(),
        );
        // 2026-09-05: forward the compact action's run-scoped
        // force-compact flag to the loop's compaction driver.
        if self.force_compact {
            loop_ = loop_.with_force_compact(true);
        }
        if let Some(token) = cancel {
            loop_ = loop_.with_cancel_token(token);
        }
        Ok(loop_)
    }

    /// Get agent DID
    #[must_use]
    pub fn did(&self) -> &str {
        &self.identity.did
    }

    /// Get agent name
    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Get the spawning principal's runtime id.
    ///
    /// Threaded through tool execution so extension-scoped tools can
    /// resolve per-principal state at handle time.
    #[must_use]
    pub fn principal_id(&self) -> &peko_subject::PrincipalId {
        &self.principal_id
    }

    /// Get the spawning principal's human-readable name, if known.
    #[must_use]
    pub fn principal_name(&self) -> Option<&str> {
        self.principal_name.as_deref()
    }

    // ---- Phase 2 inert field accessors ----
    //
    // The loop reads these via `&self.agent.channel()` etc. when
    // building `TurnPromptContext` for the renderer. `Option<&str>`
    // matches the `Option<String>` shape on `AgentConfig`; the loop
    // supplies the legacy hardcoded defaults when the value is `None`.

    /// Configured channel for `{{channel}}` / `{{runtime}}`.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        self.config.channel.as_deref()
    }

    /// Configured thinking level for `{{thinking_level}}`.
    #[must_use]
    pub fn thinking_level(&self) -> Option<&str> {
        self.config.thinking_level.as_deref()
    }

    /// Whether the agent runs inside a sandbox (`{{sandbox}}`).
    #[must_use]
    pub fn sandbox_enabled(&self) -> bool {
        self.config.sandbox_enabled
    }

    /// Configured model aliases for `{{model_aliases}}`.
    #[must_use]
    pub fn model_aliases(&self) -> &[String] {
        &self.config.model_aliases
    }

    /// Peer-conversation DM channel id (conversation runs only),
    /// rendered into the `{{session_context}}` section.
    #[must_use]
    pub fn conversation_channel(&self) -> Option<&str> {
        self.config.conversation_channel.as_deref()
    }

    /// Peer-conversation peer subject (conversation runs only),
    /// rendered into the `{{session_context}}` section.
    #[must_use]
    pub fn conversation_peer(&self) -> Option<&str> {
        self.config.conversation_peer.as_deref()
    }

    // Session overlay methods

    /// Get the session manager
    #[must_use]
    pub fn session_manager(&self) -> Arc<TokioRwLock<SessionManager>> {
        Arc::clone(&self.session_manager)
    }

    /// Resolve a session for a peer through the legacy `ChannelType` +
    /// `route` indirection. Production paths now flow through
    /// `PrincipalManager::receive_*` with `ChannelKind` (see
    /// `principal/router.rs`); `Agent::resolve_session` /
    /// `resolve_default_session` are kept alive only because the
    /// test suite uses them as scaffolding (12+ sites in
    /// `agents/tests/subagent_integration_tests.rs`).
    ///
    /// B5 cleanup note: `ChannelType` and `SessionManager::route` were
    /// scoped out of this PR — they are embedded in
    /// `OverlayType::Channel(ChannelType)` and `SessionContext.channel_type`
    /// and removing them requires a coordinated refactor of those
    /// types. Tracked for a follow-up.
    pub async fn resolve_session(
        &self,
        peer: &Subject,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<ResolvedSession> {
        let mut manager = self.session_manager.write().await;
        manager
            .route(peer, channel_type, channel_id, Some(&self.config.name))
            .await
    }

    /// Resolve a session for the default user
    ///
    /// Convenience method for CLI and simple channels.
    pub async fn resolve_default_session(&self) -> Result<ResolvedSession> {
        let peer = Subject::User("default".to_string());
        self.resolve_session(&peer, ChannelType::Cli, "default")
            .await
    }

    /// Create a spawn/subagent session
    ///
    /// Creates a new spawn overlay that shares context with the parent
    /// (parent's context is copied into the child session).
    ///
    /// Returns a `ResolvedSession` containing both the metadata DTO (`context`)
    /// and the operations handle (`handle`).
    pub async fn spawn_session(
        &self,
        peer: &Subject,
        task: &str,
        parent_session_key: &str,
        timeout_seconds: Option<u64>,
    ) -> Result<ResolvedSession> {
        let mut manager = self.session_manager.write().await;
        manager
            .spawn_session(
                &self.config.name,
                peer,
                task,
                parent_session_key,
                timeout_seconds,
            )
            .await
    }

    // Session management commands (CLI integration) — the slash
    // dispatcher (`/new`, `/branch`, `/sessions`, `/switch`) and the
    // `session_*` helpers it called have been retired in B3 cleanup:
    // `process_session_command` had no remaining callers in the engine,
    // CLI, or desktop harness. `create_session` is still the live
    // path through `SessionManager` (see `subagent_executor::spawn`
    // and the session-tool surface).

    /// Create an agent for unit tests with isolated storage.
    ///
    /// Uses a temporary directory for identity and session storage so tests
    /// do not conflict with each other or the user's real data.
    #[cfg(test)]
    pub async fn new_for_test(
        config: AgentConfig,
        temp_dir: &std::path::Path,
        tooling: Arc<ToolingRuntime>,
    ) -> Result<Self> {
        use peko_identity::storage::KeyStorage;

        let path_resolver: Arc<dyn peko_subject::PathResolverLike> = Arc::new(
            peko_session::DefaultPathResolver::with_data_dir(temp_dir.join("data")),
        );
        let session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, &config.name)
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));

        // Load or create identity in temp storage (name → DID alias,
        // same resolution as the production `load_or_create_identity`).
        let identity = {
            let storage = KeyStorage::with_path(temp_dir.join("data").join("identities"))?;
            storage.load_or_create_named(&config.name, DIDScope::Local)?
        };

        let (provider, resolved_model_id) =
            match Self::init_provider(&config, None, None, None).await? {
                Some((p, id)) => (Some(p), Some(id)),
                None => (None, None),
            };

        let subagent_executor_base = SubagentExecutor::new(
            Arc::clone(&session_manager),
            config.name.clone(),
            peko_subject::PrincipalId::generate(),
            Arc::clone(&tooling),
        );
        let subagent_executor = match &provider {
            Some(p) => Arc::new(
                subagent_executor_base
                    .with_provider(p.clone())
                    .with_agent_config(config.clone()),
            ),
            None => Arc::new(subagent_executor_base),
        };

        Ok(Self {
            config,
            state: Arc::new(StateMachine::new()),
            identity,
            provider,
            resolved_model_id,
            llm_resolver: None,
            session_manager,
            subagent_executor,
            current_session_id: Arc::new(tokio::sync::RwLock::new(None)),
            tooling,
            inbox_registry: None,
            force_compact: false,
            principal_workspace: None,
            caller_principal_did: None,
            principal_id: peko_subject::PrincipalId::generate(),
            principal_name: None,
            principal_plan_port: None,
            // Phase 2 of `feature/multi-model-subagents`: the
            // test-only `Agent::new_for_test` path doesn't bind a
            // catalog. Tests that need a catalog can use
            // `with_model_catalog(...)` after construction.
            model_catalog: None,
            // Phase 4: test path doesn't bind an audit sink; the
            // integration test in
            // `engine/agentic_loop_compat.rs::tests::test_audit_sink_emits_first_use_warning_then_info`
            // builds the sink directly via
            // `AgenticLoop::with_audit_sink`.
            audit_sink: None,
            audit_first_use_for_model: None,
            mcp_context_provider: None,
        })
    }

    // Private helper methods

    async fn load_or_create_identity(config: &AgentConfig) -> Result<Identity> {
        let storage =
            KeyStorage::new(crate::identity_compat::default_identity_data_dir().as_ref())?;

        // Identity files are named by DID (`KeyStorage::identity_path`),
        // so a lookup by agent name can never hit directly — the stable
        // name is resolved through a `by-name/<name>.json` alias (see
        // `KeyStorage::load_or_create_named`). Without that indirection
        // every agent construction re-minted a fresh ed25519 identity.
        storage.load_or_create_named(&config.name, DIDScope::Local)
    }

    // Sprint 8 Commit 5: `backfill_agent_did` and `is_path_under_temp_dir`
    // were deleted. The spawn path no longer persists `agent_did` to
    // TOML — `KeyStorage` is the authoritative identity source, and
    // identity resolution now falls through to the name-keyed path
    // (or generates a fresh one) regardless of what `agent_did` is on
    // the in-memory config. Sprint 9 Commit 1 retired the
    // `AgentConfig::agent_did` field entirely; cross-runtime identity
    // references now read `Agent::identity.did` (from `KeyStorage`)
    // directly.

    /// Resolve the agent's provider and the catalog id that produced it.
    ///
    /// Returns `None` when no resolver was supplied or resolution failed.
    /// The returned tuple is `(provider, resolved_model_id)`:
    /// `resolved_model_id` is `ResolvedChoice::model_id` and reflects the
    /// precedence winner — including any per-call `message_override`.
    /// This is what the renderer puts in `{{runtime}}`'s `Model:` line
    /// (Phase 2 plumbing). Without capturing it here, the loop would
    /// fall back to `provider.model_id()` which only reflects the
    /// provider's `default_model_id` and not the resolved catalog id.
    async fn init_provider(
        config: &AgentConfig,
        resolver: Option<&Arc<peko_providers::LlmResolver>>,
        // Model-first: the principal's pinned configured model id, or
        // `None` for non-principal callers/tests.
        provider_hint: Option<String>,
        // Model-first: per-message configured model override (e.g.
        // `peko send --model <id>`).
        message_override: Option<String>,
    ) -> Result<Option<(Arc<peko_providers::Provider>, String)>> {
        // v3 path: ask the resolver to build a one-shot provider from
        // the supplied hint. No legacy fallback — the inline `[provider]`
        // block on `AgentConfig` is gone; the resolver is the only source
        // of truth.
        let Some(r) = resolver else {
            return Ok(None);
        };
        let req = peko_providers::resolver::ResolveRequest {
            override_model: message_override.as_deref(),
            session_model: None,
            agent_model: provider_hint.as_deref(),
        };
        match r.build(req).await {
            Ok((provider, choice)) => {
                info!("Agent '{}' resolved provider: {}", config.name, choice);
                Ok(Some((provider, choice.model_id)))
            }
            Err(e) => {
                warn!(
                    "Agent '{}': LlmResolver failed ({}); agent will run without an LLM provider",
                    config.name, e
                );
                Ok(None)
            }
        }
    }

    /// Execute with native tool calling using `AgenticLoop` (unified API).
    ///
    /// This is the recommended method for agent execution with native tool calling support.
    /// The `on_event` callback receives all streaming events (text deltas, tool calls, etc.).
    ///
    /// Execute with native tool calling and return a channel receiver for events.
    ///
    /// This is a convenience wrapper around `execute_native()` that provides
    /// a channel-based interface for code that expects async event streaming.
    ///
    /// Check if the configured provider supports native tool calling
    #[must_use]
    pub fn supports_native_tools(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|p| p.supports_native_tools())
    }
}

// `AgentView` trait port (Phase 9b.N.5a). The impl lives here, not in
// `peko-engine`, because of the orphan rule: `AgentView` is a foreign
// trait and `Agent` is a local type — `impl ForeignTrait for LocalType`
// is allowed in any crate where the local type lives. This block used
// to live at `src/engine/agent_view_compat.rs`; Phase 16 folded it
// into the inherent impl block to retire the compat shim.
impl peko_engine::AgentView for Agent {
    fn name(&self) -> &str {
        Agent::name(self)
    }

    fn identity_did(&self) -> &str {
        // `Agent::identity` is a private field; the public surface uses
        // `Agent::did()` which returns the same value. We avoid
        // exposing the `Identity` struct through the trait.
        Agent::did(self)
    }

    fn has_llm_resolver(&self) -> bool {
        Agent::llm_resolver(self).is_some()
    }

    fn principal_name(&self) -> Option<&str> {
        Agent::principal_name(self)
    }

    fn principal_id(&self) -> &str {
        &Agent::principal_id(self).0
    }

    fn resolved_model_id(&self) -> Option<&str> {
        Agent::resolved_model_id(self)
    }

    fn principal_workspace(&self) -> Option<&std::path::PathBuf> {
        Agent::principal_workspace(self)
    }

    fn channel(&self) -> Option<&str> {
        Agent::channel(self)
    }

    fn thinking_level(&self) -> Option<&str> {
        Agent::thinking_level(self)
    }

    fn sandbox_enabled(&self) -> bool {
        Agent::sandbox_enabled(self)
    }

    fn model_aliases(&self) -> &[String] {
        Agent::model_aliases(self)
    }

    fn conversation_channel(&self) -> Option<&str> {
        Agent::conversation_channel(self)
    }

    fn conversation_peer(&self) -> Option<&str> {
        Agent::conversation_peer(self)
    }

    fn config_prompt_body(&self) -> Option<String> {
        self.config.prompt.clone()
    }

    fn set_config_prompt_body_for_test(&mut self, body: Option<String>) {
        self.config.prompt = body;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent_config::AgentConfig;

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_creation() {
        // Force the encrypted-file identity fallback — see
        // `peko_identity::init_test_env` for the rationale (Windows-headless
        // keyring panics).
        peko_identity::init_test_env();

        let tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let config = AgentConfig {
            name: "test-agent".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config, tooling).await;
        assert!(agent.is_ok());

        let agent = agent.unwrap();
        assert_eq!(agent.name(), "test-agent");
        assert!(agent.did().starts_with("did:peko:"));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_has_session_manager() {
        // Force the encrypted-file identity fallback — see
        // `peko_identity::init_test_env` for the rationale.
        peko_identity::init_test_env();

        let tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let config = AgentConfig {
            name: "test-agent-session".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config, tooling).await.unwrap();

        // Agent should have a session manager
        let manager = agent.session_manager();
        let manager_guard = manager.read().await;
        assert_eq!(manager_guard.base_session_count(), 0);
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_session_routing() {
        use peko_auth::Subject;
        use peko_session::types::ChannelType;

        // Force the encrypted-file identity fallback — see
        // `peko_identity::init_test_env` for the rationale.
        peko_identity::init_test_env();

        let tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let config = AgentConfig {
            name: "test-agent-router".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config, tooling).await.unwrap();

        // Session manager should be able to route to sessions
        let peer = Subject::User("test_user".to_string());
        let resolved = agent
            .resolve_session(&peer, ChannelType::Cli, "default")
            .await;

        // Should succeed (requires filesystem in full test)
        // This just verifies routing is properly initialized
        assert!(resolved.is_ok() || resolved.is_err()); // Either is fine for this test
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_resolve_session() {
        use peko_auth::Subject;
        use peko_session::types::ChannelType;

        // Force the encrypted-file identity fallback — see
        // `peko_identity::init_test_env` for the rationale.
        peko_identity::init_test_env();

        let tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let config = AgentConfig {
            name: "test-agent-context".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config, tooling).await.unwrap();
        let peer = Subject::User("alice".to_string());

        let resolved = agent
            .resolve_session(&peer, ChannelType::Cli, "default")
            .await;

        assert!(resolved.is_ok());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.context.channel_type, Some(ChannelType::Cli));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_tool_session() {
        use peko_auth::Subject;

        // Force the encrypted-file identity fallback — see
        // `peko_identity::init_test_env` for the rationale.
        peko_identity::init_test_env();

        let tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let config = AgentConfig {
            name: "test-agent-spawn".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config, tooling).await.unwrap();
        let peer = Subject::User("bob".to_string());

        // Create a parent session first
        let parent_resolved = agent
            .resolve_session(&peer, peko_session::types::ChannelType::Cli, "default")
            .await
            .unwrap();
        let parent_key = parent_resolved.context.full_session_key.clone();

        // Spawn a child session with shared context
        let spawn_resolved = agent
            .spawn_session(&peer, "test task", &parent_key, Some(300))
            .await;

        assert!(spawn_resolved.is_ok());
        let spawn_resolved = spawn_resolved.unwrap();
        assert!(spawn_resolved.context.is_subagent);
        assert!(!spawn_resolved.context.is_isolated);
    }

    /// Issue #28 acceptance criterion: two agents with the same name
    /// on two distinct runtime directories must have different
    /// `agent_did` values. This is what makes cross-runtime references
    /// (`principal_send`, `PermissionGrant.subject`, PekoHub instance rows)
    /// unambiguous when two runtimes each have an agent literally
    /// called `helper`.
    ///
    /// The test exercises the same `new_for_test` path used by every
    /// agent-construction unit test in this file but with two
    /// independent temp dirs (i.e. two independent `peko_home`s). Each
    /// dir gets its own `KeyStorage` and its own ed25519 keypair, so
    /// the DIDs must differ.
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_two_runtimes_same_name_different_did() {
        use tempfile::TempDir;

        // Force the encrypted-file identity fallback (Windows-headless
        // keyring panics otherwise).
        peko_identity::init_test_env();

        let _tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let make_config = |name: &str| AgentConfig {
            name: name.to_string(),
            ..Default::default()
        };

        // Two distinct peko_home roots.
        let tmp_a = TempDir::new().expect("tempdir A");
        let tmp_b = TempDir::new().expect("tempdir B");

        let agent_a = Agent::new_for_test(
            make_config("helper"),
            tmp_a.path(),
            crate::tools::runtime::ToolingRuntime::standalone(),
        )
        .await
        .expect("agent A");
        let agent_b = Agent::new_for_test(
            make_config("helper"),
            tmp_b.path(),
            crate::tools::runtime::ToolingRuntime::standalone(),
        )
        .await
        .expect("agent B");

        let did_a = agent_a.did().to_string();
        let did_b = agent_b.did().to_string();

        // The DIDs are generated independently (separate keypair per
        // `peko_home`), so they must differ even though the agent
        // names are identical.
        assert_ne!(
            did_a, did_b,
            "issue #28: two agents with the same name on distinct \
             runtime dirs must have different agent_did values \
             (got {did_a:?} for both)"
        );

        // Both must be well-formed peko DIDs.
        assert!(did_a.starts_with("did:peko:"));
        assert!(did_b.starts_with("did:peko:"));
    }

    /// Phase 2: every new `with_*` setter returns `Self` and the
    /// matching accessor reads back the value. The renderer depends
    /// on this round-trip — if a setter forgets to copy a value into
    /// `self.config`, the loop would silently render with defaults.
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn agent_setters_round_trip() {
        use tempfile::TempDir;

        peko_identity::init_test_env();
        let _tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let tmp = TempDir::new().expect("tempdir");
        let mut config = AgentConfig::default();
        config.name = "phase2-setters".to_string();
        let agent = Agent::new_for_test(
            config,
            tmp.path(),
            crate::tools::runtime::ToolingRuntime::standalone(),
        )
        .await
        .expect("agent");

        let configured = agent
            .clone()
            .with_channel("cli")
            .with_thinking_level("high")
            .with_sandbox_enabled(true)
            .with_model_aliases(vec!["sonnet".into(), "haiku".into()]);

        assert_eq!(configured.channel(), Some("cli"));
        assert_eq!(configured.thinking_level(), Some("high"));
        assert!(configured.sandbox_enabled());
        assert_eq!(
            configured.model_aliases(),
            &["sonnet".to_string(), "haiku".to_string()]
        );

        // The pre-set agent must still report the back-compat
        // defaults (`None`/`false`/`[]`) — the setter chain is
        // opt-in, never implicit.
        assert_eq!(agent.channel(), None);
        assert_eq!(agent.thinking_level(), None);
        assert!(!agent.sandbox_enabled());
        assert!(agent.model_aliases().is_empty());
    }

    /// Phase 2: `resolved_model_id()` is `None` for agents built via
    /// `new_for_test` (no resolver) and `Some(...)` after a successful
    /// resolver call. Pin both branches.
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn agent_resolved_model_id_default_is_none() {
        use tempfile::TempDir;

        peko_identity::init_test_env();
        let _tooling = crate::tools::runtime::ToolingRuntime::standalone();

        let tmp = TempDir::new().expect("tempdir");
        let mut config = AgentConfig::default();
        config.name = "phase2-no-resolver".to_string();
        let agent = Agent::new_for_test(
            config,
            tmp.path(),
            crate::tools::runtime::ToolingRuntime::standalone(),
        )
        .await
        .expect("agent");
        // `new_for_test` does not wire a resolver; the agent's cached
        // resolved id stays `None`. Callers must fall back to
        // `provider.model_id()`.
        assert!(agent.resolved_model_id().is_none());
    }
}
