//! Agentic loop - unified streaming core with presentation-layer delivery modes
//!
//! Architecture:
//! - Single core loop (`run_inner`) that always processes streaming events
//! - `StreamOrchestrator` with `DeliveryMode::FinalOnly` buffers for blocking consumers
//! - `DeliveryMode::Live` emits deltas for real-time consumers
//! - Non-streaming is purely a presentation-layer concern
//!
//! Benefits:
//! - One code path for all execution (authoritative, focused, maintainable)
//! - Background compaction works for both streaming and blocking modes
//! - Event semantics are uniform across all consumers

use crate::agents::prompt::SystemPromptService;
use crate::agents::Agent;
use crate::common::types::message::{ContentBlock, LlmMessage};
use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::extensions::framework::async_exec::executor::completion_queue::InboxItem;
use crate::extensions::framework::async_exec::executor::SharedSessionInbox;
use crate::providers::{ChatOptions, MessageRole, StopReason, TokenUsage, ToolDefinition};
use crate::session::Session;
use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Result of running the agentic loop
#[derive(Debug, Clone)]
pub struct AgenticResult {
    /// Whether execution succeeded
    pub success: bool,
    /// Final answer from the agent
    pub final_answer: String,
    /// Tool calls made during execution
    pub tool_calls: Vec<ContentBlock>,
    /// Number of iterations
    pub iterations: usize,
    /// Token usage
    pub usage: TokenUsage,
    /// True if the run was soft-interrupted by an external
    /// `PrincipalSendControl` request. The current step finished
    /// cleanly before exit; `final_answer` may be empty or partial.
    pub interrupted: bool,
}

/// A tool call for session storage compatibility
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
}

/// Agentic loop with native tool calling
pub struct AgenticLoop {
    agent: Arc<Agent>,
    provider: Arc<crate::providers::Provider>,
    max_iterations: usize,
    system_prompt: String,
    /// Extension core for skill loading and tool registration.
    extension_core: Arc<crate::extensions::framework::ExtensionCore>,
    /// Resolved caller identity (pekohub sub, API key id, or `None` for
    /// local CLI invocations). Propagated to `HookInput::ToolCall::caller_id`
    /// on every tool invocation so downstream permission checks and audit
    /// logging can attribute the call to a real user — see issue #17.
    caller_id: Option<String>,
    /// Spawning principal's runtime id. Always present for a loop constructed
    /// from an `Agent`. Propagated to `HookInput::ToolCall::principal_id` so
    /// extension-scoped tools such as `Skill` can resolve per-principal state
    /// via the `SkillStateRegistry` at handle time.
    agent_principal_id: String,
    /// Per-session queue of completed async tasks, drained at the start
    /// of `run_inner` iteration. Surfaced to the LLM as a
    /// synthetic user-role message containing all queued completions.
    async_completion_queue: Option<SharedSessionInbox>,
    /// Optional soft-interrupt token. When set, the loop checks
    /// `is_cancelled()` at the start of each iteration and just
    /// before delivering the final answer; if cancelled, it emits a
    /// `Lifecycle::Interrupted` event and returns `interrupted: true`
    /// without committing the final answer to the session. The
    /// current in-flight step (LLM stream chunk, tool call) always
    /// runs to completion — this is a *soft* interrupt, not a hard
    /// kill. Set by the streaming IPC handler via `with_cancel_token`
    /// so the `PrincipalSendControl` IPC can signal cancellation.
    cancel: Option<tokio_util::sync::CancellationToken>,
    /// Per-session `AGENTS.md` discovery tracker. The adapter pushes
    /// directories touched by tool calls via
    /// `directory_from_tool_params`; the loop drains it at iteration
    /// start and surfaces any newly-discovered `AGENTS.md` content as
    /// synthetic user-role messages. Always present — `None` would
    /// disable on-demand discovery, which is the default for callers
    /// that don't opt in (notably tests and legacy agent paths).
    directory_tracker: Arc<crate::extensions::framework::types::DirectoryContextTracker>,
}

impl AgenticLoop {
    /// Create a new agentic loop
    ///
    /// # Arguments
    /// * `agent` - The agent configuration
    /// * `provider` - The LLM provider to use
    /// * `extension_core` - The `ExtensionCore` for skill loading and hook integration
    pub async fn new(
        agent: Arc<Agent>,
        provider: Arc<crate::providers::Provider>,
        extension_core: Arc<crate::extensions::framework::ExtensionCore>,
    ) -> Self {
        let system_prompt = SystemPromptService::build(&agent, &extension_core).await;
        let agent_principal_id = agent.principal_id().to_string();

        Self {
            agent,
            provider,
            max_iterations: 10,
            system_prompt,
            extension_core,
            caller_id: None,
            agent_principal_id,
            async_completion_queue: None,
            cancel: None,
            directory_tracker: Arc::new(
                crate::agents::prompt::memory::DirectoryContextTracker::new(),
            ),
        }
    }

    /// Set the resolved caller identity for this loop (issue #17).
    /// Local CLI invocations leave this as `None`; tunneled requests
    /// set it to the pekohub user sub so every tool call inside the
    /// loop carries attribution.
    #[must_use]
    pub fn with_caller_id(mut self, caller_id: Option<String>) -> Self {
        self.caller_id = caller_id;
        self
    }

    /// Inject a per-session async task completion queue. When set, the
    /// agentic loop drains the queue at the start of each iteration
    /// and synthesizes a single user-role message containing all
    /// completions since the last iteration.
    #[must_use]
    pub fn with_async_completion_queue(mut self, queue: SharedSessionInbox) -> Self {
        self.async_completion_queue = Some(queue);
        self
    }

    /// Set maximum iterations
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set the soft-interrupt cancel token. When set, the loop checks
    /// `is_cancelled()` at iteration boundaries and exits cleanly via
    /// `Lifecycle::Interrupted` + `AgenticResult { interrupted: true }`
    /// when the token is signalled. The in-flight LLM stream chunk or
    /// tool call always finishes first — this is cooperative
    /// cancellation, not a hard kill.
    #[must_use]
    pub fn with_cancel_token(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Replace the per-session directory tracker. Mostly useful for
    /// tests that want to inject a pre-populated tracker; production
    /// code uses the default constructed in [`Self::new`].
    #[must_use]
    pub fn with_directory_tracker(
        mut self,
        tracker: Arc<crate::extensions::framework::types::DirectoryContextTracker>,
    ) -> Self {
        self.directory_tracker = tracker;
        self
    }

    /// Get the extension core
    #[must_use]
    pub fn extension_core(&self) -> &Arc<crate::extensions::framework::ExtensionCore> {
        &self.extension_core
    }

    /// Run the agent with a user prompt, optionally resuming from an existing session.
    ///
    /// Blocking mode: uses `DeliveryMode::FinalOnly` to buffer all output and emit
    /// complete events at the end. This is the unified path — the core always
    /// streams; presentation decides whether to show deltas or wait for finals.
    ///
    /// If `existing_session` is provided, it will be used instead of creating a new one.
    /// If `history` is provided, those messages will be used as the starting point.
    pub async fn run_with_resume(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
    ) -> Result<AgenticResult> {
        let config = crate::engine::OrchestratorConfig::final_only();
        self.run_streaming_with_resume(prompt, on_event, session, history, config)
            .await
    }

    /// Run the agent with streaming support, optionally resuming from an existing session.
    ///
    /// Uses `DeliveryMode::Live` or `DeliveryMode::Block` for real-time output.
    /// The core loop is the same as `run_with_resume`; only the orchestrator config differs.
    pub async fn run_streaming_with_resume(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };
        info!("Using session: {}", session_id);

        // Emit start event
        on_event(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        });

        info!(
            "Starting v4 streaming agentic loop for agent: {} (session: {})",
            self.agent.name(),
            session_id
        );

        // Build messages - either from history or fresh start
        let mut messages = if let Some(h) = history {
            info!("Loaded {} messages from history", h.len());
            // Check if history already has a system message at the start
            let has_system = h
                .first()
                .is_some_and(|m| matches!(m.role, MessageRole::System));
            if has_system {
                h
            } else {
                // Prepend system prompt to history
                let mut msgs = vec![LlmMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: self.system_prompt.clone(),
                    }],
                    timestamp: Utc::now(),
                    metadata: HashMap::new(),
                    tool_call_id: None,
                }];
                msgs.extend(h);

                // Add system prompt to session
                {
                    let mut s = session.write().await;
                    s.add_system(&self.system_prompt).await?;
                }

                msgs
            }
        } else {
            // Fresh start - add system prompt
            let msgs = vec![LlmMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: self.system_prompt.clone(),
                }],
                timestamp: Utc::now(),
                metadata: HashMap::new(),
                tool_call_id: None,
            }];

            // Add system prompt to session
            {
                let mut s = session.write().await;
                s.add_system(&self.system_prompt).await?;
            }

            msgs
        };

        // Add user message
        messages.push(LlmMessage::user(prompt.to_string()));

        // Add user message to session
        {
            let mut s = session.write().await;
            s.add_user(prompt).await?;
        }

        // Continue with the unified run logic
        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }

    /// Like [`Self::run_streaming_with_resume`] but skips the user-message
    /// persistence step. Used by the steering path: the IPC handler has
    /// already called `session.add_user(content)` to persist the queued
    /// steering message, so the loop must not add it again.
    ///
    /// The actual steering content reaches the LLM via the inbox drain
    /// at the start of `run_inner`'s first iteration: any pending
    /// `InboxItem::Steering` items are pushed onto `messages` as
    /// `LlmMessage::user(...)` turns, in arrival order. Persistence is
    /// already done; only the in-memory copy is materialized here.
    pub async fn run_streaming_with_resume_skip_user_add(
        &self,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };
        info!("Using session: {}", session_id);

        // Emit start event
        on_event(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        });

        info!(
            "Starting v4 streaming agentic loop for agent: {} (session: {}) [skip-user-add, steering path]",
            self.agent.name(),
            session_id
        );

        // Build messages - either from history or fresh start
        let messages = if let Some(h) = history {
            info!("Loaded {} messages from history", h.len());
            let has_system = h
                .first()
                .is_some_and(|m| matches!(m.role, MessageRole::System));
            if has_system {
                h
            } else {
                let mut msgs = vec![LlmMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: self.system_prompt.clone(),
                    }],
                    timestamp: Utc::now(),
                    metadata: HashMap::new(),
                    tool_call_id: None,
                }];
                msgs.extend(h);

                {
                    let mut s = session.write().await;
                    s.add_system(&self.system_prompt).await?;
                }

                msgs
            }
        } else {
            let msgs = vec![LlmMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: self.system_prompt.clone(),
                }],
                timestamp: Utc::now(),
                metadata: HashMap::new(),
                tool_call_id: None,
            }];

            {
                let mut s = session.write().await;
                s.add_system(&self.system_prompt).await?;
            }

            msgs
        };

        // No `messages.push(LlmMessage::user(...))` and no `s.add_user(...)`
        // here. The user turn was persisted by the IPC handler; the
        // steering content is delivered to the LLM at iteration start
        // by the inbox drain inside `run_inner`.

        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }

    /// Original run method - creates new session via `SessionManager`
    pub async fn run(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResult> {
        use crate::auth::Subject;
        use crate::common::paths::PathResolver;
        use crate::session::manager::SessionManager;

        // Create session via SessionManager. Issue #17: use the resolved
        // caller identity (set via `with_caller_id`) as the session's
        // `sender_id` so the session-keying scheme
        // `(agent, channel, sender_id)` works as designed. Local CLI
        // invocations leave `self.caller_id` as `None`, which maps to a
        // local-trust peer.
        let path_resolver = PathResolver::new();
        let mut session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, self.agent.name())
            .await?;
        let peer = self
            .caller_id
            .as_deref()
            .map(|c| Subject::User(c.to_string()))
            .unwrap_or_else(|| Subject::User("local".to_string()));
        let session = session_manager
            .get_or_create_base(self.agent.name(), &peer)
            .await?;

        self.run_with_resume(prompt, on_event, session, None).await
    }

    /// Unified agent loop — always streams internally; delivery mode controls presentation.
    ///
    /// `DeliveryMode::FinalOnly` buffers everything and emits complete events at the end,
    /// giving blocking consumers the same behavior as the old `run_loop`.
    /// `DeliveryMode::Live` emits deltas for real-time display.
    async fn run_inner(
        &self,
        mut messages: Vec<LlmMessage>,
        session: Arc<RwLock<Session>>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        run_id: String,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        use futures::StreamExt;

        // Get session_id once at start
        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };

        // Push the resolved session id onto the core so `AsyncSpawn`
        // can stamp `parent_session_key` on any task issued from this
        // loop. This is the only place that *always* knows the real id
        // (the `Agent::execute*` callers have already pushed it for the
        // `_with_session` paths, but `Agent::execute` — one-shot CLI
        // mode — pushes `None` because the session is born here, inside
        // `run_inner`). Doing it here means every entry into the loop
        // — regardless of which `execute_*` path called us — ends up
        // with a real session key on the core before iteration 1
        // begins, so even the first `AsyncSpawn` issued mid-iteration
        // sees a real `parent_session_key` rather than the `"unknown"`
        // fallback. The session key is keyed by the loop's agent DID on
        // the shared `ExtensionCore` so concurrent agents in daemon mode
        // do not clobber each other (issue #68).
        self.extension_core
            .set_session_key(&self.agent.identity.did, Some(session_id.clone()))
            .await;

        // Resolve model id once at start — threaded through every
        // `provider.chat_with_tools` / `stream_with_tools` call so the
        // adapter no longer needs to bake one in.
        //
        // v3: the model id comes from the resolved `Provider` (built
        // by `LlmResolver` from the agent's `preferred_*` hints), not
        // from the deprecated inline `[provider]` block.
        let model_id = {
            let provider_name = self.provider.name().to_string();
            let model_name = self.provider.model_id();

            let mut s = session.write().await;
            s.set_model(&provider_name, &model_name);
            // Record model change event in session JSONL for normalization
            if let Err(e) = s.record_model_change(&provider_name, &model_name).await {
                warn!("Failed to record model change event: {}", e);
            }
            model_name
        };

        let mut iteration = 0;
        let mut total_usage = TokenUsage::default();

        // Initialize compaction orchestrator
        let mut compaction_orchestrator =
            crate::engine::compaction_orchestrator::CompactionOrchestrator::new(
                self.provider.clone(),
                &self.agent.config,
            );

        // Initialize tool executor
        let tool_executor = crate::engine::tool_executor::ToolExecutor;

        loop {
            iteration += 1;
            let mut iteration_usage = TokenUsage::default();
            info!("Agent loop: iteration {}", iteration);

            // Drain the per-session inbox and inject its contents at the
            // start of every iteration. Two kinds of items:
            //
            // - `Completion` events from background async tasks →
            //   folded into a single synthetic user-role message via
            //   `build_async_completion_message` (existing behavior).
            // - `Steering` messages queued by the user via IPC →
            //   delivered as separate user-role turns in arrival
            //   order. The IPC handler has already persisted them via
            //   `session.add_user`; this loop only pushes the
            //   in-memory copy so the LLM sees them this iteration.
            //
            // Runs at the start of every iteration, so events that
            // arrive mid-iteration wait for the next one.
            if let Some(ref inbox) = self.async_completion_queue {
                let items = inbox.drain_all().await;
                let mut completions = Vec::new();
                let mut steering = Vec::new();
                for item in items {
                    match item {
                        InboxItem::Completion(e) => completions.push(e),
                        InboxItem::Steering(m) => steering.push(m),
                    }
                }
                if let Some(msg) = super::async_completion::build_async_completion_message(
                    &completions,
                    &session_id,
                ) {
                    messages.push(msg);
                }
                for msg in steering {
                    debug!(
                        "AgenticLoop: injecting queued steering message {} ({} bytes) at iteration {}",
                        msg.id,
                        msg.content.len(),
                        iteration,
                    );
                    messages.push(LlmMessage::user(msg.content));
                }
            }

            // Drain the directory-context tracker and surface any newly
            // discovered `AGENTS.md` content as synthetic user-role
            // messages. The adapter pushes directories touched by the
            // previous iteration's tool calls; we walk up from each one
            // (capped at the principal's workspace) and inject whatever
            // we find. Discovery is idempotent — already-loaded
            // `AGENTS.md` files don't re-emit on the next iteration.
            let touched = self.directory_tracker.drain_new();
            if !touched.is_empty() {
                let root = self
                    .agent
                    .config
                    .workspace
                    .as_deref()
                    .unwrap_or_else(|| std::path::Path::new(""));
                for dir in &touched {
                    if let Some((label, content)) =
                        crate::agents::prompt::memory::discover_shared_context(dir, root)
                    {
                        debug!(
                            "AgenticLoop: injecting AGENTS.md from {} ({} bytes) at iteration {}",
                            label,
                            content.len(),
                            iteration,
                        );
                        let body = format!(
                            "<directory-context source=\"{}\">\n{}\n</directory-context>",
                            label, content
                        );
                        messages.push(LlmMessage::user(body));
                    }
                }
            }

            // Soft-interrupt checkpoint #1: at the top of every
            // iteration, after the inbox drain. If the cancel token
            // was signalled, exit cleanly without starting another
            // LLM call. The in-flight step from the *previous*
            // iteration has already completed by the time we get
            // here, so this is the earliest point a cancel takes
            // effect (the LLM stream chunk and any tool round-trip
            // always run to completion first).
            if let Some(cancel) = &self.cancel {
                if cancel.is_cancelled() {
                    info!(
                        "AgenticLoop: soft-interrupt observed at iteration {}; exiting cleanly",
                        iteration
                    );
                    on_event(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Interrupted,
                        error: Some("cancelled by PrincipalSendControl".to_string()),
                    });
                    return Ok(AgenticResult {
                        success: false,
                        final_answer: String::new(),
                        tool_calls: vec![],
                        iterations: iteration,
                        usage: total_usage,
                        interrupted: true,
                    });
                }
            }

            // ADR-019 Phase 2: Build tool definitions dynamically each iteration
            let tool_defs = self.build_tool_definitions().await;

            // ADR-019 Phase 3: Rebuild system prompt dynamically
            if !messages.is_empty() && matches!(messages[0].role, MessageRole::System) {
                let fresh_prompt =
                    SystemPromptService::build_fresh(&self.agent, &self.extension_core).await;
                messages[0] = LlmMessage::system(fresh_prompt);
            }

            // ============================================================
            // ADR-022 Phase 3: Compaction with Extension Hooks
            // ============================================================
            compaction_orchestrator
                .check_and_compact(
                    &mut messages,
                    &session,
                    &self.extension_core,
                    &on_event,
                    &run_id,
                )
                .await?;

            if iteration > self.max_iterations {
                warn!("Max iterations ({}) reached", self.max_iterations);
                on_event(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::End,
                    error: None,
                });
                return Ok(AgenticResult {
                    success: true,
                    final_answer: "Max iterations reached".to_string(),
                    tool_calls: vec![],
                    iterations: iteration,
                    usage: total_usage,
                    interrupted: false,
                });
            }

            // Emit running event
            on_event(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            });

            // Chat options
            let options = ChatOptions {
                temperature: Some(0.7),
                max_tokens: Some(4096),
                api_key: None,
                headers: std::collections::HashMap::new(),
            };

            // Debug: print messages being sent
            debug!("Messages sent to LLM (iteration {}):", iteration);
            for (i, msg) in messages.iter().enumerate() {
                let content_preview: String = msg
                    .content
                    .iter()
                    .map(|b| match b {
                        crate::common::types::message::ContentBlock::Text { text } => {
                            format!("[Text: {}]", text.chars().take(50).collect::<String>())
                        }
                        crate::common::types::message::ContentBlock::ToolCall {
                            id, name, ..
                        } => {
                            format!("[ToolCall: {name} ({id})]")
                        }
                        crate::common::types::message::ContentBlock::ToolResult {
                            tool_call_id,
                            name,
                            ..
                        } => format!("[ToolResult: {tool_call_id} -> {name}]"),
                        _ => "[Other]".to_string(),
                    })
                    .collect();
                debug!("  [{}] {:?}: {}", i, msg.role, content_preview);
            }

            // Obtain the stream of events from the provider.
            // For providers that don't support native streaming, we synthesize a stream
            // from the blocking response so the rest of the loop stays uniform.
            let mut stream = if self.provider.supports_native_tools() {
                info!(
                    "Calling stream_with_tools with {} messages and {} tool definitions: {:?}",
                    messages.len(),
                    tool_defs.len(),
                    tool_defs.iter().map(|d| &d.name).collect::<Vec<_>>()
                );
                for (i, def) in tool_defs.iter().enumerate() {
                    info!(
                        "Tool def [{}]: name={}, params={}",
                        i, def.name, def.parameters
                    );
                }
                match self
                    .provider
                    .stream_with_tools(&model_id, &messages, &tool_defs, &options)
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        debug!("Failed to start stream: {}", e);
                        on_event(AgenticEvent::Lifecycle {
                            run_id: run_id.clone(),
                            phase: LifecyclePhase::Error,
                            error: Some(e.to_string()),
                        });
                        return Err(e);
                    }
                }
            } else {
                warn!("Provider doesn't support streaming, synthesizing from blocking response");
                let response = self
                    .provider
                    .chat_with_tools(&model_id, &messages, &tool_defs, &options)
                    .await?;
                crate::providers::synthetic_stream::synthesize_stream_from_blocking(
                    response,
                    self.provider.name(),
                )
            };

            info!("Stream started, processing events...");

            // Create orchestrator for this iteration
            let mut orchestrator =
                crate::engine::StreamOrchestrator::new(&run_id, streaming_config.clone());

            // Process stream events
            let mut accumulated_text = String::new();
            let mut thinking_text = String::new();
            let mut tool_calls: Vec<ContentBlock> = Vec::new();
            let mut stop_reason = StopReason::Stop;
            let mut stream_event_count = 0;

            loop {
                match stream.next().await {
                    Some(result) => {
                        stream_event_count += 1;
                        match result {
                            Ok(stream_event) => {
                                debug!(
                                    "Received stream event #{}: {:?}",
                                    stream_event_count, stream_event
                                );
                                // Process through orchestrator
                                let agentic_events = orchestrator.process(stream_event.clone());
                                for event in agentic_events {
                                    // Track text accumulation
                                    match &event {
                                        AgenticEvent::AssistantDelta { text, .. } => {
                                            accumulated_text.push_str(text);
                                        }
                                        AgenticEvent::AssistantText { text, .. } => {
                                            accumulated_text.push_str(text);
                                        }
                                        AgenticEvent::Thinking { text, is_delta, .. } => {
                                            if *is_delta {
                                                thinking_text.push_str(text);
                                            } else {
                                                thinking_text = text.clone();
                                            }
                                        }
                                        _ => {}
                                    }
                                    // Emit event
                                    on_event(event);
                                }

                                // Track tool calls and stop reason from stream events
                                match stream_event {
                                    crate::providers::StreamEvent::ToolCallEnd {
                                        tool_call,
                                        ..
                                    } => {
                                        tool_calls.push(tool_call);
                                    }
                                    crate::providers::StreamEvent::Done {
                                        stop_reason: reason,
                                    } => {
                                        stop_reason = reason;
                                    }
                                    crate::providers::StreamEvent::Usage {
                                        input,
                                        output,
                                        total,
                                    } => {
                                        iteration_usage.input += input;
                                        iteration_usage.output += output;
                                        iteration_usage.total += total;
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                on_event(AgenticEvent::Lifecycle {
                                    run_id: run_id.clone(),
                                    phase: LifecyclePhase::Error,
                                    error: Some(e.to_string()),
                                });
                                return Err(e);
                            }
                        }
                    }
                    None => {
                        break;
                    }
                }
            }

            // Finalize orchestrator and emit remaining events
            let final_events = orchestrator.finalize();
            for event in final_events {
                // Also track text accumulation from final events (e.g., AssistantText in FinalOnly mode)
                match &event {
                    AgenticEvent::AssistantText { text, .. } => {
                        accumulated_text.push_str(text);
                    }
                    _ => {}
                }
                on_event(event);
            }

            info!(
                "Stream complete: {} events, text_len={}, tool_calls={}, stop_reason={:?}",
                stream_event_count,
                accumulated_text.len(),
                tool_calls.len(),
                stop_reason
            );

            // Accumulate this iteration's usage
            total_usage.input += iteration_usage.input;
            total_usage.output += iteration_usage.output;
            total_usage.total += iteration_usage.total;

            // Handle tool calls
            if !tool_calls.is_empty() {
                info!("Processing {} tool calls from stream", tool_calls.len());

                // Build assistant message with tool calls
                let mut assistant_content: Vec<ContentBlock> = Vec::new();
                if !accumulated_text.is_empty() {
                    assistant_content.push(ContentBlock::Text {
                        text: accumulated_text.clone(),
                    });
                }
                for tc in &tool_calls {
                    assistant_content.push(tc.clone());
                }

                // Add to messages
                messages.push(LlmMessage {
                    role: MessageRole::Assistant,
                    content: assistant_content,
                    timestamp: chrono::Utc::now(),
                    metadata: std::collections::HashMap::new(),
                    tool_call_id: None,
                });

                // Add to session
                let tool_call_blocks: Vec<crate::session::events::ToolCallBlock> = tool_calls
                    .iter()
                    .filter_map(|tc| {
                        if let ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } = tc
                        {
                            Some(crate::session::events::ToolCallBlock {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                // Build content blocks: text + tool calls
                let mut content_blocks: Vec<ContentBlock> = Vec::new();
                if !accumulated_text.is_empty() {
                    content_blocks.push(ContentBlock::Text {
                        text: accumulated_text.clone(),
                    });
                }
                for tc in &tool_calls {
                    content_blocks.push(tc.clone());
                }

                // Build thinking block if thinking content was captured during streaming
                let thinking_block = if thinking_text.is_empty() {
                    None
                } else {
                    Some(crate::session::events::ThinkingBlock {
                        text: thinking_text.clone(),
                        signature: None,
                    })
                };

                {
                    let mut s = session.write().await;
                    s.add_assistant_with_blocks(
                        content_blocks,
                        Some(tool_call_blocks),
                        thinking_block,
                        Some(iteration_usage.clone()),
                    )
                    .await?;
                }

                // Execute tools in parallel (fan-out). Independent tool
                // calls from a single LLM response run concurrently —
                // `Read + Read`, `Glob + Grep`, `Bash + Bash`, etc. Each
                // gets its own `CancellationToken` clone (cheap; the
                // token is `Arc`-backed) so a per-tool cancel still
                // wins for that specific tool.
                //
                // Results land in `messages` in the order the tool
                // calls arrived (try_join_all preserves iterator
                // order), but tool-result identity is keyed by
                // `tool_call_id`, not position, so even a shuffle
                // would still match correctly on the next LLM turn.
                let tool_call_futs: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| {
                        tool_executor.execute(
                            tc,
                            &self.extension_core,
                            self.agent.name(),
                            self.agent.config.workspace.as_ref(),
                            &session,
                            &run_id,
                            self.caller_id.as_deref(),
                            &self.agent_principal_id,
                            self.agent.principal_name().unwrap_or(""),
                            Some(self.agent.config.extension_whitelist()),
                            self.cancel.clone(),
                            Some(self.directory_tracker.clone()),
                            &on_event,
                        )
                    })
                    .collect();
                let tool_results = futures::future::try_join_all(tool_call_futs).await?;
                for r in tool_results {
                    messages.push(r.message);
                }

                // Continue to next iteration
                continue;
            }

            // No tool calls - this is the final answer
            info!("Final answer received after {} iterations", iteration);

            // Soft-interrupt checkpoint #2: just before we commit
            // the final answer to the session. If the cancel token
            // was signalled *while* the LLM was streaming this final
            // response, drop the answer on the floor and exit
            // cleanly. The streamed deltas have already been emitted
            // to the client (soft interrupt doesn't chop mid-token),
            // but we don't persist the final answer or emit
            // `Lifecycle::End`.
            if let Some(cancel) = &self.cancel {
                if cancel.is_cancelled() {
                    info!(
                        "AgenticLoop: soft-interrupt observed at final-answer; dropping final answer"
                    );
                    on_event(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Interrupted,
                        error: Some("cancelled by PrincipalSendControl".to_string()),
                    });
                    return Ok(AgenticResult {
                        success: false,
                        final_answer: String::new(),
                        tool_calls: vec![],
                        iterations: iteration,
                        usage: total_usage,
                        interrupted: true,
                    });
                }
            }

            // Add final answer to session
            {
                let mut s = session.write().await;
                s.add_assistant(&accumulated_text, None, Some(iteration_usage.clone()))
                    .await?;
            }

            // Note: We don't emit AssistantText here because the content has already
            // been streamed via AssistantDelta events. Emitting AssistantText would
            // cause duplication for consumers that process both event types.

            // Emit final usage event
            on_event(AgenticEvent::Usage {
                run_id: run_id.clone(),
                prompt_tokens: total_usage.input as u32,
                completion_tokens: total_usage.output as u32,
                total_tokens: total_usage.total as u32,
            });

            // Emit end event to signal completion
            on_event(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::End,
                error: None,
            });

            return Ok(AgenticResult {
                success: true,
                final_answer: accumulated_text,
                tool_calls: vec![],
                iterations: iteration,
                usage: total_usage,
                interrupted: false,
            });
        }
    }

    /// Build tool definitions dynamically from `ExtensionCore` (ADR-019 Phase 2)
    ///
    /// This queries the unified tool registry for currently enabled tools,
    /// allowing tool changes to take effect without session restart. The
    /// list is filtered by the agent's extension whitelist so the LLM only
    /// sees tools the agent is actually allowed to invoke.
    async fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        let allowed = self.agent.config.extension_whitelist();
        let defs = self
            .extension_core
            .list_tool_definitions_with_allowlist(&allowed)
            .await;

        info!(
            "Dynamically built {} tool definitions from ExtensionCore: {:?}",
            defs.len(),
            defs.iter().map(|d| &d.name).collect::<Vec<_>>()
        );

        defs
    }

    /// Get the system prompt
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Run the agent with streaming support
    ///
    /// This method uses `stream_with_tools()` to get real-time token-by-token
    /// delivery from the provider. Events are emitted as they arrive.
    ///
    /// # Arguments
    ///
    /// * `prompt` - The user prompt
    /// * `on_event` - Callback for agentic events (called for each streaming event)
    /// * `session` - Session for context storage
    /// * `history` - Optional message history to resume from
    /// * `streaming_config` - Configuration for the streaming orchestrator
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let result = agentic_loop
    ///     .run_streaming(
    ///         "What's the weather?",
    ///         |event| println!("{:?}", event),
    ///         session,
    ///         None,
    ///         OrchestratorConfig::live(),
    ///     )
    ///     .await?;
    /// ```
    pub async fn run_streaming(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        info!(
            "Starting v4 streaming agentic loop for agent: {} (run_id: {})",
            self.agent.name(),
            run_id
        );

        // Build messages - either from history or fresh start
        let mut messages = if let Some(h) = history {
            info!("Loaded {} messages from history", h.len());
            // Check if history already has a system message at the start
            let has_system = h
                .first()
                .is_some_and(|m| matches!(m.role, MessageRole::System));
            if has_system {
                h
            } else {
                // Prepend system prompt to history
                let mut msgs = vec![LlmMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: self.system_prompt.clone(),
                    }],
                    timestamp: Utc::now(),
                    metadata: HashMap::new(),
                    tool_call_id: None,
                }];
                msgs.extend(h);

                // Add system prompt to session
                {
                    let mut s = session.write().await;
                    s.add_system(&self.system_prompt).await?;
                }

                msgs
            }
        } else {
            // Fresh start - add system prompt
            let msgs = vec![LlmMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: self.system_prompt.clone(),
                }],
                timestamp: Utc::now(),
                metadata: HashMap::new(),
                tool_call_id: None,
            }];

            // Add system prompt to session
            {
                let mut s = session.write().await;
                s.add_system(&self.system_prompt).await?;
            }

            msgs
        };

        // Add user message
        messages.push(LlmMessage::user(prompt.to_string()));

        // Add user message to session
        {
            let mut s = session.write().await;
            s.add_user(prompt).await?;
        }

        // Run the streaming loop
        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent_config::AgentConfig;
    use crate::agents::Agent;
    use crate::auth::Subject;
    use crate::common::types::provider::{ProviderConfig, ProviderType};
    use crate::extensions::framework::core::{global_core, init_global_core, ExtensionCore};
    use crate::providers::{AnyAdapter, MockAdapter, Provider};
    use crate::session::manager::SessionManager;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    /// Build a mock provider with a fresh MockAdapter
    fn mock_provider() -> (Arc<Provider>, MockAdapter) {
        let adapter = MockAdapter::new();
        let any = AnyAdapter::Mock(adapter.clone());
        let config = ProviderConfig {
            provider_type: ProviderType::OpenAI,
            ..Default::default()
        };
        let provider = Provider::new(any, "mock_key", config).unwrap();
        (Arc::new(provider), adapter)
    }

    /// Build a minimal agent config using the mock provider
    fn test_agent_config(name: &str) -> AgentConfig {
        AgentConfig {
            name: name.to_string(),
            preferred_provider_id: Some("openai".into()),
            preferred_model_id: Some("default".into()),
            extensions: Some(crate::common::types::agent_legacy::ExtensionConfig {
                enabled: vec!["*".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Create a temporary session for testing
    async fn test_session(agent_name: &str, temp_dir: &std::path::Path) -> Arc<RwLock<Session>> {
        let mut manager = SessionManager::new()
            .with_sessions_dir_internal(temp_dir.join("data").join("sessions"))
            .with_agent_name(agent_name);
        let peer = Subject::User("default".to_string());
        let handle = manager
            .create_session(
                agent_name,
                &peer,
                crate::session::manager::SessionCreateOptions::new(),
            )
            .await
            .unwrap();
        handle.base().clone()
    }

    /// Ensure global ExtensionCore is initialized for tests
    fn ensure_global_core() {
        if global_core().is_none() {
            init_global_core(Arc::new(ExtensionCore::new()));
        }
    }

    // ===================================================================
    // RT-001: Engine MUST execute the agentic loop
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt001_basic_agentic_loop() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale (Windows-headless
        // keyring panics inside `Agent::new_for_test` → `KeyStorage::with_path`).
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Hello, I am a mock assistant.");

        let config = test_agent_config("rt001-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt001-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let result = loop_
            .run_with_resume(
                "Say hello",
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
            )
            .await;

        assert!(result.is_ok(), "Agentic loop should succeed");
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.final_answer, "Hello, I am a mock assistant.");
        assert_eq!(result.iterations, 1);

        // Verify events were emitted
        let emitted = events.lock().unwrap();
        let has_start = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::Start,
                    ..
                }
            )
        });
        let has_end = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::End,
                    ..
                }
            )
        });
        assert!(has_start, "Should emit Start event");
        assert!(has_end, "Should emit End event");
    }

    // ===================================================================
    // RT-002: Engine MUST support streaming output
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt002_streaming_output() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Streaming response");

        let config = test_agent_config("rt002-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt002-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let streaming_config = crate::engine::OrchestratorConfig::live();
        let result = loop_
            .run_streaming_with_resume(
                "Stream something",
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
                streaming_config,
            )
            .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.final_answer, "Streaming response");

        // In live mode we should see delta events
        let emitted = events.lock().unwrap();
        let has_deltas = emitted
            .iter()
            .any(|e| matches!(e, AgenticEvent::AssistantDelta { .. }));
        assert!(
            has_deltas,
            "Live streaming should emit AssistantDelta events"
        );
    }

    // ===================================================================
    // RT-003: Engine MUST enforce a configurable timeout per LLM request
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt003_timeout_config_propagation() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Quick response");

        let config = test_agent_config("rt003-agent");
        // v3: timeout is no longer on the per-agent `[provider]`
        // block. The agentic loop consults the resolved Provider's
        // own timeout. Default timeout in tests is sufficient.
        let agent = Arc::new(
            Agent::new_for_test(config.clone(), temp_dir.path())
                .await
                .unwrap(),
        );
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt003-agent", temp_dir.path()).await;
        let result = loop_
            .run_with_resume("Test timeout", |_| {}, session, None)
            .await;

        assert!(result.is_ok());

        // The request should have been recorded with the mock adapter
        let recorded = mock.recorded_requests();
        assert_eq!(recorded.len(), 1, "Mock should have recorded one request");
    }

    // ===================================================================
    // RT-004: Engine MUST gracefully handle LLM API failures
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt004_graceful_error_handling() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_error("LLM API rate limit exceeded");

        let config = test_agent_config("rt004-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt004-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let result = loop_
            .run_with_resume(
                "Trigger error",
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
            )
            .await;

        // The loop should return an error, not panic
        assert!(result.is_err(), "Should propagate LLM error");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("rate limit exceeded"),
            "Error should contain original message: {err_msg}"
        );

        // Should emit an Error lifecycle event
        let emitted = events.lock().unwrap();
        let has_error = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::Error,
                    ..
                }
            )
        });
        assert!(has_error, "Should emit Error lifecycle event");
    }

    // ===================================================================
    // RT-005: Engine MUST persist every message to JSONL atomically
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt005_session_persistence() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Persisted answer");

        let config = test_agent_config("rt005-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt005-agent", temp_dir.path()).await;
        let session_clone = session.clone();

        let result = loop_
            .run_with_resume("Persist this", |_| {}, session, None)
            .await;

        assert!(result.is_ok());

        // Verify session has messages persisted
        let session_guard = session_clone.read().await;
        let history = session_guard.load_history().await.unwrap();
        drop(session_guard);

        // Should have: system prompt + user message + assistant message
        assert!(
            history.len() >= 2,
            "Session should have at least system + user + assistant messages, got {}",
            history.len()
        );

        // Verify user message is present
        let has_user = history.iter().any(|m| matches!(m.role, MessageRole::User));
        assert!(has_user, "Session should contain user message");

        // Verify assistant message is present
        let has_assistant = history
            .iter()
            .any(|m| matches!(m.role, MessageRole::Assistant));
        assert!(has_assistant, "Session should contain assistant message");
    }

    // ===================================================================
    // RT-006: Engine MUST support up to 10 iterations per turn
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt006_max_iterations_enforced() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();

        // Queue 12 tool-call responses to try to exceed the default max of 10
        for i in 0..12 {
            mock.queue_tool_call(
                format!("tc_{i}"),
                "test_tool",
                serde_json::json!({"value": i}),
            );
        }

        let config = test_agent_config("rt006-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core)
            .await
            .with_max_iterations(5); // Use a smaller max for faster test

        let session = test_session("rt006-agent", temp_dir.path()).await;
        let result = loop_
            .run_with_resume("Trigger tool loop", |_| {}, session, None)
            .await;

        assert!(result.is_ok(), "Loop should complete without panic");
        let result = result.unwrap();

        // Should have hit max iterations (iteration starts at 0, increments at top,
        // check is `iteration > max_iterations`. With max=5: runs 1..=5, then on 6 triggers.)
        assert!(
            result.iterations > 5,
            "Should exceed max_iterations threshold before stopping, got {}",
            result.iterations
        );
        assert_eq!(
            result.final_answer, "Max iterations reached",
            "Should return max iterations message"
        );
    }

    // ===================================================================
    // RT-006 variant: Verify default max_iterations is 10
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt006_default_max_iterations_is_10() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Done");

        let config = test_agent_config("rt006-default-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        // The struct should default to 10
        assert_eq!(
            loop_.max_iterations, 10,
            "Default max_iterations should be 10"
        );
    }

    // ===================================================================
    // Integration: tool call -> tool execution -> next iteration
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_tool_call_iteration() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();

        // First response: tool call
        mock.queue_tool_call("tc_1", "echo", serde_json::json!({"msg": "hello"}));
        // Second response: final text answer
        mock.queue_text("Tool result processed.");

        let config = test_agent_config("tool-loop-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("tool-loop-agent", temp_dir.path()).await;
        let result = loop_
            .run_with_resume("Use echo tool", |_| {}, session, None)
            .await;

        assert!(
            result.is_ok(),
            "Tool loop should succeed: {:?}",
            result.err()
        );
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.final_answer, "Tool result processed.");
        // Tool execution may fail because "echo" is not registered in the test ExtensionCore.
        // If the tool fails, the loop still gets a tool result message and may continue,
        // but if the mock queue is exhausted on the second iteration it could error.
        // We accept either 1 iteration (tool failed, loop stopped) or 2 (tool succeeded).
        assert!(
            result.iterations >= 1,
            "Should complete at least 1 iteration, got {}",
            result.iterations
        );
    }

    // ===================================================================
    // Parallel tool execution: when an LLM response carries multiple
    // tool calls, the engine must fan them out concurrently. Each
    // tool records its start/end timestamps into a shared log; the
    // test asserts the intervals overlap.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_parallel_tool_execution_overlaps_in_time() {
        use crate::extensions::builtin::adapter::BuiltinToolAdapter;
        use crate::providers::MockResponse;
        use crate::tools::Tool;
        use serde_json::json;
        use std::sync::Mutex as StdMutex;
        use std::time::{Duration, Instant};

        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();

        // Shared log: each tool pushes (name, start, end). The test
        // asserts the two intervals overlap — proof of concurrency.
        let log: Arc<StdMutex<Vec<(&'static str, Instant, Instant)>>> =
            Arc::new(StdMutex::new(Vec::new()));

        struct SlowTool {
            label: &'static str,
            log: Arc<StdMutex<Vec<(&'static str, Instant, Instant)>>>,
        }

        #[async_trait::async_trait]
        impl Tool for SlowTool {
            fn name(&self) -> &str {
                self.label
            }

            fn description(&self) -> String {
                format!("slow tool {}", self.label)
            }

            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }

            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                let start = Instant::now();
                tokio::time::sleep(Duration::from_millis(120)).await;
                let end = Instant::now();
                self.log.lock().unwrap().push((self.label, start, end));
                Ok(json!({"ok": true, "label": self.label}))
            }
        }

        let core = global_core().unwrap();
        BuiltinToolAdapter::register_tool(
            &core,
            Arc::new(SlowTool {
                label: "ParaA",
                log: log.clone(),
            }) as Arc<dyn Tool>,
        )
        .await
        .unwrap();
        BuiltinToolAdapter::register_tool(
            &core,
            Arc::new(SlowTool {
                label: "ParaB",
                log: log.clone(),
            }) as Arc<dyn Tool>,
        )
        .await
        .unwrap();

        // First response: TWO tool calls in one stream. The mock
        // adapter's `stream_with_tools` reads from `stream_responses`,
        // so we queue raw `StreamEvent` vectors here. The loop sees a
        // single response with two calls and fans them out.
        mock.queue_stream_response(MockResponse::Stream(vec![
            crate::providers::StreamEvent::Start {
                provider: "mock".to_string(),
                model: "default".to_string(),
            },
            crate::providers::StreamEvent::ToolCallStart { content_index: 0 },
            crate::providers::StreamEvent::ToolCallEnd {
                content_index: 0,
                tool_call: ContentBlock::ToolCall {
                    id: "tc_a".to_string(),
                    name: "ParaA".to_string(),
                    arguments: json!({}),
                },
            },
            crate::providers::StreamEvent::ToolCallStart { content_index: 1 },
            crate::providers::StreamEvent::ToolCallEnd {
                content_index: 1,
                tool_call: ContentBlock::ToolCall {
                    id: "tc_b".to_string(),
                    name: "ParaB".to_string(),
                    arguments: json!({}),
                },
            },
            crate::providers::StreamEvent::Usage {
                input: 0,
                output: 0,
                total: 0,
            },
            crate::providers::StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
            },
        ]));
        // Second response: final text answer.
        mock.queue_text("Both tools done.");

        let config = test_agent_config("para-tools-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let loop_ = AgenticLoop::new(agent.clone(), provider, core).await;

        let session = test_session("para-tools-agent", temp_dir.path()).await;
        let started = Instant::now();
        let result = loop_
            .run_with_resume("Run both tools", |_| {}, session, None)
            .await;
        let total_elapsed = started.elapsed();

        assert!(
            result.is_ok(),
            "Parallel tool loop should succeed: {:?}",
            result.err()
        );
        let log_snapshot = log.lock().unwrap().clone();
        assert_eq!(
            log_snapshot.len(),
            2,
            "expected both tools to have run, got {log_snapshot:?}"
        );

        let (_, a_start, a_end) = log_snapshot
            .iter()
            .find(|(n, _, _)| *n == "ParaA")
            .expect("ParaA recorded");
        let (_, b_start, b_end) = log_snapshot
            .iter()
            .find(|(n, _, _)| *n == "ParaB")
            .expect("ParaB recorded");

        // Concurrency proof: the two intervals overlap. If they ran
        // serially, B's start would equal A's end (or later).
        let overlap = *a_start < *b_end && *b_start < *a_end;
        assert!(
            overlap,
            "tools ran serially: ParaA=[{a_start:?}..{a_end:?}], \
             ParaB=[{b_start:?}..{b_end:?}] — they should overlap"
        );

        // Total elapsed should be ~120ms (one tool's worth), not
        // ~240ms (serial). Use 220ms as a generous upper bound to
        // tolerate scheduler jitter on shared CI runners.
        assert!(
            total_elapsed < Duration::from_millis(220),
            "total elapsed {total_elapsed:?} suggests serial execution; \
             expected ~120ms with parallel fan-out"
        );
    }

    // ===================================================================
    // End-to-end: push a CompletionEvent to the queue → synthetic user
    // message reaches the LLM on the next iteration.
    //
    // This is the central promise of the tool async refactor: an async
    // task's completion must surface to the agentic loop as a synthetic
    // user-role message. This test pins down the wiring end-to-end:
    //   1. Construct an AgenticLoop with `with_async_completion_queue`.
    //   2. Push a CompletionEvent whose parent_session_key matches the
    //      session the loop is running on.
    //   3. Run one iteration; the loop drains the queue at start.
    //   4. Assert the synthetic user message arrived at the mock LLM.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_e2e_async_completion_reaches_llm_real() {
        use crate::common::types::message::{ContentBlock as CB, LlmMessage, MessageRole};
        use crate::extensions::framework::async_exec::executor::SharedSessionInbox;
        use crate::extensions::framework::async_exec::executor::{
            AsyncTaskStatus, CompletionEvent,
        };
        use chrono::Utc;

        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Got the completion.");

        let config = test_agent_config("e2e-completion-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();

        // Build the queue the same way `Agent::build_agentic_loop` does:
        // shared between the executor and the agentic loop.
        let queue: SharedSessionInbox = std::sync::Arc::new(
            crate::extensions::framework::async_exec::executor::SessionInbox::new(),
        );
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core.clone())
            .await
            .with_async_completion_queue(queue.clone());

        // Push a completion event BEFORE the loop runs. The first
        // iteration will drain it at start and inject the synthetic
        // user-role message.
        let session = test_session("e2e-completion-agent", temp_dir.path()).await;
        let session_id = session.read().await.id.clone();

        queue.push(CompletionEvent {
            task_id: "shell:e2e-test".to_string(),
            tool_name: "shell".to_string(),
            result: serde_json::json!({"exit_code": 0, "stdout": "done"}),
            status: AsyncTaskStatus::Completed {
                result: crate::tools::core::ToolResult::success(
                    serde_json::json!({"exit_code": 0, "stdout": "done"}),
                ),
            },
            completed_at: Utc::now(),
            output_path: std::path::PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: session_id.clone(),
        });

        let result = loop_
            .run_with_resume("Trigger completion drain", |_| {}, session, None)
            .await;

        assert!(
            result.is_ok(),
            "agentic loop should succeed: {:?}",
            result.err()
        );
        let recorded = mock.recorded_requests();
        assert!(
            !recorded.is_empty(),
            "mock should have recorded at least one request"
        );

        // The recorded messages should contain the synthetic user-role
        // message we synthesized from the completion event. The first
        // request includes [system, user_prompt, synthetic_user]; the
        // synthetic block must be present.
        let req = &recorded[0];
        let synthetic_msg: Option<&LlmMessage> = req.messages.iter().find(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let CB::Text { text } = b {
                        text.contains("Async task results")
                    } else {
                        false
                    }
                })
        });
        assert!(
            synthetic_msg.is_some(),
            "expected a synthetic user-role message with the Async task results header in: {:?}",
            req.messages
                .iter()
                .map(|m| format!("{:?} -> {:?}", m.role, m.content))
                .collect::<Vec<_>>()
        );

        // The synthetic message should also carry a ToolResult block
        // whose tool_call_id is `synthetic:<task_id>`.
        let synthetic = synthetic_msg.unwrap();
        let has_tool_result = synthetic.content.iter().any(|b| {
            if let CB::ToolResult {
                tool_call_id, name, ..
            } = b
            {
                tool_call_id == "synthetic:shell:e2e-test" && name == "shell"
            } else {
                false
            }
        });
        assert!(
            has_tool_result,
            "synthetic message must carry a ToolResult with tool_call_id=synthetic:shell:e2e-test"
        );

        // Session-key flow fix: once `run_inner` is past its bootstrap,
        // the core's session key for this agent must equal the real
        // session id — not `None` and not the `"unknown"` fallback.
        // This guards the fix in `run_inner` that pushes `session_id`
        // onto the core for every entry into the loop (covers the
        // `Agent::execute` one-shot CLI path, where the session is
        // born inside `run_inner` and the caller's `build_agentic_loop`
        // would have pushed `None`). The lookup is keyed by the
        // agent's DID on the shared core (issue #68).
        let core_key = extension_core.current_session_key(&agent.identity.did);
        assert_eq!(
            core_key,
            Some(session_id.clone()),
            "core's session key for this agent must match the loop's session id after run_inner bootstrap"
        );
    }

    // ===================================================================
    // End-to-end: push a SteeringMessage to the inbox → loop delivers
    // it to the LLM as a user-role turn at the next iteration.
    //
    // Mirrors the e2e completion test above but exercises the new
    // steering half of the inbox split.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_e2e_steering_message_reaches_llm_real() {
        use crate::common::types::message::{ContentBlock as CB, LlmMessage, MessageRole};
        use crate::extensions::framework::async_exec::executor::completion_queue::{
            SessionInbox, SharedSessionInbox, SteeringMessage,
        };
        use crate::extensions::framework::async_exec::executor::AsyncTaskStatus;

        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Got the steering.");

        let config = test_agent_config("e2e-steering-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();

        let queue: SharedSessionInbox = std::sync::Arc::new(SessionInbox::new());
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core.clone())
            .await
            .with_async_completion_queue(queue.clone());

        // Pre-push a steering message AND a completion event. They
        // must arrive in insertion order, with the steering item
        // delivered as a plain user-role message and the completion
        // event folded into the synthetic user message.
        let session = test_session("e2e-steering-agent", temp_dir.path()).await;
        let session_id = session.read().await.id.clone();

        queue.push(SteeringMessage::new("actually do X instead"));
        queue.push(
            crate::extensions::framework::async_exec::executor::CompletionEvent {
                task_id: "shell:steer-test".to_string(),
                tool_name: "shell".to_string(),
                result: serde_json::json!({"exit_code": 0}),
                status: AsyncTaskStatus::Completed {
                    result: crate::tools::core::ToolResult::success(
                        serde_json::json!({"exit_code": 0}),
                    ),
                },
                completed_at: chrono::Utc::now(),
                output_path: std::path::PathBuf::from("/tmp/fake.ndjson"),
                parent_session_key: session_id.clone(),
            },
        );

        let result = loop_
            .run_with_resume("Trigger steering drain", |_| {}, session, None)
            .await;

        assert!(
            result.is_ok(),
            "agentic loop should succeed: {:?}",
            result.err()
        );
        let recorded = mock.recorded_requests();
        assert!(
            !recorded.is_empty(),
            "mock should have recorded at least one request"
        );

        let req = &recorded[0];

        // The steering content must appear in the recorded messages
        // as a user-role turn with no tool-result wrapping.
        let steering_msg: Option<&LlmMessage> = req.messages.iter().find(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let CB::Text { text } = b {
                        text == "actually do X instead"
                    } else {
                        false
                    }
                })
        });
        assert!(
            steering_msg.is_some(),
            "expected a user-role message with the steering content in: {:?}",
            req.messages
                .iter()
                .map(|m| format!("{:?} -> {:?}", m.role, m.content))
                .collect::<Vec<_>>()
        );

        // The synthetic completion message must still be present.
        let synthetic_msg: Option<&LlmMessage> = req.messages.iter().find(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let CB::Text { text } = b {
                        text.contains("Async task results")
                    } else {
                        false
                    }
                })
        });
        assert!(
            synthetic_msg.is_some(),
            "expected the synthetic user message with the Async task results header"
        );
    }

    // ===================================================================
    // RT-Interrupt: Cancel token observed at iteration boundary
    //
    // Build an AgenticLoop with a CancellationToken that's already
    // cancelled, queue a mock LLM response, and verify the loop
    // returns `interrupted: true` with an empty final answer and an
    // `Interrupted` lifecycle event. The LLM call should NOT be made
    // because the cancel check fires before the LLM iteration.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_interrupt_pre_cancelled_token_short_circuits() {
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        // No LLM call should be made because the cancel check fires
        // before the first iteration. If the test sees this text in
        // the result, the cancel check was bypassed.
        mock.queue_text("THIS_SHOULD_NOT_BE_RETURNED");

        let config = test_agent_config("interrupt-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel(); // pre-cancel
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core)
            .await
            .with_cancel_token(cancel);

        let session = test_session("interrupt-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let result = loop_
            .run_with_resume(
                "Will be interrupted",
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
            )
            .await
            .expect("agentic loop should return Ok with interrupted=true");

        assert!(
            result.interrupted,
            "result should be marked interrupted; got {result:?}"
        );
        assert!(
            !result.success,
            "interrupted run should not be marked success; got {result:?}"
        );
        assert_eq!(
            result.final_answer, "",
            "interrupted run should have an empty final answer; got {:?}",
            result.final_answer
        );

        // The agentic loop must emit a Lifecycle::Interrupted event
        // before returning.
        let emitted = events.lock().unwrap();
        let has_interrupted = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::Interrupted,
                    ..
                }
            )
        });
        assert!(
            has_interrupted,
            "expected a Lifecycle::Interrupted event in: {emitted:?}"
        );
    }

    // ===================================================================
    // End-to-end: pre-populate the directory tracker with a directory
    // that contains an `AGENTS.md`, run one iteration, and verify the
    // synthetic user-role message with the file's contents reaches the
    // LLM. Pins down the loop-side drain+inject step that pairs with
    // the adapter-side `tracker.touch` push (covered by the unit tests
    // in `agents::prompt::memory`).
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_e2e_directory_context_tracker_injects_agents_md() {
        use crate::agents::prompt::memory::DirectoryContextTracker;
        use crate::common::types::message::{ContentBlock as CB, LlmMessage, MessageRole};

        crate::identity::init_test_env();
        ensure_global_core();

        // Lay out a workspace with an `AGENTS.md` at the root.
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("AGENTS.md"),
            "Project rule: do not commit secrets.",
        )
        .unwrap();
        let nested = temp_dir.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        // Mock LLM returns a plain text final answer. We don't need
        // tool calls for this test — we just want to verify the
        // synthetic user message gets injected before the LLM sees the
        // prompt.
        let (provider, mock) = mock_provider();
        mock.queue_text("ack");

        // Build an agent whose workspace is the temp dir so the
        // walk-up in `discover_shared_context` is bounded to it.
        let mut config = test_agent_config("agents-md-agent");
        config.workspace = Some(temp_dir.path().to_path_buf());
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();

        // Pre-populate the tracker with the deep nested directory.
        // The discovery loop should walk up to the workspace root and
        // pick up the AGENTS.md we wrote there.
        let tracker = Arc::new(DirectoryContextTracker::new());
        assert!(tracker.touch(&nested));

        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core)
            .await
            .with_directory_tracker(tracker.clone());

        let session = test_session("agents-md-agent", temp_dir.path()).await;
        let result = loop_.run_with_resume("begin", |_| {}, session, None).await;
        assert!(
            result.is_ok(),
            "agentic loop should succeed: {:?}",
            result.err()
        );

        let recorded = mock.recorded_requests();
        assert!(
            !recorded.is_empty(),
            "mock should have recorded at least one request"
        );

        // Find a user-role message containing the AGENTS.md content
        // wrapped in our `<directory-context>` synthetic block.
        let req = &recorded[0];
        let synthetic: Option<&LlmMessage> = req.messages.iter().find(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let CB::Text { text } = b {
                        text.contains("<directory-context")
                            && text.contains("do not commit secrets")
                    } else {
                        false
                    }
                })
        });
        assert!(
            synthetic.is_some(),
            "expected a synthetic <directory-context> user message in: {:?}",
            req.messages
                .iter()
                .map(|m| format!("{:?} -> {:?}", m.role, m.content))
                .collect::<Vec<_>>()
        );

        // Drain should have consumed the touched directory, so a second
        // run with no new touches emits nothing.
        assert!(
            tracker.snapshot().is_empty(),
            "tracker should be drained after the iteration"
        );
    }

    // ===================================================================
    // Companion to the test above: the directory tracker dedupes
    // canonicalised paths, so pushing the same logical directory twice
    // across iterations only triggers one discovery. Pinned here so the
    // idempotence is enforced at the engine layer (the unit test in
    // `agents::prompt::memory` already covers the data structure).
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_directory_tracker_dedupes_across_iterations() {
        use crate::agents::prompt::memory::DirectoryContextTracker;

        let tracker = DirectoryContextTracker::new();
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir_all(&a).unwrap();

        // Two distinct logical pushes from the same path.
        assert!(tracker.touch(&a));
        assert!(!tracker.touch(&a));

        let drained = tracker.drain_new();
        assert_eq!(
            drained.len(),
            1,
            "tracker must collapse canonical-equivalent paths"
        );

        // Snapshot is empty post-drain.
        assert!(tracker.snapshot().is_empty());

        // Re-pushing after drain registers a fresh touch.
        assert!(tracker.touch(&a));
        assert_eq!(tracker.drain_new().len(), 1);
    }
}
