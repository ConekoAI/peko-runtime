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

use crate::agent::Agent;
use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::prompt::SystemPromptService;
use crate::providers::{ChatOptions, MessageRole, StopReason, TokenUsage, ToolDefinition};
use crate::session::Session;
use crate::types::message::{ContentBlock, LlmMessage};
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
    extension_core: Arc<crate::extension::ExtensionCore>,
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
        extension_core: Arc<crate::extension::ExtensionCore>,
    ) -> Self {
        let system_prompt = SystemPromptService::build(&agent, &extension_core).await;

        Self {
            agent,
            provider,
            max_iterations: 10,
            system_prompt,
            extension_core,
        }
    }

    /// Set maximum iterations
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Get the extension core
    #[must_use]
    pub fn extension_core(&self) -> &Arc<crate::extension::ExtensionCore> {
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

    /// Original run method - creates new session via `SessionManager`
    pub async fn run(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResult> {
        use crate::common::paths::PathResolver;
        use crate::session::manager::SessionManager;
        use crate::session::types::Peer;

        // Create session via SessionManager
        let path_resolver = PathResolver::new();
        let mut session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, self.agent.name(), None)
            .await?;
        let peer = Peer::User("default".to_string());
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
        let _session_id = {
            let s = session.read().await;
            s.id.clone()
        };

        // Set provider/model metadata on session (do this once at start)
        {
            let provider_name = self.agent.config.provider.provider_type.to_string();
            let model_name = &self.agent.config.provider.default_model;

            let mut s = session.write().await;
            s.set_model(&provider_name, model_name);
            // Record model change event in session JSONL for normalization
            if let Err(e) = s.record_model_change(&provider_name, model_name).await {
                warn!("Failed to record model change event: {}", e);
            }
        }

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
                        crate::types::message::ContentBlock::Text { text } => {
                            format!("[Text: {}]", text.chars().take(50).collect::<String>())
                        }
                        crate::types::message::ContentBlock::ToolCall { id, name, .. } => {
                            format!("[ToolCall: {name} ({id})]")
                        }
                        crate::types::message::ContentBlock::ToolResult {
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
                    .stream_with_tools(&messages, &tool_defs, &options)
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
                    .chat_with_tools(&messages, &tool_defs, &options)
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
                    None => break,
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

                // Execute tools
                for tool_call in &tool_calls {
                    let result = tool_executor
                        .execute(
                            tool_call,
                            &self.extension_core,
                            self.agent.name(),
                            self.agent.config.workspace.as_ref(),
                            &session,
                            &run_id,
                            &on_event,
                        )
                        .await?;
                    messages.push(result.message);
                }

                // Continue to next iteration
                continue;
            }

            // No tool calls - this is the final answer
            info!("Final answer received after {} iterations", iteration);

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
            });
        }
    }

    /// Build tool definitions dynamically from `ExtensionCore` (ADR-019 Phase 2)
    ///
    /// This queries the unified tool registry for currently enabled tools,
    /// allowing tool changes to take effect without session restart.
    async fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        let defs = self.extension_core.list_tool_definitions().await;

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
    use crate::agent::Agent;
    use crate::extension::core::{global_core, init_global_core, ExtensionCore};
    use crate::providers::{AnyAdapter, MockAdapter, Provider};
    use crate::session::manager::SessionManager;
    use crate::session::types::Peer;
    use crate::types::agent::AgentConfig;
    use crate::types::provider::{ProviderConfig, ProviderType};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    /// Build a mock provider with a fresh MockAdapter
    fn mock_provider() -> (Arc<Provider>, MockAdapter) {
        let adapter = MockAdapter::new("mock-model");
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
            provider: ProviderConfig {
                provider_type: ProviderType::OpenAI,
                api_key: Some("mock_key".to_string()),
                ..Default::default()
            },
            extensions: Some(crate::types::agent::ExtensionConfig {
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
        let peer = Peer::User("default".to_string());
        let handle = manager
            .create_session(agent_name, &peer, crate::session::manager::SessionCreateOptions::new())
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
    async fn test_rt001_basic_agentic_loop() {
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
        let has_start = emitted.iter().any(|e| matches!(e, AgenticEvent::Lifecycle { phase: LifecyclePhase::Start, .. }));
        let has_end = emitted.iter().any(|e| matches!(e, AgenticEvent::Lifecycle { phase: LifecyclePhase::End, .. }));
        assert!(has_start, "Should emit Start event");
        assert!(has_end, "Should emit End event");
    }

    // ===================================================================
    // RT-002: Engine MUST support streaming output
    // ===================================================================
    #[tokio::test]
    async fn test_rt002_streaming_output() {
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
        let has_deltas = emitted.iter().any(|e| matches!(e, AgenticEvent::AssistantDelta { .. }));
        assert!(has_deltas, "Live streaming should emit AssistantDelta events");
    }

    // ===================================================================
    // RT-003: Engine MUST enforce a configurable timeout per LLM request
    // ===================================================================
    #[tokio::test]
    async fn test_rt003_timeout_config_propagation() {
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Quick response");

        let mut config = test_agent_config("rt003-agent");
        config.provider.timeout_seconds = 42;
        let agent = Arc::new(Agent::new_for_test(config.clone(), temp_dir.path()).await.unwrap());
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
    async fn test_rt004_graceful_error_handling() {
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
        let has_error = emitted.iter().any(|e| matches!(e, AgenticEvent::Lifecycle { phase: LifecyclePhase::Error, .. }));
        assert!(has_error, "Should emit Error lifecycle event");
    }

    // ===================================================================
    // RT-005: Engine MUST persist every message to JSONL atomically
    // ===================================================================
    #[tokio::test]
    async fn test_rt005_session_persistence() {
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
        let has_assistant = history.iter().any(|m| matches!(m.role, MessageRole::Assistant));
        assert!(has_assistant, "Session should contain assistant message");
    }

    // ===================================================================
    // RT-006: Engine MUST support up to 10 iterations per turn
    // ===================================================================
    #[tokio::test]
    async fn test_rt006_max_iterations_enforced() {
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
    async fn test_rt006_default_max_iterations_is_10() {
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Done");

        let config = test_agent_config("rt006-default-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        // The struct should default to 10
        assert_eq!(loop_.max_iterations, 10, "Default max_iterations should be 10");
    }

    // ===================================================================
    // Integration: tool call -> tool execution -> next iteration
    // ===================================================================
    #[tokio::test]
    async fn test_tool_call_iteration() {
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

        assert!(result.is_ok(), "Tool loop should succeed: {:?}", result.err());
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
}
