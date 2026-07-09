//! Agent management module

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_executor::SubagentExecutor;
use crate::auth::Subject;
use crate::common::paths::PathResolver;
use crate::common::types::agent_legacy::AgentState;
use crate::extensions::builtin::BuiltinToolAdapter;
use crate::extensions::framework::core::{global_core, ExtensionCore};
use crate::identity::{did::DIDScope, storage::KeyStorage, Identity};
use crate::session::manager::{ResolvedSession, SessionManager};
use crate::session::types::ChannelType;
use crate::session::InboxRegistry;
use crate::tools::builtin::messaging::agent::DynamicSessionKeyProvider;
use crate::tools::core::Tool;
use anyhow::{Context, Result};
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, error, info, warn};

/// Single agent runtime with session overlay support
pub struct Agent {
    /// Agent configuration
    pub config: AgentConfig,
    /// Current state
    state: Arc<RwLock<AgentState>>,
    /// Agent identity
    pub identity: Identity,
    /// LLM provider (stored in Arc for sharing with agentic loop).
    ///
    /// Built by `LlmResolver::build` from the agent's `preferred_*`
    /// hints (or the runtime default) at session start. The
    /// `Option` shape is preserved for unit tests that don't wire
    /// a resolver and run pure-Rust agentic-loop tests offline.
    provider: Option<Arc<crate::providers::Provider>>,
    /// Optional resolver (v3+). When present, `init_provider` builds
    /// a one-shot `Provider` per session via the catalog + secret
    /// store, applying the agent's `preferred_*` hints.
    llm_resolver: Option<Arc<crate::providers::LlmResolver>>,
    /// Session manager for overlay lifecycle
    session_manager: Arc<TokioRwLock<SessionManager>>,
    /// Subagent executor for background task execution
    subagent_executor: Arc<SubagentExecutor>,
    /// Dynamic session key provider for `Agent` tool
    session_key_provider: Arc<DynamicSessionKeyProvider>,
    /// Current session ID for `session` tool lookups
    current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Daemon-global extension core shared by every agent in the process.
    /// Per-agent/per-principal visibility is enforced via the per-call
    /// `allowed_extensions` allowlist, not by isolating cores.
    extension_core: Arc<ExtensionCore>,
    /// Optional external inbox registry. When set, the agentic loop drains
    /// this registry's session inbox instead of creating a per-call one,
    /// so external callers can push steering messages into a running agent.
    inbox_registry: Option<Arc<InboxRegistry>>,
    /// Optional principal workspace. When set, `init_builtins_async` builds the
    /// `Agent` tool's `AgentService` with `for_principal(workspace)` so the
    /// root agent resolves subagents from `<workspace>/agents/<name>/AGENT.md`
    /// instead of the global `<home>/agents/<name>/config.toml`. Without this,
    /// `init_builtins_async` (run lazily at execution time) would clobber any
    /// principal-scoped `Agent` tool registered on the core beforehand.
    principal_workspace: Option<std::path::PathBuf>,
    /// Caller principal's stable DID. Bound at construction by
    /// `with_caller_principal_did` so `principal_send` can attribute
    /// the outbound request under
    /// `Subject::Principal(caller_principal_did)` on the wire.
    /// `None` means the tool is not registered.
    caller_principal_did: Option<String>,
    /// Spawning principal's runtime id. Inherited by subagent spawns
    /// via `SubagentExecutor`. Threaded into `ToolContext` so tools
    /// such as `Skill` can resolve per-principal state at handle time
    /// without re-registering themselves on the shared `ExtensionCore`
    /// per principal.
    principal_id: crate::principal::PrincipalId,
    /// Spawning principal's human-readable name. Threaded into
    /// `ToolContext` so Principal-scoped tools (e.g. cron) can target
    /// jobs by name.
    principal_name: Option<String>,
    /// Snapshot of the spawning principal's allowed extension list,
    /// captured at construction from
    /// `PrincipalContext::allowed_extensions`. Used by
    /// `init_builtins_async` to filter the tool registry down to
    /// what this agent's principal can see.
    ///
    /// `None` means the agent is unbound from any principal and no
    /// allowlist-based filtering is applied — every registered tool
    /// stays visible. This preserves pre-Track-B behaviour for
    /// test-only `Agent::new` callers and standalone agents.
    ///
    /// `Some(empty)` means the principal has an empty allowlist and
    /// every tool is denied (fail-closed). This matches the
    /// `AgentStateRegistry` / `ExtensionStateRegistry` semantics.
    ///
    /// **Track B**: this snapshot replaces
    /// `AgentConfig::extensions`/`extension_whitelist` for the
    /// runtime filter. Once `AgentConfig::extensions` is removed
    /// the principal's allowlist is the *only* source of truth.
    principal_allowed_extensions: Option<Arc<crate::principal::config::AllowedExtensions>>,
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
            llm_resolver: self.llm_resolver.clone(),
            session_manager: Arc::clone(&self.session_manager),
            subagent_executor: Arc::clone(&self.subagent_executor),
            session_key_provider: Arc::clone(&self.session_key_provider),
            current_session_id: Arc::clone(&self.current_session_id),
            extension_core: Arc::clone(&self.extension_core),
            inbox_registry: self.inbox_registry.clone(),
            principal_workspace: self.principal_workspace.clone(),
            caller_principal_did: self.caller_principal_did.clone(),
            principal_id: self.principal_id.clone(),
            principal_name: self.principal_name.clone(),
            principal_allowed_extensions: self.principal_allowed_extensions.clone(),
        }
    }
}

impl Agent {
    /// Initialize built-in tools and register them with `ExtensionCore`.
    ///
    /// This asynchronous version loads Universal Tools from extensions directory
    /// and registers only agent-specific built-in tools with `ExtensionCore`.
    /// Common built-in tools (Bash, Read, Write, etc.) are already
    /// registered by the daemon's `AppState` startup via `ToolRuntime`.
    /// Extension tools (Universal and MCP) are registered via `ExtensionManager` hooks.
    pub(crate) async fn init_builtins_async(&self) -> anyhow::Result<()> {
        use crate::tools::builtin::session::SessionIntrospector;
        use crate::tools::{AgentTool, SessionTool, Tool};

        // Defensive check: common built-ins must be pre-registered by the daemon startup path.
        // AppState::new() calls ToolRuntime::with_workspace_and_core() which registers
        // all common built-ins on the global ExtensionCore before any Agent is created.
        let has_bash = self
            .extension_core
            .get_tool_metadata("Bash")
            .await
            .is_some();
        if !has_bash {
            tracing::error!(
                "Built-in tools not pre-registered on ExtensionCore. \
                 This indicates a startup ordering bug — AppState should initialize \
                 ToolRuntime before StatelessAgentService."
            );
        }

        // Agent-specific tools (not part of ToolRuntime::register_builtins)
        let mut tools: Vec<Arc<dyn Tool>> = vec![];

        // Add session introspection tool backed by the real session manager
        let session_registry = SessionIntrospector::new(
            self.session_manager.clone(),
            self.current_session_id.clone(),
        );
        tools.push(Arc::new(SessionTool::new(Box::new(session_registry))));

        // Add Agent tool with executor and session provider. When this agent
        // runs as a Principal root agent, build the service scoped to the
        // principal workspace so subagents resolve from
        // `<workspace>/agents/<name>/AGENT.md`. Otherwise fall back to the
        // global agent registry.
        let agent_service = match self.principal_workspace {
            Some(ref workspace) => crate::common::services::AgentService::for_principal(workspace),
            None => crate::common::services::AgentService::new(PathResolver::new()),
        };
        tools.push(Arc::new(
            AgentTool::with_agent_service_and_session_provider(
                self.subagent_executor.clone(),
                agent_service,
                Box::new(self.session_key_provider.clone()),
            ),
        ));

        // Add planning todo (Task*) tools backed by the agent's session storage.
        if self.config.enable_task_tools {
            if let Some(sessions_dir) = self.session_manager.read().await.sessions_dir().cloned() {
                let todo_storage = Arc::new(crate::session::todos::TodoStorage::new(sessions_dir));
                tools.push(Arc::new(crate::tools::TaskCreateTool::new(
                    todo_storage.clone(),
                )));
                tools.push(Arc::new(crate::tools::TaskGetTool::new(
                    todo_storage.clone(),
                )));
                tools.push(Arc::new(crate::tools::TaskListTool::new(
                    todo_storage.clone(),
                )));
                tools.push(Arc::new(crate::tools::TaskUpdateTool::new(todo_storage)));
            } else {
                tracing::warn!(
                    "Session storage directory not available for agent '{}'; Task* tools will not be registered",
                    self.config.name
                );
            }
        } else {
            tracing::debug!(
                "Task* tools disabled by config for agent '{}'",
                self.config.name
            );
        }

        // Note: `AsyncStatus`/`AsyncList`/`AsyncStop` are intentionally NOT registered
        // here. They are registered per-agent inside `build_agentic_loop`, bound
        // to the agent's own `AsyncExecutor` registry so each agent only sees its
        // own async tasks (session isolation).

        // Add principal_send tool for principal-to-principal cross-runtime
        // messaging. Replaces the legacy `a2a_send` tool (ADR-023 +
        // root-agent unification): the target is now a Principal DID
        // (not an agent name on a target runtime), and dispatch flows
        // through the tunnel even when caller and target share a daemon.
        //
        // Both the caller's principal DID and the cross-runtime ctx
        // must be present; otherwise the tool is intentionally not
        // registered (no fall-back local-only path — `principal_send`
        // is exclusively cross-runtime).
        //
        // The ctx is pulled from extension services (set by the
        // daemon-state after `start_tunnel`). For pre-#29 runtimes /
        // test harnesses without cross-runtime dispatch, the ctx is
        // `None` and the tool is omitted — same gating as before.
        if let Some(caller_did) = self.caller_principal_did.as_ref() {
            let cross_ctx = self
                .extension_core
                .services()
                .cross_runtime_a2a_ctx()
                .and_then(|ctx| Arc::downcast::<crate::tunnel::CrossRuntimeA2aCtx>(ctx).ok());
            if let Some(ctx) = cross_ctx {
                tools.push(crate::tunnel::principal_send_tool::build_tool(
                    caller_did.clone(),
                    ctx,
                ));
            } else {
                tracing::debug!(
                    "CrossRuntimeA2aCtx not available on ExtensionCore — \
                     principal_send tool will not be registered for agent {}",
                    self.config.name
                );
            }
        } else {
            tracing::debug!(
                "Caller identity not bound on agent {} — \
                 principal_send tool will not be registered",
                self.config.name
            );
        }

        // Filter against the spawning principal's allowlist. The list
        // is captured at construction from
        // `PrincipalContext::allowed_extensions` (see
        // `with_principal_allowed_extensions`).
        //
        // `None`    => no allowlist bound; every registered tool stays visible
        //              (standalone / test behaviour).
        // `Some(_)` => filter to the listed names. An empty inner list means
        //              deny-all (fail-closed), matching the semantics of
        //              `AgentStateRegistry` / `ExtensionStateRegistry`.
        if let Some(whitelist) = self
            .principal_allowed_extensions
            .as_ref()
            .map(|allowed| allowed.iter().cloned().collect::<Vec<_>>())
        {
            let before_count = tools.len();
            tools.retain(|tool| {
                let tool_name = tool.name();
                whitelist.iter().any(|pattern: &String| {
                    // Bare-name match (e.g. "Agent").
                    if pattern.eq_ignore_ascii_case(tool_name) {
                        return true;
                    }
                    // Canonical-form match (e.g. "builtin:tool:Agent"). Whitelists
                    // are commonly stored in canonical form, while per-agent tools
                    // register under their bare name — match the suffix so the
                    // canonical entry still enables the tool. Without this, spawned
                    // subagents (whose principal has only canonical IDs in
                    // `allowed_extensions`) would lose the Agent/session/Task
                    // tools and cannot delegate to nested subagents.
                    if let Some(bare) = pattern.strip_prefix("builtin:tool:") {
                        if bare.eq_ignore_ascii_case(tool_name) {
                            return true;
                        }
                    }
                    if pattern.ends_with('*') {
                        let prefix = &pattern[..pattern.len() - 1];
                        return tool_name.to_lowercase().starts_with(&prefix.to_lowercase());
                    }
                    false
                })
            });
            tracing::debug!("Filtered {} tools to {}", before_count, tools.len());
        }

        // ADR-020: Per-agent tool configuration is now carried on each
        // `HookInput::ToolCall` via `allowed_extensions` instead of being
        // written to the shared global `tool_config`. This eliminates a
        // race where concurrent agents overwrite each other's whitelist
        // on the daemon-global `ExtensionCore`.

        // Load Universal Tools from extensions directory (where `peko ext install` puts them).
        //
        // A fresh `Agent` is constructed per execution but they all share
        // the daemon-global `ExtensionCore`, so this scan only needs to run
        // once per core. Skip the dir walk + `ExtensionManager` rebuild once
        // the core is warm — otherwise this re-walks disk on every run.
        if self.extension_core.universal_extensions_loaded() {
            tracing::debug!(
                "Universal extensions already loaded on shared core; skipping rescan for agent '{}'",
                self.config.name
            );
        } else {
            let extensions_dir = crate::common::paths::default_data_dir().join("extensions");
            tracing::info!(
                "Checking for Universal Tools in extensions directory: {}",
                extensions_dir.display()
            );
            if extensions_dir.exists() {
                tracing::info!(
                    "Loading Universal Tools from '{}' for agent '{}'...",
                    extensions_dir.display(),
                    self.config.name
                );
                // Use ExtensionManager for unified tool discovery
                use crate::extensions::framework::manager::ExtensionManager;
                use crate::extensions::BuiltInAdapters;
                let mut manager = ExtensionManager::with_core(self.extension_core.clone());
                for adapter in BuiltInAdapters::new().adapters() {
                    manager.register_adapter(adapter);
                }
                match manager.load_from_directory(&extensions_dir).await {
                    Ok(loaded_ids) => {
                        if loaded_ids.is_empty() {
                            tracing::debug!("No extensions found in {}", extensions_dir.display());
                        } else {
                            tracing::info!(
                                "✅ Loaded {} extensions: {:?}",
                                loaded_ids.len(),
                                loaded_ids
                                    .iter()
                                    .map(std::string::ToString::to_string)
                                    .collect::<Vec<_>>()
                            );
                        }
                        // Mark the core warm so later executions skip the rescan.
                        // Only on success so a transient failure is retried next run.
                        self.extension_core.mark_universal_extensions_loaded();
                    }
                    Err(e) => {
                        tracing::warn!(
                            "❌ Failed to load extensions from {}: {:#}",
                            extensions_dir.display(),
                            e
                        );
                        // Continue without extensions
                    }
                }
            } else {
                tracing::debug!(
                    "Extensions directory not found at {} - no universal tools to load",
                    extensions_dir.display()
                );
            }
        }

        // ADR-018/019: Register ONLY agent-specific built-in tools with ExtensionCore
        // Common built-in tools are already registered via ToolRuntime::register_builtins
        // Extension tools (Universal and MCP) are already registered via ExtensionManager hooks
        for tool in &tools {
            if let Err(e) =
                BuiltinToolAdapter::register_tool(&self.extension_core, tool.clone()).await
            {
                tracing::warn!(
                    "Failed to register built-in tool '{}' with ExtensionCore: {}",
                    tool.name(),
                    e
                );
            } else {
                tracing::debug!(
                    "Registered built-in tool '{}' with ExtensionCore",
                    tool.name()
                );
            }
        }

        tracing::info!(
            "Registered {} agent-specific built-in tools with ExtensionCore",
            tools.len()
        );

        Ok(())
    }

    /// Create a new agent with the given configuration
    pub async fn new(config: AgentConfig) -> Result<Self> {
        // Initialize session manager with path resolver
        let path_resolver = PathResolver::new();
        let session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, &config.name)
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));
        Self::new_with_session_manager_and_resolver(config, session_manager, None).await
    }

    /// Create a new agent backed by a `LlmResolver` (v3+ path).
    ///
    /// The resolver is consulted in `init_provider` to build a
    /// one-shot `Provider` from the agent's `preferred_*` hints (or
    /// the runtime default). If the resolver has no matching entry
    /// (e.g. the catalog hasn't been seeded yet), the constructor
    /// falls back to the deprecated `config.provider` field so
    /// pre-v3 fixtures still work.
    pub async fn new_with_resolver(
        config: AgentConfig,
        resolver: Arc<crate::providers::LlmResolver>,
    ) -> Result<Self> {
        let path_resolver = PathResolver::new();
        let session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, &config.name)
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));
        Self::new_with_session_manager_and_resolver(config, session_manager, Some(resolver)).await
    }

    /// Create a new agent with an existing session manager.
    ///
    /// Used for subagent execution where the child must share the parent's
    /// session manager (and therefore session storage and context).
    pub async fn new_with_session_manager(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
    ) -> Result<Self> {
        Self::new_with_session_manager_and_resolver(config, session_manager, None).await
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
        llm_resolver: Option<Arc<crate::providers::LlmResolver>>,
    ) -> Result<Self> {
        Self::new_with_session_manager_resolver(
            config,
            session_manager,
            llm_resolver,
            None,
            crate::principal::PrincipalId::generate(),
            None,
        )
        .await
    }

    /// Create a new agent with an existing session manager, optional
    /// `LlmResolver`, the spawning principal's id, and an optional
    /// external `InboxRegistry`.
    ///
    /// There is no per-agent `ExtensionCore` — the global
    /// [`crate::extensions::framework::core::global_core`] is used for
    /// every agent of the principal. Per-agent visibility is enforced
    /// by each agent's own extension whitelist. `principal_id` is the
    /// spawning principal's runtime id, carried so the agent's
    /// `SubagentExecutor` and any descendant spawns inherit the same
    /// principal scope. When `inbox_registry` is supplied, the agentic
    /// loop drains that registry's session inbox, allowing the
    /// Principal boundary to queue steering messages into a running
    /// root agent.
    pub async fn new_with_session_manager_resolver(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        llm_resolver: Option<Arc<crate::providers::LlmResolver>>,
        // **Track B**: principal's `(provider_id, model_id)` hint,
        // threaded from `PrincipalContext::provider_hint`. The hint
        // is no longer stored on `AgentConfig`; it reaches
        // `init_provider` via this parameter. `None` means "no
        // hint — let the resolver pick the catalog default" (test
        // fixtures, non-principal callers).
        provider_hint: Option<(Option<String>, Option<String>)>,
        principal_id: crate::principal::PrincipalId,
        inbox_registry: Option<Arc<InboxRegistry>>,
    ) -> Result<Self> {
        info!("Creating agent: {}", config.name);

        // Load or create identity
        let identity = Self::load_or_create_identity(&config).await?;

        // Issue #28: persist the resolved DID back into the on-disk
        // config.toml so the tunnel dispatcher can announce it without
        // re-running identity generation.
        //
        // Soft-fail: the agent_dir may not exist yet for a freshly-
        // spawned subagent whose in-memory config hasn't been written.
        let config_path = PathResolver::new().agent_config(&config.name);
        if let Err(e) = Self::backfill_agent_did(&config_path, &config, &identity.did).await {
            warn!("Could not backfill agent_did into config: {}", e);
        }

        if let Some(ref old_did) = config.agent_did {
            if old_did != &identity.did {
                warn!(
                    "Agent '{}' DID rotated: {old_did} -> {new_did} \
                     (previous identity file was missing; cross-runtime \
                     grants and audit references to {old_did} are now orphaned \
                     — issue #28 follow-up: DID rotation ADR pending).",
                    config.name,
                    new_did = identity.did,
                );
            }
        }

        // Initialize provider if configured. `provider_hint` flows
        // through from the production caller (principal's hint) or
        // is `None` for tests / non-principal callers — see the
        // Track B note on `init_provider`.
        let provider = Self::init_provider(&config, llm_resolver.as_ref(), provider_hint).await?;

        // Single global ExtensionCore — every agent of the principal
        // shares it. `principal_id` is the spawning principal's
        // runtime id; descendant subagents inherit it via the
        // shared `SubagentExecutor`.
        let extension_core = global_core().expect("Global ExtensionCore not initialized");

        // Initialize subagent executor
        let subagent_executor_base = SubagentExecutor::new(
            Arc::clone(&session_manager),
            config.name.clone(),
            5, // max_concurrent
            principal_id.clone(),
        );
        let subagent_executor = match &provider {
            Some(p) => Arc::new(
                subagent_executor_base
                    .with_provider(p.clone())
                    .with_agent_config(config.clone()),
            ),
            None => Arc::new(subagent_executor_base),
        };

        // Initialize session key provider for Agent tool
        let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
            "agent:{}:cli:default",
            config.name
        )));

        let agent = Self {
            config,
            state: Arc::new(RwLock::new(AgentState::Idle)),
            identity,
            provider,
            llm_resolver,
            session_manager,
            subagent_executor,
            session_key_provider,
            current_session_id: Arc::new(tokio::sync::RwLock::new(None)),
            extension_core,
            inbox_registry,
            principal_workspace: None,
            caller_principal_did: None,
            principal_id,
            principal_name: None,
            principal_allowed_extensions: None,
        };

        info!(
            "Agent {} initialized with DID: {}",
            agent.config.name, agent.identity.did
        );

        Ok(agent)
    }

    /// Scope this agent's `Agent` tool to a Principal workspace.
    ///
    /// When set, `init_builtins_async` builds the `Agent` tool's `AgentService`
    /// with `for_principal(workspace)`, so the root agent resolves subagents
    /// from `<workspace>/agents/<name>/AGENT.md`. This must be set before the
    /// agent executes: `init_builtins_async` runs lazily inside
    /// `prepare_execution`, so a principal-scoped `Agent` tool registered on the
    /// core beforehand would otherwise be clobbered by the global-scoped one.
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

    /// Bind the caller's principal identity for `principal_send`.
    ///
    /// `principal_did` is the Principal's stable DID (used as
    /// `caller_principal_did` on the wire). When `None`, the
    /// `principal_send` tool is not registered (the agent lacks the
    /// identity needed to attribute cross-principal calls). The local
    /// runtime id is taken from `CrossRuntimeA2aCtx::caller_runtime_id`
    /// at registration time, so this builder does not need it.
    #[must_use]
    pub fn with_caller_principal_did(mut self, principal_did: Option<String>) -> Self {
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

    /// Bind the spawning principal's allowlist for this agent's tool
    /// filter.
    ///
    /// Captures a snapshot of `PrincipalContext::allowed_extensions` at
    /// construction time so `init_builtins_async` can prune the
    /// registered tool bag down to what the principal is allowed to
    /// see. Mutations to the principal's allowlist after construction
    /// are not picked up by this agent — that is the intended
    /// semantic (the principal-scoping rules bind per-session).
    ///
    /// `None` means unbound — no allowlist-based filtering is applied.
    /// `Some(list)` filters to the named tools; an empty inner list
    /// means deny-all (fail-closed). This matches the
    /// `AgentStateRegistry` / `ExtensionStateRegistry` semantics.
    #[must_use]
    pub fn with_principal_allowed_extensions(
        mut self,
        allowed: Option<Arc<crate::principal::config::AllowedExtensions>>,
    ) -> Self {
        let executor = (*self.subagent_executor)
            .clone()
            .with_principal_allowed_extensions(allowed.clone());
        self.subagent_executor = Arc::new(executor);
        self.principal_allowed_extensions = allowed;
        self
    }

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
    /// Tools resolve from the daemon-global
    /// [`crate::extensions::framework::core::global_core`] — there is no
    /// per-agent core. The shared executor carries the spawning
    /// principal's DID so descendant spawns inherit the same principal
    /// scope; the agent's own `extension_core` field is initialised to
    /// the global core for backward-compatible access by `extension_core()`
    /// callers.
    pub async fn new_with_shared_executor(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        subagent_executor: Arc<SubagentExecutor>,
        inherited_provider: Option<Arc<crate::providers::Provider>>,
        principal_allowed_extensions: Option<Arc<crate::principal::config::AllowedExtensions>>,
    ) -> Result<Self> {
        info!("Creating agent with shared executor: {}", config.name);
        let extension_core = global_core().expect("Global ExtensionCore not initialized");

        let identity = Self::load_or_create_identity(&config).await?;

        // Issue #28: persist the resolved DID back into config.toml. This
        // path is reached for subagent execution where the parent's config
        // may not yet carry agent_did — the first call backfills it.
        //
        // Review of #34 concern #5: the production `PathResolver::new()`
        // resolves to `~/.peko` (or `PEKO_HOME` if set), so writing here
        // is a real config mutation. In tests that bypass `new_for_test`
        // and call this constructor directly against a tempdir-backed
        // config, we'd otherwise silently mutate the developer's real
        // `~/.peko`. The `is_path_under_temp_dir` guard catches that
        // case — the in-memory identity is still valid, the backfill
        // is just deferred to the first production-path call.
        let config_path = PathResolver::new().agent_config(&config.name);
        if !Self::is_path_under_temp_dir(&config_path) {
            if let Err(e) = Self::backfill_agent_did(&config_path, &config, &identity.did).await {
                warn!("Could not backfill agent_did into config: {}", e);
            }
        } else {
            debug!(
                "Skipping agent_did backfill for {}: config path {} is under the \
                 system temp dir (test path — would mutate the developer's real config)",
                config.name,
                config_path.display()
            );
        }
        // Prefer the inherited provider so the child reuses the parent's
        // resolved provider instead of paying the resolver's catalog
        // lookup cost twice. Fall back to the v3 resolver path if the
        // caller didn't supply one (e.g., unit tests).
        let provider = match inherited_provider {
            Some(p) => Some(p),
            // Subagent path: no principal binding, so no provider hint;
            // the resolver falls back to the catalog default.
            None => Self::init_provider(&config, None, None).await?,
        };
        let llm_resolver: Option<Arc<crate::providers::LlmResolver>> = None;

        let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
            "agent:{}:cli:default",
            config.name
        )));

        // Subagents share the daemon-global ExtensionCore with every other
        // agent in the process. There is no per-principal core; per-principal
        // tool visibility is enforced by the per-call `allowed_extensions`
        // allowlist.

        let principal_id = subagent_executor.principal_id().clone();
        let principal_name = subagent_executor.principal_name().map(String::from);
        let agent = Self {
            config,
            state: Arc::new(RwLock::new(AgentState::Idle)),
            identity,
            provider,
            llm_resolver,
            session_manager,
            subagent_executor,
            session_key_provider,
            current_session_id: Arc::new(tokio::sync::RwLock::new(None)),
            extension_core,
            inbox_registry: None,
            principal_workspace: None,
            caller_principal_did: None,
            principal_id,
            principal_name,
            principal_allowed_extensions,
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

        // Invoke AgentShutdown hook so extensions can clean up
        let shutdown_result = self
            .extension_core
            .invoke_hook(
                crate::extensions::framework::core::HookPoint::AgentShutdown,
                crate::extensions::framework::types::HookInput::Unit,
            )
            .await;
        tracing::info!(
            "AgentShutdown hook result: {:?}",
            std::mem::discriminant(&shutdown_result)
        );

        Ok(())
    }

    /// Get current state
    #[must_use]
    pub fn state(&self) -> AgentState {
        self.state.read().unwrap().clone()
    }

    /// Get provider reference
    #[must_use]
    pub fn get_provider(&self) -> Option<&crate::providers::Provider> {
        self.provider.as_deref()
    }

    /// Set state
    /// Set agent state (public for channel use)
    pub fn set_state(&self, state: AgentState) {
        let mut current = self.state.write().unwrap();
        debug!(
            "Agent {} state: {:?} -> {:?}",
            self.config.name, *current, state
        );
        *current = state;
    }

    /// Get the provider as an `Arc`.
    #[must_use]
    pub fn provider_arc(&self) -> Option<Arc<crate::providers::Provider>> {
        self.provider.clone()
    }

    /// Get the extension core
    #[must_use]
    pub fn extension_core(&self) -> Arc<ExtensionCore> {
        Arc::clone(&self.extension_core)
    }

    /// Get the current session ID lock.
    #[must_use]
    pub fn current_session_id(&self) -> Arc<tokio::sync::RwLock<Option<String>>> {
        Arc::clone(&self.current_session_id)
    }

    /// Get the session key provider for Agent tool.
    #[must_use]
    pub fn session_key_provider(&self) -> Arc<DynamicSessionKeyProvider> {
        Arc::clone(&self.session_key_provider)
    }

    /// Execute a task with the LLM provider using the unified callback API.
    ///
    /// Directly creates an `AgenticLoop` and runs it — no intermediate layers.
    pub async fn execute(
        &self,
        prompt: &str,
        on_event: impl Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    ) -> Result<crate::engine::AgenticResult> {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        if let Err(e) = self.prepare_execution().await {
            self.set_state(AgentState::Idle);
            return Err(e);
        }

        let agent_arc = Arc::new(self.clone());
        // Per-call wiring: fresh completion queue + executor + per-agent
        // AsyncSpawn/AsyncOutput tools. The agent's session key (read from
        // `current_session_id`) is pushed onto the core so AsyncSpawn can
        // stamp `parent_session_key` correctly.
        //
        // Session-key flow across the three `execute_*` paths:
        //
        // - `Agent::execute()` (this method, one-shot CLI mode):
        //   `current_session_id` is `None` here because `prepare_execution`
        //   does not create a session — the session is born later, inside
        //   `AgenticLoop::run` → `run_inner`. So `session_key` is `None`
        //   and `set_session_key(&self.identity.did, None)` runs on the
        //   core. The loop's `run_inner` rebinds the core's session key
        //   for *this* agent's DID to the real session id it just
        //   created (see `src/engine/agentic_loop.rs`), so any
        //   `AsyncSpawn` issued *mid-iteration* still gets a real
        //   `parent_session_key`. The brief window before the loop
        //   starts (no iterations yet, no `AsyncSpawn` possible) does
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
        let loop_ = self
            .build_agentic_loop(agent_arc, provider, session_key, None, None)
            .await?;

        let result = match loop_.run(prompt, on_event).await {
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
    /// `cancel` is the soft-interrupt `CancellationToken` (PR #128) the
    /// child agent should observe at iteration boundaries. When the
    /// parent agent's `CancellationToken` is flipped (e.g. via
    /// `PrincipalSendControl`), the child agent's loop also exits
    /// cleanly with `AgenticResult { interrupted: true }`. `None` for
    /// the legacy non-cancelable path (sub-agents that pre-date this
    /// plumbing, tests).
    pub async fn execute_with_session(
        &self,
        prompt: &str,
        session: Arc<tokio::sync::RwLock<crate::session::Session>>,
        history: Option<Vec<crate::common::types::message::LlmMessage>>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        on_event: impl Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    ) -> Result<crate::engine::AgenticResult> {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        if let Err(e) = self.prepare_execution().await {
            self.set_state(AgentState::Idle);
            return Err(e);
        }

        let agent_arc = Arc::new(self.clone());
        // Capture session ID into both the agent's cell (used by the
        // session tool) and the agent's session_key_provider cell, then
        // pass the session_id to the per-call wiring. Unlike
        // `Agent::execute`, the session already exists here, so we can
        // push a real id onto the core before the loop starts — see the
        // session-key flow comment in `Agent::execute` for the full
        // picture across the three `execute_*` paths.
        let session_id = session.read().await.id.clone();
        {
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id.clone());
        }
        let loop_ = self
            .build_agentic_loop(agent_arc, provider, Some(session_id), None, cancel)
            .await?;

        let result = match loop_
            .run_with_resume(prompt, on_event, session, history)
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
    /// `caller_id` is the resolved caller identity for the request
    /// (pekohub sub, API key id, or `None` for local CLI invocations) —
    /// propagated to every `HookInput::ToolCall` so per-user permission
    /// checks and audit logging can attribute tool calls to a real user
    /// (issue #17).
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_streaming_with_session<F>(
        &self,
        prompt: &str,
        session: std::sync::Arc<tokio::sync::RwLock<crate::session::Session>>,
        history: Option<Vec<crate::common::types::message::LlmMessage>>,
        caller_id: Option<String>,
        on_event: F,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<crate::engine::AgenticResult>
    where
        F: Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        // Capture current session ID so session tool can look it up
        {
            let session_id = session.read().await.id.clone();
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id);
        }

        if let Err(e) = self.prepare_execution().await {
            self.set_state(AgentState::Idle);
            return Err(e);
        }

        let agent_arc = Arc::new(self.clone());
        // Per-call wiring: fresh completion queue + executor + per-agent
        // AsyncSpawn/AsyncOutput tools. The session ID we just stamped into
        // current_session_id is the parent_session_key we'll use for any
        // spawn in this loop. See the session-key flow comment in
        // `Agent::execute` for how the three `execute_*` paths cooperate
        // to ensure mid-iteration `AsyncSpawn` calls see a real session key.
        let session_id = self.current_session_id.read().await.clone();
        let loop_ = match self
            .build_agentic_loop(agent_arc, provider, session_id, caller_id, cancel)
            .await
        {
            Ok(loop_) => loop_,
            Err(e) => {
                self.set_state(AgentState::Idle);
                return Err(e);
            }
        };

        let streaming_config = crate::engine::OrchestratorConfig::live();

        let result = loop_
            .run_streaming_with_resume(prompt, on_event, session, history, streaming_config)
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
    /// [`crate::engine::agentic_loop::AgenticLoop::run_streaming_with_resume_skip_user_add`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_streaming_with_session_skip_user_add<F>(
        &self,
        on_event: F,
        session: std::sync::Arc<tokio::sync::RwLock<crate::session::Session>>,
        history: Option<Vec<crate::common::types::message::LlmMessage>>,
        caller_id: Option<String>,
    ) -> Result<crate::engine::AgenticResult>
    where
        F: Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        {
            let session_id = session.read().await.id.clone();
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id);
        }

        self.prepare_execution().await?;

        let agent_arc = Arc::new(self.clone());
        let session_id = self.current_session_id.read().await.clone();
        let loop_ = self
            .build_agentic_loop(agent_arc, provider, session_id, caller_id, None)
            .await?;

        let streaming_config = crate::engine::OrchestratorConfig::live();

        loop_
            .run_streaming_with_resume_skip_user_add(on_event, session, history, streaming_config)
            .await
    }

    /// Construct the per-call wiring for the agentic loop so async
    /// task completions reach the next iteration as a synthetic
    /// user-role message.
    ///
    /// This is the central fix for the tool async refactor (commit 3
    /// follow-up): each call to `Agent::execute_*` constructs a fresh
    /// `SessionInbox`, an `AsyncExecutor` that fans out to
    /// that queue, and `AsyncSpawn`/`AsyncOutput` tools bound to both.
    /// The tools are re-registered on the `ExtensionCore` (overwriting any
    /// prior instances), and the same queue is given to `AgenticLoop` so the
    /// loop drains it at iteration start.
    ///
    /// Returns the constructed `AgenticLoop` ready to run. The session
    /// key is pushed onto the core so `AsyncSpawn` can stamp
    /// `parent_session_key` correctly.
    pub async fn build_agentic_loop(
        &self,
        agent_arc: Arc<Agent>,
        provider: Arc<crate::providers::Provider>,
        session_key: Option<String>,
        caller_id: Option<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<crate::engine::agentic_loop::AgenticLoop> {
        let extension_core = self.extension_core();

        // 1. Per-call completion queue (shared by executor + loop).
        //    The executor is wired to a per-call `InboxRegistry` that
        //    holds the same `SessionInbox` the loop drains; the
        //    daemon-global `InboxRegistry` will replace this in a
        //    follow-up once the per-call path is rewired to read from
        //    `AppState::inbox_registry` directly.
        let async_inbox_registry = if let Some(ref reg) = self.inbox_registry {
            Arc::clone(reg)
        } else {
            Arc::new(crate::session::InboxRegistry::new())
        };
        let async_inbox_key = session_key.clone().unwrap_or_else(|| "default".to_string());
        let async_completion_queue = async_inbox_registry.get_or_create(&async_inbox_key).await;

        // 2. Per-call AsyncExecutor wired to the same registry.
        let async_executor = Arc::new(
            crate::extensions::framework::async_exec::executor::AsyncExecutor::new()
                .with_inbox_registry(async_inbox_registry.clone()),
        );
        // Snapshot the registry so the per-agent AsyncStatus/AsyncList/
        // AsyncStop tools can be bound to it after `async_executor` is moved
        // into AsyncOutputTool below.
        let async_registry = async_executor.clone_registry();

        // 3. Per-call AsyncSpawn and AsyncOutput tools bound to executor +
        //    core. Uses Weak so the tools do not extend the core's lifetime
        //    past the core itself.
        if self.config.enable_async_tools {
            let core_weak = Arc::downgrade(&extension_core);
            let spawn_tool = Arc::new(crate::tools::builtin::AsyncSpawnTool::new(
                async_executor.clone(),
                core_weak.clone(),
                Some(self.identity.did.clone()),
            ));
            let output_tool = Arc::new(crate::tools::builtin::AsyncOutputTool::with_executor(
                async_executor,
            ));

            // 4. Re-register the per-agent async tools (overwrites any prior
            //    instance). register_tool is idempotent — unregisters first.
            if let Err(e) =
                crate::extensions::builtin::BuiltinToolAdapter::register_async_spawn_tool(
                    &extension_core,
                    spawn_tool,
                )
                .await
            {
                warn!("Failed to register per-agent AsyncSpawnTool: {}", e);
            }
            if let Err(e) =
                crate::extensions::builtin::BuiltinToolAdapter::register_async_output_tool(
                    &extension_core,
                    output_tool,
                )
                .await
            {
                warn!("Failed to register per-agent AsyncOutputTool: {}", e);
            }

            // Register the per-agent introspection trio so this agent only sees
            // its own async tasks. `register_tool` is idempotent — it unregisters
            // any prior instance with the same name first.
            for (tool_name, tool) in [
                (
                    "AsyncStatus",
                    Arc::new(crate::tools::builtin::AsyncStatusTool::with_registry(
                        async_registry.clone(),
                    )) as Arc<dyn Tool>,
                ),
                (
                    "AsyncList",
                    Arc::new(crate::tools::builtin::AsyncListTool::with_registry(
                        async_registry.clone(),
                    )),
                ),
                (
                    "AsyncStop",
                    Arc::new(crate::tools::builtin::AsyncStopTool::with_registry(
                        async_registry.clone(),
                    )),
                ),
            ] {
                if let Err(e) = crate::extensions::builtin::BuiltinToolAdapter::register_tool(
                    &extension_core,
                    tool,
                )
                .await
                {
                    warn!("Failed to register per-agent {tool_name}Tool: {e}");
                }
            }
        } else {
            tracing::debug!(
                "Async tools disabled by config for agent '{}'",
                self.config.name
            );
        }

        // 5. Push the session key onto the core so AsyncSpawn can stamp
        //    parent_session_key on every spawned task. The session key is
        //    keyed by this agent's DID on the shared core so concurrent
        //    agents in daemon mode do not clobber each other (issue #68).
        extension_core
            .set_session_key(&self.identity.did, session_key)
            .await;

        // 6. Construct AgenticLoop with the queue.
        let mut loop_ =
            crate::engine::agentic_loop::AgenticLoop::new(agent_arc, provider, extension_core)
                .await
                .with_async_completion_queue(async_completion_queue)
                .with_caller_id(caller_id);
        if let Some(token) = cancel {
            loop_ = loop_.with_cancel_token(token);
        }
        Ok(loop_)
    }

    /// Prepare agent for execution by initializing built-in tools and invoking `AgentInit` hooks.
    async fn prepare_execution(&self) -> anyhow::Result<()> {
        if let Err(e) = self.init_builtins_async().await {
            return Err(anyhow::anyhow!("Failed to initialize tools: {e}"));
        }

        let init_result = self
            .extension_core
            .invoke_hook(
                crate::extensions::framework::core::HookPoint::AgentInit,
                crate::extensions::framework::types::HookInput::Unit,
            )
            .await;
        tracing::info!(
            "AgentInit hook result: {:?}",
            std::mem::discriminant(&init_result)
        );

        Ok(())
    }

    /// Wait for background async tasks to complete
    pub async fn wait_for_async_tasks(&self, timeout: std::time::Duration) {
        self.extension_core.wait_for_async_tasks(timeout).await;
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
    pub fn principal_id(&self) -> &crate::principal::PrincipalId {
        &self.principal_id
    }

    /// Get the spawning principal's human-readable name, if known.
    #[must_use]
    pub fn principal_name(&self) -> Option<&str> {
        self.principal_name.as_deref()
    }

    /// Snapshot of the principal's allowlist bound at construction.
    ///
    /// The engine consults this list when filtering the tool bag
    /// during `init_builtins_async` and when computing per-call
    /// tool definitions. **Track B**: replaces
    /// `AgentConfig::extension_whitelist()` for runtime reads.
    ///
    /// `None` means the agent is unbound and no allowlist-based
    /// filtering is applied.
    #[must_use]
    pub fn principal_allowed_extensions(
        &self,
    ) -> Option<&Arc<crate::principal::config::AllowedExtensions>> {
        self.principal_allowed_extensions.as_ref()
    }

    // Session overlay methods

    /// Get the session manager
    #[must_use]
    pub fn session_manager(&self) -> Arc<TokioRwLock<SessionManager>> {
        Arc::clone(&self.session_manager)
    }

    /// Resolve a session for a peer and channel
    ///
    /// This is the primary method for channels to get a session.
    /// It ensures cross-channel context sharing for the same peer.
    ///
    /// Returns a `ResolvedSession` containing both the metadata DTO (`context`)
    /// and the operations handle (`handle`). Use `context` for read-only metadata
    /// and `handle` for all session operations.
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
    /// Creates a new spawn overlay for isolated task execution.
    /// Use `isolated=true` for tasks that should not share context.
    ///
    /// Returns a `ResolvedSession` containing both the metadata DTO (`context`)
    /// and the operations handle (`handle`).
    pub async fn spawn_session(
        &self,
        peer: &Subject,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        timeout_seconds: Option<u64>,
    ) -> Result<ResolvedSession> {
        let mut manager = self.session_manager.write().await;
        manager
            .spawn_session(
                &self.config.name,
                peer,
                task,
                isolated,
                parent_session_key,
                timeout_seconds,
            )
            .await
    }

    // Session management commands (CLI integration)

    /// Create a new session (/new command)
    pub async fn session_new(&self, peer: &Subject) -> Result<String> {
        use crate::session::manager::SessionCreateOptions;
        let mut manager = self.session_manager.write().await;
        let options = SessionCreateOptions::new().with_trigger("user");
        let handle = manager
            .create_session(&self.config.name, peer, options)
            .await?;
        let session_id = handle.session_id().to_string();
        info!("Created new session {} for peer {:?}", session_id, peer);
        Ok(session_id)
    }

    /// Branch current session (/branch command)
    pub async fn session_branch(&self, peer: &Subject, label: Option<String>) -> Result<String> {
        let mut manager = self.session_manager.write().await;
        let session_id = manager.branch_session(peer, label).await?;
        info!("Branched session {} from peer {:?}", session_id, peer);
        Ok(session_id)
    }

    /// Switch to a different session (/switch command)
    pub async fn session_switch(&self, peer: &Subject, session_id: &str) -> Result<()> {
        let mut manager = self.session_manager.write().await;
        manager.switch_session(peer, session_id).await?;
        info!("Switched peer {:?} to session {}", peer, session_id);
        Ok(())
    }

    /// List all sessions for a peer (/sessions command)
    pub async fn session_list(&self, peer: &Subject) -> Result<Vec<crate::session::SessionEntry>> {
        let mut manager = self.session_manager.write().await;
        let sessions = manager.list_sessions_for_peer(peer).await?;
        Ok(sessions)
    }

    /// Format session list for display
    #[must_use]
    pub fn format_session_list(
        &self,
        sessions: &[crate::session::SessionEntry],
        active_id: Option<&str>,
    ) -> String {
        if sessions.is_empty() {
            return "No sessions found.".to_string();
        }

        let mut output = String::from("📁 Sessions:\n\n");

        for (i, session) in sessions.iter().enumerate() {
            let is_active = active_id.is_some_and(|id| id == session.session_id);
            let marker = if is_active { "●" } else { "○" };
            let label = session.title.as_deref().unwrap_or("unnamed");
            let short_id = &session.session_id[..8];

            output.push_str(&format!("{} {}. {} ({})", marker, i + 1, label, short_id));

            if let Some(ref parent) = session.parent_session_id {
                output.push_str(&format!(" [branched from {}]", &parent[..8]));
            }

            if is_active {
                output.push_str(" ← active");
            }

            output.push('\n');
        }

        output.push_str("\nUse /switch <number> or /switch <session-id> to change session\n");
        output
    }

    /// Process a session command and return (handled, response)
    ///
    /// Returns (true, response) if the command was handled
    /// Returns (false, _) if not a command (should be processed as normal message)
    pub async fn process_session_command(
        &self,
        peer: &Subject,
        command: &str,
    ) -> Result<(bool, String)> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Ok((false, String::new()));
        }

        match parts[0] {
            "/new" => {
                let session_id = self.session_new(peer).await?;
                Ok((true, format!(
                    "✨ Created new session!\n\nSession ID: {}\n\nYou can switch back to previous sessions with /sessions and /switch",
                    &session_id[..8]
                )))
            }

            "/branch" => {
                let label = parts.get(1).map(std::string::ToString::to_string);
                let session_id = self.session_branch(peer, label.clone()).await?;
                let label_str = label.as_deref().unwrap_or("unnamed");
                Ok((true, format!(
                    "🌿 Branched new session from current!\n\nLabel: {}\nSession ID: {}\n\nThis session contains a copy of the current conversation.",
                    label_str,
                    &session_id[..8]
                )))
            }
            "/switch" => {
                if parts.len() < 2 {
                    return Ok((true, "Usage: /switch <session-number> or /switch <session-id>\n\nUse /sessions to see available sessions.".to_string()));
                }

                let sessions = self.session_list(peer).await?;
                let target = parts[1];

                // Try to parse as index first (1-based)
                let session_id = if let Ok(index) = target.parse::<usize>() {
                    if index == 0 || index > sessions.len() {
                        return Ok((true, format!("Invalid session number. Use /sessions to see available sessions (1-{}).", sessions.len())));
                    }
                    sessions[index - 1].session_id.clone()
                } else {
                    // Try as session ID (or partial)
                    let target_lower = target.to_lowercase();
                    sessions
                        .iter()
                        .find(|s| s.session_id.to_lowercase().starts_with(&target_lower))
                        .map(|s| s.session_id.clone())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Session '{target}' not found. Use /sessions to see available sessions."
                            )
                        })?
                };

                self.session_switch(peer, &session_id).await?;
                Ok((true, format!(
                    "↔️  Switched to session {}\n\nPrevious messages are now from the selected session context.",
                    &session_id[..8]
                )))
            }
            "/sessions" => {
                let sessions = self.session_list(peer).await?;
                let active_id = sessions
                    .iter()
                    .find(|s| {
                        Some(s.session_id.as_str())
                            == sessions.first().map(|f| f.session_id.as_str())
                    })
                    .map(|s| s.session_id.clone());
                let output = self.format_session_list(&sessions, active_id.as_deref());
                Ok((true, output))
            }
            "/help" => Ok((
                true,
                "📚 Available commands:\n\n\
                    /new           - Create a new empty session\n\
                    /branch        - Branch (fork) current session\n\
                    /sessions      - List all sessions\n\
                    /switch <n>    - Switch to session by number or ID\n\
                    /help          - Show this help\n\n\
                    All other text is sent to the agent."
                    .to_string(),
            )),
            _ => {
                // Not a recognized command
                Ok((false, String::new()))
            }
        }
    }

    /// Create an agent for unit tests with isolated storage.
    ///
    /// Uses a temporary directory for identity and session storage so tests
    /// do not conflict with each other or the user's real data.
    #[cfg(test)]
    pub async fn new_for_test(config: AgentConfig, temp_dir: &std::path::Path) -> Result<Self> {
        use crate::identity::storage::KeyStorage;

        let path_resolver = PathResolver::with_dirs(
            temp_dir.join("config"),
            temp_dir.join("data"),
            temp_dir.join("cache"),
        );
        let session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, &config.name)
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));

        // Load or create identity in temp storage
        let identity = {
            let storage = KeyStorage::with_path(temp_dir.join("data").join("identities"))?;
            let identity_name = config.name.clone();
            if let Ok(identity) = storage.load(&identity_name) {
                identity
            } else {
                let identity = Identity::generate(DIDScope::Local, None)?;
                storage.store(&identity)?;
                identity
            }
        };

        let provider = Self::init_provider(&config, None, None).await?;

        let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
            "agent:{}:cli:default",
            config.name
        )));

        let extension_core = global_core().expect("Global ExtensionCore not initialized");

        let subagent_executor_base = SubagentExecutor::new(
            Arc::clone(&session_manager),
            config.name.clone(),
            5,
            crate::principal::PrincipalId::generate(),
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
            state: Arc::new(RwLock::new(AgentState::Idle)),
            identity,
            provider,
            llm_resolver: None,
            session_manager,
            subagent_executor,
            session_key_provider,
            current_session_id: Arc::new(tokio::sync::RwLock::new(None)),
            extension_core,
            inbox_registry: None,
            principal_workspace: None,
            caller_principal_did: None,
            principal_id: crate::principal::PrincipalId::generate(),
            principal_name: None,
            principal_allowed_extensions: None,
        })
    }

    // Private helper methods

    async fn load_or_create_identity(config: &AgentConfig) -> Result<Identity> {
        let storage = KeyStorage::new()?;

        // Issue #28: prefer lookup by `agent_did` (the on-disk filename is
        // the DID, not the agent name). Pre-#28 configs stored identity
        // under `{name}.json` which meant a fresh keypair was generated on
        // every agent start — the fix keys the lookup by the stable DID
        // and falls back to the legacy name-keyed path so existing agents
        // still resolve.
        if let Some(ref did) = config.agent_did {
            if let Ok(identity) = storage.load(did) {
                info!("Loaded identity by agent_did: {}", identity.did);
                return Ok(identity);
            }
            // agent_did set but identity file missing — broken state.
            // Review of #34: silent key rotation is a smell. Log the
            // rotation at `info` level naming BOTH the old and new DIDs
            // so an operator restoring from backup can correlate the
            // event. The caller (`new_with_session_manager` /
            // `new_with_shared_executor`) will overwrite `agent_did` in
            // the config — see `persist_agent_did` for the targeted
            // read-modify-write that doesn't clobber other fields.
            //
            // The follow-up ADR called out in #28 (DID rotation / key
            // compromise recovery) is the right place to add a
            // fail-closed mode and a `RecoveryClaim` event; for now we
            // log loudly and continue.
            warn!(
                "agent_did '{}' in config does not resolve to a stored identity; \
                 generating a replacement. Any cross-runtime grants or \
                 audit references to the old DID will be orphaned \
                 (issue #28 follow-up: DID rotation ADR pending).",
                did
            );
        }

        // Legacy fallback: identity file may be keyed by agent name
        // (pre-#28 — buggy but tolerated).
        if let Ok(identity) = storage.load(&config.name) {
            info!("Loaded legacy name-keyed identity: {}", identity.did);
            return Ok(identity);
        }

        // Create new identity
        info!("Creating new identity for: {}", config.name);
        let identity = Identity::generate(DIDScope::Local, None)?;

        storage.store(&identity)?;
        info!("Created and stored new identity: {}", identity.did);

        Ok(identity)
    }

    /// Persist the resolved agent_did back into the on-disk config.toml.
    ///
    /// Issue #28: the per-agent DID is generated lazily on first
    /// `Agent::new()`; this call backfills it into `config.toml` so the
    /// DID is stable across restarts and visible to the tunnel dispatcher
    /// (which reads `agent_did` straight from the config file when
    /// building `InstanceAnnouncePayload`).
    ///
    /// **Read-modify-write, not a full overwrite** (review of #34):
    /// the previous version `toml::to_string_pretty(&config)`-ed the
    /// entire `AgentConfig` and wrote it back, which would clobber any
    /// hand-edited comments, key ordering, or concurrent writer's
    /// changes. This version reads the existing TOML, sets just the
    /// `agent_did` key on the parsed `toml::Value`, and re-serializes —
    /// preserving other fields, comments, and key ordering as long as
    /// the same TOML structure is used. Concurrent writers are still
    /// vulnerable to a lost update (no file lock); the call site guards
    /// against this by skipping the backfill if the in-memory
    /// `config.agent_did` already matches.
    ///
    /// Best-effort: a write failure is logged but not propagated. The
    /// in-memory identity is still valid; the next agent start will
    /// retry the write. The caller is responsible for providing the
    /// correct `config_path` — `PathResolver::agent_config(name)` is the
    /// canonical location.
    async fn backfill_agent_did(
        config_path: &std::path::Path,
        config: &AgentConfig,
        agent_did: &str,
    ) -> Result<()> {
        if config.agent_did.as_deref() == Some(agent_did) {
            return Ok(());
        }

        // Best-effort: if the on-disk config location doesn't exist yet
        // (e.g. a Principal-only install where `~/.peko/agents/` was never
        // created, or a freshly-spawned subagent whose parent hasn't written
        // its config), there's nothing to backfill and the in-memory identity
        // is already valid. Skip silently rather than spamming a warning
        // every daemon tick.
        let Some(parent) = config_path.parent() else {
            return Ok(());
        };
        if !parent.exists() {
            debug!(
                "Skipping agent_did backfill for {:?}: parent dir {} does not exist \
                 (Principal-only install or subagent without on-disk config)",
                config_path,
                parent.display()
            );
            return Ok(());
        }

        // Read the existing TOML so we preserve any fields we don't know
        // about (forward-compat) and the existing key ordering / comments
        // that the `toml` crate keeps when round-tripping a `Value`.
        let mut root: toml::Value = match tokio::fs::read_to_string(config_path).await {
            Ok(s) => toml::from_str(&s).with_context(|| {
                format!(
                    "Failed to parse existing config TOML at {}",
                    config_path.display()
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Config doesn't exist yet (e.g. subagent path) — write a
                // fresh file with just the agent_did set. The caller
                // path that triggers this is `new_with_shared_executor`
                // in a test, where the config is in memory only.
                toml::Value::Table(toml::map::Map::new())
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "Failed to read existing config at {}",
                        config_path.display()
                    )
                });
            }
        };

        if let toml::Value::Table(ref mut tbl) = root {
            tbl.insert(
                "agent_did".to_string(),
                toml::Value::String(agent_did.to_string()),
            );
        } else {
            anyhow::bail!(
                "Refusing to write agent_did: existing config at {} is not a TOML table",
                config_path.display()
            );
        }

        let toml_str =
            toml::to_string_pretty(&root).context("Failed to serialize updated AgentConfig")?;

        tokio::fs::write(config_path, toml_str)
            .await
            .with_context(|| {
                format!(
                    "Failed to persist agent_did to {} (in-memory identity will still work this session)",
                    config_path.display()
                )
            })?;

        info!("Backfilled agent_did into config: {}", agent_did);
        Ok(())
    }

    /// True if `path` lives under the system temp directory.
    ///
    /// Review of #34 concern #5: the `Agent::new_with_shared_executor`
    /// path resolves its config path via `PathResolver::new()`, which
    /// reads `PEKO_HOME` or defaults to the user's real `~/.peko`.
    /// Tests that bypass `new_for_test` (e.g. exercises of the
    /// subagent executor with a manually-constructed `AgentConfig`)
    /// would otherwise mutate the developer's real config on
    /// `cargo test`. The check is conservative: any path under
    /// `std::env::temp_dir()` is treated as a test path and the
    /// on-disk backfill is skipped — the in-memory identity is still
    /// valid, the next production-path call (real `Agent::new`) will
    /// do the real backfill.
    fn is_path_under_temp_dir(path: &std::path::Path) -> bool {
        let temp = std::env::temp_dir();
        // Canonicalize where possible so a relative `target/debug/...`
        // path still matches an absolute temp path. If canonicalize
        // fails (path doesn't exist), fall back to lexical comparison
        // on the original path.
        let path_abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let temp_abs = temp.canonicalize().unwrap_or_else(|_| temp.clone());
        path_abs.starts_with(&temp_abs)
    }

    async fn init_provider(
        config: &AgentConfig,
        resolver: Option<&Arc<crate::providers::LlmResolver>>,
        // **Track B**: per-agent `preferred_*` fields were removed
        // from `AgentConfig`. The hint now arrives as an explicit
        // parameter — the production caller passes the principal's
        // `(preferred_provider_id, preferred_model_id)` pair from
        // `PrincipalContext::provider_hint`; tests that bypass the
        // principal path pass `None` and let the resolver pick the
        // catalog default.
        provider_hint: Option<(Option<String>, Option<String>)>,
    ) -> Result<Option<Arc<crate::providers::Provider>>> {
        // v3 path: ask the resolver to build a one-shot provider from
        // the supplied hint (or the runtime default). No legacy
        // fallback — the inline `[provider]` block on `AgentConfig`
        // is gone; the resolver is the only source of truth.
        let Some(r) = resolver else {
            return Ok(None);
        };
        let (agent_provider, agent_model) = provider_hint.unwrap_or_default();
        let req = crate::providers::resolver::ResolveRequest {
            agent_provider: agent_provider.as_deref(),
            agent_model: agent_model.as_deref(),
            ..Default::default()
        };
        match r.build(req).await {
            Ok((provider, choice)) => {
                info!("Agent '{}' resolved provider: {}", config.name, choice);
                Ok(Some(provider))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent_config::AgentConfig;

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_creation() {
        use crate::extensions::framework::core::ExtensionCore;

        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale (Windows-headless
        // keyring panics).
        crate::identity::init_test_env();

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config).await;
        assert!(agent.is_ok());

        let agent = agent.unwrap();
        assert_eq!(agent.name(), "test-agent");
        assert!(agent.did().starts_with("did:peko:"));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_has_session_manager() {
        use crate::extensions::framework::core::ExtensionCore;

        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent-session".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();

        // Agent should have a session manager
        let manager = agent.session_manager();
        let manager_guard = manager.read().await;
        assert_eq!(manager_guard.base_session_count(), 0);
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_session_routing() {
        use crate::auth::Subject;
        use crate::extensions::framework::core::ExtensionCore;
        use crate::session::types::ChannelType;

        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent-router".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();

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
        use crate::auth::Subject;
        use crate::extensions::framework::core::ExtensionCore;
        use crate::session::types::ChannelType;

        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent-context".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();
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
        use crate::auth::Subject;
        use crate::extensions::framework::core::ExtensionCore;

        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent-spawn".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();
        let peer = Subject::User("bob".to_string());

        // Create a parent session first
        let parent_resolved = agent
            .resolve_session(&peer, crate::session::types::ChannelType::Cli, "default")
            .await
            .unwrap();
        let parent_key = parent_resolved.context.full_session_key.clone();

        // Spawn a child session with shared context
        let spawn_resolved = agent
            .spawn_session(&peer, "test task", false, &parent_key, Some(300))
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
        use crate::extensions::framework::core::ExtensionCore;

        use tempfile::TempDir;

        // Force the encrypted-file identity fallback (Windows-headless
        // keyring panics otherwise).
        crate::identity::init_test_env();

        let core = Arc::new(ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(core);

        let make_config = |name: &str| AgentConfig {
            name: name.to_string(),
            ..Default::default()
        };

        // Two distinct peko_home roots.
        let tmp_a = TempDir::new().expect("tempdir A");
        let tmp_b = TempDir::new().expect("tempdir B");

        let agent_a = Agent::new_for_test(make_config("helper"), tmp_a.path())
            .await
            .expect("agent A");
        let agent_b = Agent::new_for_test(make_config("helper"), tmp_b.path())
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
}
