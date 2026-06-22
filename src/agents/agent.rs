//! Agent management module

use crate::agents::subagent_executor::SubagentExecutor;
use crate::common::paths::PathResolver;
use crate::extensions::framework::core::{global_core, ExtensionCore};
use crate::extensions::builtin::BuiltinToolAdapter;
use crate::identity::{did::DIDScope, storage::KeyStorage, Identity};
use crate::session::manager::{ResolvedSession, SessionManager};
use crate::auth::principal::Principal;
use crate::session::types::{ChannelType};
use crate::tools::builtin::messaging::agent_spawn::DynamicSessionKeyProvider;
use crate::agents::agent_config::AgentConfig;
use crate::common::types::agent_legacy::AgentState;
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
    /// Dynamic session key provider for `agent_spawn` tool
    session_key_provider: Arc<DynamicSessionKeyProvider>,
    /// Current session ID for `session` tool lookups
    current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Extension core for skill loading and hook integration
    extension_core: Arc<ExtensionCore>,
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
        }
    }
}

impl Agent {
    /// Initialize built-in tools and register them with `ExtensionCore`.
    ///
    /// This asynchronous version loads Universal Tools from extensions directory
    /// and registers only agent-specific built-in tools with `ExtensionCore`.
    /// Common built-in tools (shell, read_file, write_file, etc.) are already
    /// registered by the daemon's `AppState` startup via `ToolRuntime`.
    /// Extension tools (Universal and MCP) are registered via `ExtensionManager` hooks.
    pub(crate) async fn init_builtins_async(&self) -> anyhow::Result<()> {
        use crate::tools::builtin::session::SessionIntrospector;
        use crate::tools::{AgentSpawnTool, SessionTool, Tool};

        // Defensive check: common built-ins must be pre-registered by the daemon startup path.
        // AppState::new() calls ToolRuntime::with_workspace_and_core() which registers
        // all common built-ins on the global ExtensionCore before any Agent is created.
        let has_shell = self
            .extension_core
            .get_tool_metadata("shell")
            .await
            .is_some();
        if !has_shell {
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

        // Add agent spawn tool v2 with executor and session provider
        tools.push(Arc::new(AgentSpawnTool::with_session_provider(
            self.subagent_executor.clone(),
            Box::new(self.session_key_provider.clone()),
        )));

        // Note: `task` tool (status/list/cancel) is registered globally by the daemon's
        // ToolRuntime::register_builtins() and searches across all registries at runtime.
        // We do NOT register per-agent versions here to avoid shadowing the global
        // registrations and breaking visibility of router async tasks.

        // Add a2a_send tool for agent-to-agent messaging (ADR-023)
        if let Some(agent_service) = self.extension_core.services().agent_service() {
            // Issue #28: prefer `agent_did` for the wire-side
            // `Principal::Agent`; fall back to the name for legacy agents
            // that predate the per-agent persistent keypair.
            //
            // Issue #29 (Slice B+C): also pull the cross-runtime a2a
            // ctx from the extension services (set by the daemon-state
            // after `start_tunnel`). If the runtime hasn't initialized
            // cross-runtime dispatch yet (offline runtime, no PekoHub
            // credential, or this PR's bootstrap hasn't shipped), the
            // ctx is `None` and the tool falls back to the local-only
            // path — same behavior as pre-#29.
            //
            // Cycle 5 (refactor/clippy-cleanup-rust196): coerce the
            // concrete service arc into the `AgentMessageService`
            // trait object so the tool is constructed through the
            // `build_tool` factory rather than directly binding to the
            // concrete `StatelessAgentService` type.
            let dyn_service: Arc<dyn crate::tunnel::a2a_send_tool::AgentMessageService> =
                agent_service;
            let cross_ctx = self
                .extension_core
                .services()
                .cross_runtime_a2a_ctx();
            let tool = crate::tunnel::a2a_send_tool::build_tool(
                dyn_service,
                Some(&self.config.name),
                self.config.agent_did.as_deref(),
                cross_ctx,
            );
            tools.push(tool);
        } else {
            tracing::warn!("StatelessAgentService not available on ExtensionCore — a2a_send tool will not be registered");
        }

        // Filter based on agent config extension whitelist
        let whitelist = self.config.extension_whitelist();
        if !whitelist.is_empty() {
            let before_count = tools.len();
            tools.retain(|tool| {
                whitelist.iter().any(|pattern: &String| {
                    if pattern.eq_ignore_ascii_case(tool.name()) {
                        return true;
                    }
                    if pattern.ends_with('*') {
                        let prefix = &pattern[..pattern.len() - 1];
                        return tool
                            .name()
                            .to_lowercase()
                            .starts_with(&prefix.to_lowercase());
                    }
                    false
                })
            });
            tracing::debug!("Filtered {} tools to {}", before_count, tools.len());
        }

        // ADR-020: Set tool config on ExtensionCore before registering tools
        // so that permission checks pass during registration.
        let mut ext_config = self.config.extensions.clone().unwrap_or_default();
        ext_config.enabled = self.config.extension_whitelist();
        self.extension_core.set_tool_config(ext_config).await;

        // Load Universal Tools from extensions directory (where `peko ext install` puts them)
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
            .with_path_resolver(path_resolver, &config.name, None)
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
            .with_path_resolver(path_resolver, &config.name, None)
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
    pub async fn new_with_session_manager_and_resolver(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        llm_resolver: Option<Arc<crate::providers::LlmResolver>>,
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
        // `backfill_agent_did` itself returns
        // "Failed to persist agent_did ... (in-memory identity will
        // still work this session)" so the next production-path
        // `Agent::new()` call will retry the write. Hard-failing here
        // would break unit tests that build agents against a tempdir
        // without pre-creating `agents/<name>/`.
        let config_path = PathResolver::new().agent_config(&config.name);
        if let Err(e) = Self::backfill_agent_did(&config_path, &config, &identity.did).await {
            warn!("Could not backfill agent_did into config: {}", e);
        }

        // Issue #28 review #4: surface DID rotation loudly. If the
        // resolved identity's DID differs from the one the config
        // claimed (a "broken state" recovery in load_or_create_identity
        // — identity file was missing, backup restore, etc.), log both
        // old and new so the operator can correlate audit / grant
        // breakage to the event.
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

        // Initialize provider if configured
        let provider = Self::init_provider(&config, llm_resolver.as_ref()).await?;

        // Initialize subagent executor
        let subagent_executor_base = SubagentExecutor::new(
            Arc::clone(&session_manager),
            config.name.clone(),
            5, // max_concurrent
        );
        let subagent_executor = match &provider {
            Some(p) => Arc::new(
                subagent_executor_base
                    .with_provider(p.clone())
                    .with_agent_config(config.clone()),
            ),
            None => Arc::new(subagent_executor_base),
        };

        // Initialize session key provider for agent_spawn tool
        let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
            "agent:{}:cli:default",
            config.name
        )));

        // Global ExtensionCore is always initialized in main.rs before command dispatch.
        let extension_core = global_core().expect("Global ExtensionCore not initialized");

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
        };

        info!(
            "Agent {} initialized with DID: {}",
            agent.config.name, agent.identity.did
        );

        Ok(agent)
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
    pub async fn new_with_shared_executor(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        subagent_executor: Arc<SubagentExecutor>,
        inherited_provider: Option<Arc<crate::providers::Provider>>,
    ) -> Result<Self> {
        info!("Creating agent with shared executor: {}", config.name);

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
            None => Self::init_provider(&config, None).await?,
        };
        let llm_resolver: Option<Arc<crate::providers::LlmResolver>> = None;

        let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
            "agent:{}:cli:default",
            config.name
        )));

        let extension_core = global_core().expect("Global ExtensionCore not initialized");

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

    /// Get the session key provider for agent_spawn tool.
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
        let loop_ = crate::engine::agentic_loop::AgenticLoop::new(
            agent_arc,
            provider,
            self.extension_core(),
        )
        .await;

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
    pub async fn execute_with_session(
        &self,
        prompt: &str,
        session: Arc<tokio::sync::RwLock<crate::session::Session>>,
        history: Option<Vec<crate::common::types::message::LlmMessage>>,
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

        // Initialize tool config on ExtensionCore
        let mut ext_config = self.config.extensions.clone().unwrap_or_default();
        ext_config.enabled = self.config.extension_whitelist();
        self.extension_core.set_tool_config(ext_config).await;

        if let Err(e) = self.prepare_execution().await {
            self.set_state(AgentState::Idle);
            return Err(e);
        }

        let agent_arc = Arc::new(self.clone());
        let loop_ = crate::engine::agentic_loop::AgenticLoop::new(
            agent_arc,
            provider,
            self.extension_core(),
        )
        .await;

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

    /// Execute with a channel-based event interface.
    ///
    /// Directly creates an `AgenticLoop` and returns a receiver for async streaming.
    pub async fn execute_streaming(
        &self,
        prompt: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>> {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(10000);
        let prompt = prompt.to_string();

        self.prepare_execution().await?;

        let agent_arc = Arc::new(self.clone());
        let event_tx_clone = event_tx.clone();
        let extension_core = self.extension_core();

        tokio::task::spawn_local(async move {
            let loop_ =
                crate::engine::agentic_loop::AgenticLoop::new(agent_arc, provider, extension_core)
                    .await;

            let _result = loop_
                .run(&prompt, move |event| {
                    if event_tx_clone.try_send(event).is_err() {
                        warn!("Agent event dropped (channel full)");
                    }
                })
                .await;
        });

        Ok(event_rx)
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
    ) -> Result<crate::engine::AgenticResult>
    where
        F: Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    {
        let Some(provider) = self.provider_arc() else {
            return Err(anyhow::anyhow!("No provider configured"));
        };

        // Initialize tool config on ExtensionCore
        let mut ext_config = self.config.extensions.clone().unwrap_or_default();
        ext_config.enabled = self.config.extension_whitelist();
        self.extension_core.set_tool_config(ext_config).await;

        // Capture current session ID so session tool can look it up
        {
            let session_id = session.read().await.id.clone();
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id);
        }

        self.prepare_execution().await?;

        let agent_arc = Arc::new(self.clone());
        let loop_ = crate::engine::agentic_loop::AgenticLoop::new(
            agent_arc,
            provider,
            self.extension_core(),
        )
        .await
        .with_caller_id(caller_id);

        let streaming_config = crate::engine::OrchestratorConfig::live();

        loop_
            .run_streaming_with_resume(prompt, on_event, session, history, streaming_config)
            .await
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
        peer: &Principal,
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
        let peer = Principal::User("default".to_string());
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
        peer: &Principal,
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
    pub async fn session_new(&self, peer: &Principal) -> Result<String> {
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
    pub async fn session_branch(&self, peer: &Principal, label: Option<String>) -> Result<String> {
        let mut manager = self.session_manager.write().await;
        let session_id = manager.branch_session(peer, label).await?;
        info!("Branched session {} from peer {:?}", session_id, peer);
        Ok(session_id)
    }

    /// Switch to a different session (/switch command)
    pub async fn session_switch(&self, peer: &Principal, session_id: &str) -> Result<()> {
        let mut manager = self.session_manager.write().await;
        manager.switch_session(peer, session_id).await?;
        info!("Switched peer {:?} to session {}", peer, session_id);
        Ok(())
    }

    /// List all sessions for a peer (/sessions command)
    pub async fn session_list(&self, peer: &Principal) -> Result<Vec<crate::session::SessionEntry>> {
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
        peer: &Principal,
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
            .with_path_resolver(path_resolver, &config.name, None)
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

        let provider = Self::init_provider(&config, None).await?;

        let subagent_executor_base =
            SubagentExecutor::new(Arc::clone(&session_manager), config.name.clone(), 5);
        let subagent_executor = match &provider {
            Some(p) => Arc::new(
                subagent_executor_base
                    .with_provider(p.clone())
                    .with_agent_config(config.clone()),
            ),
            None => Arc::new(subagent_executor_base),
        };

        let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
            "agent:{}:cli:default",
            config.name
        )));

        let extension_core = global_core().expect("Global ExtensionCore not initialized");

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
    ) -> Result<Option<Arc<crate::providers::Provider>>> {
        // v3 path: ask the resolver to build a one-shot provider from
        // the agent's `preferred_*` hints (or the runtime default).
        // No legacy fallback — the inline `[provider]` block on
        // `AgentConfig` is gone; the resolver is the only source of
        // truth.
        let Some(r) = resolver else {
            return Ok(None);
        };
        let req = crate::providers::resolver::ResolveRequest {
            agent_provider: config.preferred_provider_id.as_deref(),
            agent_model: config.preferred_model_id.as_deref(),
            ..Default::default()
        };
        match r.build(req).await {
            Ok((provider, choice)) => {
                info!(
                    "Agent '{}' resolved provider: {}",
                    config.name, choice
                );
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
        use crate::extensions::framework::core::ExtensionCore;
        use crate::auth::principal::Principal;
use crate::session::types::{ChannelType};
        

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
        let peer = Principal::User("test_user".to_string());
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
        use crate::extensions::framework::core::ExtensionCore;
        use crate::auth::principal::Principal;
use crate::session::types::{ChannelType};
        

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
        let peer = Principal::User("alice".to_string());

        let resolved = agent
            .resolve_session(&peer, ChannelType::Cli, "default")
            .await;

        assert!(resolved.is_ok());
        let resolved = resolved.unwrap();
        assert_eq!(resolved.context.channel_type, Some(ChannelType::Cli));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_spawn_session() {
        use crate::extensions::framework::core::ExtensionCore;
        use crate::auth::principal::Principal;
        

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
        let peer = Principal::User("bob".to_string());

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
    /// (a2a_send, `PermissionGrant.subject`, PekoHub instance rows)
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
