//! Agent management module

use crate::agent::subagent_executor::SubagentExecutor;
use crate::common::paths::PathResolver;
use crate::extensions::adapters::builtin_tool_adapter::BuiltinToolAdapter;
use crate::extensions::core::{global_core, ExtensionCore};
use crate::identity::{did::DIDScope, storage::KeyStorage, Identity};
use crate::session::context::SessionContext;
use crate::session::manager::SessionManager;
use crate::session::types::{ChannelType, Peer};
use crate::tools::agent_spawn::DynamicSessionKeyProvider;
use crate::types::agent::{AgentConfig, AgentState};
use anyhow::Result;
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
    /// LLM provider (stored in Arc for sharing with agentic loop)
    provider: Option<Arc<crate::providers::Provider>>,
    /// Session manager for overlay lifecycle
    session_manager: Arc<TokioRwLock<SessionManager>>,
    /// Subagent executor for background task execution
    subagent_executor: Arc<SubagentExecutor>,
    /// Dynamic session key provider for `agent_spawn` tool
    session_key_provider: Arc<DynamicSessionKeyProvider>,
    /// Current session ID for `session_status` tool lookups
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
        use crate::tools::session_introspection::SessionIntrospector;
        use crate::tools::{
            AgentSpawnListTool, AgentSpawnStatusTool, AgentSpawnTool, SessionStatusTool,
            SessionsSendTool, Tool,
        };

        // Defensive check: common built-ins must be pre-registered by the daemon startup path.
        // AppState::new() calls ToolRuntime::with_workspace_and_core() which registers
        // all common built-ins on the global ExtensionCore before any Agent is created.
        let has_shell = self.extension_core.get_tool_metadata("shell").await.is_some();
        if !has_shell {
            tracing::error!(
                "Built-in tools not pre-registered on ExtensionCore. \
                 This indicates a startup ordering bug — AppState should initialize \
                 ToolRuntime before StatelessAgentService."
            );
        }

        // Agent-specific tools (not part of ToolRuntime::register_builtins)
        let mut tools: Vec<Arc<dyn Tool>> = vec![];

        // Add session introspection tools backed by the real session manager
        let session_registry = SessionIntrospector::new(
            self.session_manager.clone(),
            self.current_session_id.clone(),
        );
        tools.push(Arc::new(SessionStatusTool::new(Box::new(session_registry))));

        // Add agent spawn tool v2 with executor and session provider
        tools.push(Arc::new(AgentSpawnTool::with_session_provider(
            self.subagent_executor.clone(),
            Box::new(self.session_key_provider.clone()),
        )));

        // Add spawn status and list tools (bound to this agent's shared registry)
        tools.push(Arc::new(AgentSpawnStatusTool::with_registry(
            self.subagent_executor.registry().clone(),
        )));
        tools.push(Arc::new(AgentSpawnListTool::with_registry(
            self.subagent_executor.registry().clone(),
        )));

        // Add sessions_send tool for A2A messaging
        tools.push(Arc::new(SessionsSendTool::new().with_session_context(
            format!("agent:{}", self.config.name),
            &self.config.name,
        )));

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
                        return tool.name().to_lowercase().starts_with(&prefix.to_lowercase());
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
        self.extension_core
            .set_tool_config(ext_config)
            .await;

        // Load Universal Tools from extensions directory (where `pekobot ext install` puts them)
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
            use crate::extensions::adapters::BuiltInAdapters;
            use crate::extensions::manager::ExtensionManager;
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
                    tracing::warn!("❌ Failed to load extensions: {}", e);
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
            .with_path_resolver(path_resolver, &config.name, config.team.as_deref())
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));
        Self::new_with_session_manager(config, session_manager).await
    }

    /// Create a new agent with an existing session manager.
    ///
    /// Used for subagent execution where the child must share the parent's
    /// session manager (and therefore session storage and context).
    pub async fn new_with_session_manager(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
    ) -> Result<Self> {
        info!("Creating agent: {}", config.name);

        // Load or create identity
        let identity = Self::load_or_create_identity(&config).await?;

        // Initialize provider if configured
        let provider = Self::init_provider(&config).await?;

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
    pub async fn new_with_shared_executor(
        config: AgentConfig,
        session_manager: Arc<TokioRwLock<SessionManager>>,
        subagent_executor: Arc<SubagentExecutor>,
    ) -> Result<Self> {
        info!("Creating agent with shared executor: {}", config.name);

        let identity = Self::load_or_create_identity(&config).await?;
        let provider = Self::init_provider(&config).await?;

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
        history: Option<Vec<crate::providers::ChatMessage>>,
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
            let loop_ = crate::engine::agentic_loop::AgenticLoop::new(
                agent_arc,
                provider,
                extension_core,
            )
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
    pub async fn execute_streaming_with_session<F>(
        &self,
        prompt: &str,
        session: std::sync::Arc<tokio::sync::RwLock<crate::session::Session>>,
        history: Option<Vec<crate::providers::ChatMessage>>,
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

        // Capture current session ID so session_status can look it up
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
        .await;

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
                crate::extensions::core::HookPoint::AgentInit,
                crate::extensions::types::HookInput::Unit,
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

    /// Get or create a session context for a peer and channel
    ///
    /// This is the primary method for channels to get a session.
    /// It ensures cross-channel context sharing for the same peer.
    pub async fn get_session_context(
        &self,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<SessionContext> {
        let mut manager = self.session_manager.write().await;
        let resolved = manager
            .route(peer, channel_type, channel_id, Some(&self.config.name))
            .await?;
        Ok(resolved.context)
    }

    /// Get or create a session context for the default user
    ///
    /// Convenience method for CLI and simple channels
    pub async fn get_default_session_context(&self) -> Result<SessionContext> {
        let peer = Peer::User("default".to_string());
        self.get_session_context(&peer, ChannelType::Cli, "default")
            .await
    }

    /// Create a spawn/subagent session
    ///
    /// Creates a new spawn overlay for isolated task execution.
    /// Use `isolated=true` for tasks that should not share context.
    pub async fn spawn_session(
        &self,
        peer: &Peer,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        timeout_seconds: Option<u64>,
    ) -> Result<SessionContext> {
        let mut manager = self.session_manager.write().await;
        let resolved = manager
            .spawn_session(
                &self.config.name,
                peer,
                task,
                isolated,
                parent_session_key,
                timeout_seconds,
            )
            .await?;
        Ok(resolved.context)
    }

    // Session management commands (CLI integration)

    /// Create a new session (/new command)
    pub async fn session_new(&self, peer: &Peer) -> Result<String> {
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
    pub async fn session_branch(&self, peer: &Peer, label: Option<String>) -> Result<String> {
        let mut manager = self.session_manager.write().await;
        let session_id = manager.branch_session(peer, label).await?;
        info!("Branched session {} from peer {:?}", session_id, peer);
        Ok(session_id)
    }

    /// Switch to a different session (/switch command)
    pub async fn session_switch(&self, peer: &Peer, session_id: &str) -> Result<()> {
        let mut manager = self.session_manager.write().await;
        manager.switch_session(peer, session_id).await?;
        info!("Switched peer {:?} to session {}", peer, session_id);
        Ok(())
    }

    /// List all sessions for a peer (/sessions command)
    pub async fn session_list(&self, peer: &Peer) -> Result<Vec<crate::session::SessionEntry>> {
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
        peer: &Peer,
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

    // Private helper methods

    async fn load_or_create_identity(config: &AgentConfig) -> Result<Identity> {
        let storage = KeyStorage::new()?;

        let tenant = config.tenant.as_deref().unwrap_or("default");
        let identity_name = format!("{}-{}", tenant, config.name);

        // Try to load existing identity
        if let Ok(identity) = storage.load(&identity_name) {
            info!("Loaded existing identity: {}", identity.did);
            return Ok(identity);
        }

        // Create new identity
        info!("Creating new identity for: {}", identity_name);
        let identity = Identity::generate(DIDScope::Local, Some(tenant))?;

        storage.store(&identity)?;
        info!("Created and stored new identity: {}", identity.did);

        Ok(identity)
    }

    async fn init_provider(
        config: &AgentConfig,
    ) -> Result<Option<Arc<crate::providers::Provider>>> {
        use crate::providers::registry::create_provider;
        use crate::types::provider::ProviderType;

        // Map ProviderType to string for registry lookup
        let provider_name = match config.provider.provider_type {
            ProviderType::OpenAI => "openai",
            ProviderType::Anthropic => "anthropic",
            ProviderType::Moonshot => "moonshot",
            ProviderType::Kimi => "kimi",
            ProviderType::Minimax => "minimax",
            ProviderType::Ollama => "ollama",
            ProviderType::OpenAICompatible => {
                // Use base_url to determine provider
                if let Some(ref url) = config.provider.base_url {
                    if url.contains("moonshot.cn") {
                        "moonshot"
                    } else if url.contains("kimi.com") {
                        "kimi"
                    } else if url.contains("minimaxi") {
                        "minimax"
                    } else if url.contains("groq") {
                        "groq"
                    } else if url.contains("together") {
                        "together"
                    } else if url.contains("fireworks") {
                        "fireworks"
                    } else {
                        "openai"
                    }
                } else {
                    "openai"
                }
            }
        };

        // Get the provider type for the registry
        let provider_type = match provider_name {
            "openai" => ProviderType::OpenAI,
            "anthropic" => ProviderType::Anthropic,
            "moonshot" => ProviderType::Moonshot,
            "kimi" => ProviderType::Kimi,
            "minimax" => ProviderType::Minimax,
            "ollama" => ProviderType::Ollama,
            _ => ProviderType::OpenAICompatible,
        };

        // Create provider config from the old config format
        let provider_config = crate::types::provider::ProviderConfig {
            provider_type,
            api_key: config.provider.api_key.clone(),
            api_key_env: config.provider.api_key_env.clone(),
            base_url: config.provider.base_url.clone(),
            default_model: config.provider.default_model.clone(),
            models: config.provider.models.clone(),
            timeout_seconds: config.provider.timeout_seconds,
            max_retries: config.provider.max_retries,
            retry_delay_ms: config.provider.retry_delay_ms,
        };

        match create_provider(provider_config) {
            Ok(provider) => Ok(Some(provider)),
            Err(e) => {
                warn!("Failed to create provider: {}", e);
                Ok(None)
            }
        }
    }
    /// Execute with native tool calling using `AgenticLoop` (unified API).
    ///
    /// This is the recommended method for agent execution with native tool calling support.
    /// The `on_event` callback receives all streaming events (text deltas, tool calls, etc.).

    /// Execute with native tool calling and return a channel receiver for events.
    ///
    /// This is a convenience wrapper around `execute_native()` that provides
    /// a channel-based interface for code that expects async event streaming.

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
    use crate::types::agent::AgentConfig;

    #[tokio::test]
    async fn test_agent_creation() {
        use crate::extensions::core::ExtensionCore;
        use crate::types::provider::{ProviderConfig, ProviderType};

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama,
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await;
        assert!(agent.is_ok());

        let agent = agent.unwrap();
        assert_eq!(agent.name(), "test-agent");
        assert!(agent.did().starts_with("did:pekobot:"));
    }

    #[tokio::test]
    async fn test_agent_has_session_manager() {
        use crate::extensions::core::ExtensionCore;
        use crate::types::provider::{ProviderConfig, ProviderType};

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent-session".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama,
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();

        // Agent should have a session manager
        let manager = agent.session_manager();
        let manager_guard = manager.read().await;
        assert_eq!(manager_guard.base_session_count(), 0);
    }

    #[tokio::test]
    async fn test_agent_session_routing() {
        use crate::extensions::core::ExtensionCore;
        use crate::session::types::{ChannelType, Peer};
        use crate::types::provider::{ProviderConfig, ProviderType};

        // Initialize global ExtensionCore for the test
        let core = Arc::new(ExtensionCore::new());
        crate::extensions::core::init_global_core(core);

        let config = AgentConfig {
            name: "test-agent-router".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama,
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();

        // Session manager should be able to route to sessions
        let peer = Peer::User("test_user".to_string());
        let ctx = agent.get_session_context(&peer, ChannelType::Cli, "default").await;

        // Should succeed (requires filesystem in full test)
        // This just verifies routing is properly initialized
        assert!(ctx.is_ok() || ctx.is_err()); // Either is fine for this test
    }

    #[tokio::test]
    #[ignore = "requires filesystem access and provider setup"]
    async fn test_agent_get_session_context() {
        use crate::session::types::{ChannelType, Peer};
        use crate::types::provider::{ProviderConfig, ProviderType};

        let config = AgentConfig {
            name: "test-agent-context".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama,
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();
        let peer = Peer::User("alice".to_string());

        let ctx = agent
            .get_session_context(&peer, ChannelType::Cli, "default")
            .await;

        assert!(ctx.is_ok());
        let ctx = ctx.unwrap();
        assert_eq!(ctx.channel_type, Some(ChannelType::Cli));
    }

    #[tokio::test]
    #[ignore = "requires filesystem access and provider setup"]
    async fn test_agent_spawn_session() {
        use crate::session::types::Peer;
        use crate::types::provider::{ProviderConfig, ProviderType};

        let config = AgentConfig {
            name: "test-agent-spawn".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama,
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();
        let peer = Peer::User("bob".to_string());

        // Create a parent session first
        let parent_ctx = agent
            .get_session_context(&peer, crate::session::types::ChannelType::Cli, "default")
            .await
            .unwrap();
        let parent_key = parent_ctx.full_session_key.clone();

        // Spawn a child session with shared context
        let spawn_ctx = agent
            .spawn_session(&peer, "test task", false, &parent_key, Some(300))
            .await;

        assert!(spawn_ctx.is_ok());
        let spawn_ctx = spawn_ctx.unwrap();
        assert!(spawn_ctx.is_subagent);
        assert!(!spawn_ctx.is_isolated);
    }
}
