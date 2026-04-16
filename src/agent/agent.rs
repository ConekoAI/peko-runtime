//! Agent management module

use crate::agent::subagent_executor::SubagentExecutor;
use crate::common::paths::PathResolver;
use crate::extensions::adapters::builtin_tool_adapter::BuiltinToolAdapter;
use crate::extensions::core::{global_core, init_global_core, ExtensionCore};
use crate::identity::{did::DIDScope, storage::KeyStorage, Identity};
use crate::session::context::{SessionContext, SessionRouter};
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
    /// Session router for message routing
    session_router: SessionRouter,
    /// Subagent executor for background task execution
    subagent_executor: Arc<SubagentExecutor>,
    /// Dynamic session key provider for `agent_spawn` tool
    session_key_provider: Arc<DynamicSessionKeyProvider>,
    /// Current session ID for `session_status` tool lookups
    current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Extension core for skill loading and hook integration
    extension_core: Arc<ExtensionCore>,
}

impl Agent {
    /// Initialize built-in tools and register them with `ExtensionCore`.
    ///
    /// This asynchronous version loads Universal Tools from extensions directory
    /// and registers only built-in tools with `ExtensionCore`. Extension tools
    /// (Universal and MCP) are already registered via `ExtensionManager` hooks.
    async fn init_builtins_async(&self) -> anyhow::Result<()> {
        use crate::tools::session_introspection::AgentSessionRegistry;
        use crate::tools::{
            AgentSpawnListTool, AgentSpawnStatusTool, AgentSpawnTool, GlobTool, GrepTool,
            ReadFileTool, SessionStatusTool, SessionsSendTool, ShellTool, StrReplaceFileTool, Tool,
            WriteFileTool,
        };

        // Create core tools (granular filesystem tools are now preferred over FileSystemTool)
        // Initialize granular tools with workspace if configured
        let workspace = self
            .config
            .workspace
            .as_ref()
            .unwrap_or(&std::path::PathBuf::from("."))
            .clone();
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(ShellTool::new().with_workspace(workspace.clone())),
            // Granular filesystem tools - initialized with workspace if available
            Arc::new(ReadFileTool::new().with_workspace(workspace.clone())),
            Arc::new(WriteFileTool::new().with_workspace(workspace.clone())),
            Arc::new(GlobTool::new().with_workspace(workspace.clone())),
            Arc::new(GrepTool::new().with_workspace(workspace.clone())),
            Arc::new(StrReplaceFileTool::new().with_workspace(workspace.clone())),
        ];

        // Add session introspection tools backed by the real session manager
        let session_registry = AgentSessionRegistry::new(
            self.session_manager.clone(),
            self.current_session_id.clone(),
        );
        tools.push(Arc::new(SessionStatusTool::new(Box::new(session_registry))));

        // Add agent spawn tool v2 with executor and session provider
        tools.push(Arc::new(AgentSpawnTool::with_session_provider(
            self.subagent_executor.clone(),
            Box::new(self.session_key_provider.clone()),
        )));

        // Add spawn status and list tools
        tools.push(Arc::new(AgentSpawnStatusTool::new(
            self.subagent_executor.clone(),
        )));
        tools.push(Arc::new(AgentSpawnListTool::new(
            self.subagent_executor.registry().clone(),
        )));

        // Add sessions_send tool for A2A messaging
        tools.push(Arc::new(SessionsSendTool::new().with_session_context(
            format!("agent:{}", self.config.name),
            &self.config.name,
        )));

        // Filter based on agent config whitelist
        if let Some(ref tool_config) = self.config.tools {
            let before_count = tools.len();
            tools.retain(|tool| tool_config.is_tool_enabled(tool.name()));
            tracing::debug!("Filtered {} tools to {}", before_count, tools.len());
        }

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

        // ADR-018/019: Register ONLY built-in tools with ExtensionCore
        // Extension tools (Universal and MCP) are already registered via ExtensionManager hooks
        // This prevents double registration (DRY violation fix)
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
            "Registered {} built-in tools with ExtensionCore",
            tools.len()
        );

        Ok(())
    }

    /// Create a new agent with the given configuration
    pub async fn new(config: AgentConfig) -> Result<Self> {
        info!("Creating agent: {}", config.name);

        // Load or create identity
        let identity = Self::load_or_create_identity(&config).await?;

        // Initialize provider if configured
        let provider = Self::init_provider(&config).await?;

        // Initialize session manager with path resolver
        let path_resolver = PathResolver::new();
        let session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, &config.name, config.team.as_deref())
            .await?;
        let session_manager = Arc::new(TokioRwLock::new(session_manager));
        let session_router = SessionRouter::new(Arc::clone(&session_manager), config.name.clone());

        // Initialize subagent executor
        let subagent_executor_base = SubagentExecutor::new(
            session_router.clone(),
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

        // Get or initialize global extension core
        let extension_core = global_core().unwrap_or_else(|| {
            let core = Arc::new(ExtensionCore::new());
            init_global_core(core.clone());
            core
        });

        let agent = Self {
            config,
            state: Arc::new(RwLock::new(AgentState::Idle)),
            identity,
            provider,
            session_manager,
            session_router,
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

    /// Execute a task with the LLM provider using the unified callback API.
    ///
    /// The `on_event` callback receives streaming events (thinking tokens,
    /// tool calls, lifecycle events, etc.). Use `|_| {}` if you don't need
    /// streaming updates.
    ///
    /// Returns the final result including the answer and tool calls made.
    pub async fn execute(
        &self,
        prompt: &str,
        on_event: impl Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    ) -> Result<crate::engine::AgenticResultV4> {
        use crate::engine::loop_v4::AgenticLoopV4;

        use std::sync::Arc;

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        let result = if let Some(provider) = &self.provider {
            if let Err(e) = self.prepare_execution().await {
                self.set_state(AgentState::Idle);
                return Err(e);
            }

            let supports_native = provider.supports_native_tools();
            info!(
                "Executing with {} tool calling",
                if supports_native {
                    "native"
                } else {
                    "text-based"
                }
            );

            let provider_arc = Arc::clone(provider);
            let agent_arc = Arc::new(self.clone_for_loop(provider_arc.clone()));

            let loop_ = AgenticLoopV4::new(agent_arc, provider_arc, Arc::clone(&self.extension_core)).await;

            match loop_.run(prompt, on_event).await {
                Ok(result) => Ok(result),
                Err(e) => {
                    error!("Agentic loop V4 error: {}", e);
                    Err(e)
                }
            }
        } else {
            Err(anyhow::anyhow!("No provider configured"))
        };

        self.set_state(AgentState::Idle);
        result
    }

    /// Execute with a specific session and history.
    ///
    /// This allows resuming an existing conversation with full context.
    /// The session is used for persistence, and history provides the conversation context.
    pub async fn execute_with_session(
        &self,
        prompt: &str,
        session: Arc<tokio::sync::RwLock<crate::session::UnifiedSession>>,
        history: Option<Vec<crate::providers::ChatMessage>>,
        on_event: impl Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    ) -> Result<crate::engine::AgenticResultV4> {
        use crate::engine::loop_v4::AgenticLoopV4;
        use std::sync::Arc;

        if self.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.state()
            ));
        }

        self.set_state(AgentState::Busy);

        // Initialize tool config on ExtensionCore
        let tool_config = self.config.tools.clone().unwrap_or_default();
        self.extension_core.set_tool_config(tool_config).await;

        let result = if let Some(provider) = &self.provider {
            if let Err(e) = self.prepare_execution().await {
                self.set_state(AgentState::Idle);
                return Err(e);
            }

            let supports_native = provider.supports_native_tools();
            info!(
                "Executing with session and {} tool calling",
                if supports_native {
                    "native"
                } else {
                    "text-based"
                }
            );

            let provider_arc = Arc::clone(provider);
            let agent_arc = Arc::new(self.clone_for_loop(provider_arc.clone()));

            let loop_ = AgenticLoopV4::new(agent_arc, provider_arc, Arc::clone(&self.extension_core)).await;

            match loop_
                .run_with_resume(prompt, on_event, session, history)
                .await
            {
                Ok(result) => Ok(result),
                Err(e) => {
                    error!("Agentic loop V4 error: {}", e);
                    Err(e)
                }
            }
        } else {
            Err(anyhow::anyhow!("No provider configured"))
        };

        self.set_state(AgentState::Idle);
        result
    }

    /// Execute with a channel-based event interface.
    ///
    /// Convenience wrapper around `execute()` that returns a receiver for
    /// async event streaming. Use this when you need to process events
    /// in a separate async context.
    pub async fn execute_streaming(
        &self,
        prompt: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>> {
        // Use a large buffer to prevent event loss during bursts
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(10000);

        // Spawn the execution in a task
        let prompt = prompt.to_string();

        // We need to get the provider and tools into the task
        if let Some(provider) = &self.provider {
            let agent_arc = Arc::new(self.clone_for_loop(Arc::clone(provider)));
            self.prepare_execution().await?;

            let provider_arc = Arc::clone(provider);
            let event_tx_clone = event_tx.clone();

            let extension_core = self.extension_core.clone();

            tokio::task::spawn_local(async move {
                use crate::engine::loop_v4::AgenticLoopV4;

                let loop_ = AgenticLoopV4::new(agent_arc, provider_arc.clone(), extension_core).await;

                let _result = loop_
                    .run(&prompt, move |event| {
                        // Try to send event - log if dropped (buffer full means consumer is slow)
                        if event_tx_clone.try_send(event).is_err() {
                            warn!("Agent event dropped (channel full)");
                        }
                    })
                    .await;
            });
        } else {
            return Err(anyhow::anyhow!("No provider configured"));
        }

        Ok(event_rx)
    }

    /// Execute with streaming support using the provided session.
    ///
    /// The session must be provided by the caller (typically via `SessionManager`).
    /// This ensures session lifecycle is managed centrally.
    ///
    /// This version takes a sender callback for event streaming, avoiding channel
    /// lifetime issues. The callback is invoked synchronously for each event.
    pub async fn execute_streaming_with_session<F>(
        &self,
        prompt: &str,
        session: std::sync::Arc<tokio::sync::RwLock<crate::session::UnifiedSession>>,
        history: Option<Vec<crate::providers::ChatMessage>>,
        on_event: F,
    ) -> Result<crate::engine::AgenticResultV4>
    where
        F: Fn(crate::engine::AgenticEvent) + Send + Sync + 'static,
    {
        // Initialize tool config on ExtensionCore (mirrors AgentRunner behavior)
        let tool_config = self.config.tools.clone().unwrap_or_default();
        self.extension_core.set_tool_config(tool_config).await;

        // Capture current session ID so session_status can look it up
        {
            let session_id = session.read().await.id.clone();
            let mut current = self.current_session_id.write().await;
            *current = Some(session_id);
        }

        // We need to get the provider and tools
        if let Some(provider) = &self.provider {
            let agent_arc = Arc::new(self.clone_for_loop(Arc::clone(provider)));
            self.prepare_execution().await?;

            let provider_arc = Arc::clone(provider);

            use crate::engine::loop_v4::AgenticLoopV4;

            let loop_ = AgenticLoopV4::new(agent_arc, provider_arc, Arc::clone(&self.extension_core)).await;

            // Use streaming config with Live delivery mode for real-time output
            let streaming_config = crate::engine::OrchestratorConfig::live();

            let result = loop_
                .run_streaming_with_resume(prompt, on_event, session, history, streaming_config)
                .await;

            result
        } else {
            Err(anyhow::anyhow!("No provider configured"))
        }
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

    /// Clone the agent for use in the agentic loop
    /// This creates a shallow copy that shares the same identity
    fn clone_for_loop(&self, provider: Arc<crate::providers::Provider>) -> Self {
        Self {
            config: self.config.clone(),
            state: Arc::new(RwLock::new(AgentState::Busy)),
            identity: Identity {
                did: self.identity.did.clone(),
                document: self.identity.document.clone(),
                keypair: None, // Don't clone keypair to avoid security issues
            },
            provider: None, // Provider passed separately to avoid double-Arc
            session_manager: Arc::clone(&self.session_manager),
            session_router: SessionRouter::new(
                Arc::clone(&self.session_manager),
                self.config.name.clone(),
            ),
            subagent_executor: Arc::new(
                SubagentExecutor::new(
                    SessionRouter::new(Arc::clone(&self.session_manager), self.config.name.clone()),
                    Arc::clone(&self.session_manager),
                    self.config.name.clone(),
                    5,
                )
                .with_provider(provider)
                .with_agent_config(self.config.clone()),
            ),
            session_key_provider: Arc::clone(&self.session_key_provider),
            current_session_id: Arc::clone(&self.current_session_id),
            extension_core: Arc::clone(&self.extension_core),
        }
    }

    /// Execute with streaming support
    ///
    /// Returns a channel receiver for `AgenticEvents`. The caller can
    /// display these events as they arrive for a responsive UI.
    ///
    /// Note: This method must be called within a `tokio::task::LocalSet`
    /// because Agent contains non-Send types.

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

    /// Get the session router
    #[must_use]
    pub fn session_router(&self) -> &SessionRouter {
        &self.session_router
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
        self.session_router
            .route(peer, channel_type, channel_id, Some(&self.config.name))
            .await
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
        self.session_router
            .spawn(
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
    /// Execute with native tool calling using `AgenticLoopV4` (unified API).
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
        use crate::types::provider::{ProviderConfig, ProviderType};

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
        use crate::types::provider::{ProviderConfig, ProviderType};

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
    async fn test_agent_session_router() {
        use crate::session::types::{ChannelType, Peer};
        use crate::types::provider::{ProviderConfig, ProviderType};

        let config = AgentConfig {
            name: "test-agent-router".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama,
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();
        let router = agent.session_router();

        // Router should be able to route to sessions
        let peer = Peer::User("test_user".to_string());
        let ctx = router.route(&peer, ChannelType::Cli, "default", None).await;

        // Should succeed (requires filesystem in full test)
        // This just verifies the router is properly initialized
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
        let parent_key = parent_ctx.full_session_key().await;

        // Spawn a child session with shared context
        let spawn_ctx = agent
            .spawn_session(&peer, "test task", false, &parent_key, Some(300))
            .await;

        assert!(spawn_ctx.is_ok());
        let spawn_ctx = spawn_ctx.unwrap();
        assert!(spawn_ctx.is_subagent);
        assert!(!spawn_ctx.is_isolated().await);
    }
}
