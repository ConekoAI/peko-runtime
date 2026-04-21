//! Agentic loop v4 - unified streaming core with presentation-layer delivery modes
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
use crate::prompt::{PromptMode, SystemPromptBuilder};
use crate::providers::{ChatMessage, ChatOptions, MessageRole, StopReason, ToolDefinition};
use crate::session::UnifiedSession;
use crate::types::message::ContentBlock;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// Extension Core imports for skill loading and tool execution
use crate::extensions::adapters::skill_adapter::{register_skills_with_core, SkillAdapter};
use crate::extensions::core::HookPointBuilder;
use crate::extensions::{HookInput, HookOutput, HookResult};

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
    pub usage: crate::providers::TokenUsage,
}

/// A tool call for session storage compatibility
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
}

/// v4 agentic loop with native tool calling
pub struct AgenticLoopV4 {
    agent: Arc<Agent>,
    provider: Arc<crate::providers::Provider>,
    max_iterations: usize,
    system_prompt: String,
    /// Extension core for skill loading and tool registration.
    extension_core: Arc<crate::extensions::ExtensionCore>,
}

impl AgenticLoopV4 {
    /// Create a new v4 agentic loop
    ///
    /// # Arguments
    /// * `agent` - The agent configuration
    /// * `provider` - The LLM provider to use
    /// * `extension_core` - The `ExtensionCore` for skill loading and hook integration
    pub async fn new(
        agent: Arc<Agent>,
        provider: Arc<crate::providers::Provider>,
        extension_core: Arc<crate::extensions::ExtensionCore>,
    ) -> Self {
        let system_prompt = build_system_prompt(&agent, &extension_core).await;

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
    pub fn extension_core(&self) -> &Arc<crate::extensions::ExtensionCore> {
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
        session: Arc<RwLock<UnifiedSession>>,
        history: Option<Vec<ChatMessage>>,
    ) -> Result<AgenticResult> {
        let config = crate::engine::OrchestratorConfig::final_only();
        self.run_streaming_with_resume(prompt, on_event, session, history, config)
            .await
    }

    /// Run the agent with streaming support, optionally resuming from an existing session.
    ///
    /// Run the agent with streaming support, optionally resuming from an existing session.
    ///
    /// Uses `DeliveryMode::Live` or `DeliveryMode::Block` for real-time output.
    /// The core loop is the same as `run_with_resume`; only the orchestrator config differs.
    pub async fn run_streaming_with_resume(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<UnifiedSession>>,
        history: Option<Vec<ChatMessage>>,
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

        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };
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
                let mut msgs = vec![ChatMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: self.system_prompt.clone(),
                    }],
                    tool_calls: None,
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
            let msgs = vec![ChatMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: self.system_prompt.clone(),
                }],
                tool_calls: None,
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
        messages.push(ChatMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
            }],
            tool_calls: None,
            tool_call_id: None,
        });

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
        mut messages: Vec<ChatMessage>,
        session: Arc<RwLock<UnifiedSession>>,
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
        }

        let mut iteration = 0;
        let mut total_usage = crate::providers::TokenUsage::default();

        // Load previous compaction summary from session for cumulative updates
        let previous_summary = {
            let s = session.read().await;
            s.load_previous_compaction_summary().await.ok().flatten()
        };
        if previous_summary.is_some() {
            info!("Found previous compaction summary for cumulative updates");
        }

        // Initialize background compactor
        let background_compactor =
            crate::compaction::background::BackgroundCompactor::new(self.provider.clone());

        // Track pending compaction
        let mut pending_compaction: Option<
            tokio::sync::oneshot::Receiver<crate::compaction::background::CompactionResponse>,
        > = None;

        // Initialize compactor config for quota checks
        let compaction_config = crate::compaction::CompactionConfig::default();

        loop {
            iteration += 1;
            let mut iteration_usage = crate::providers::TokenUsage::default();
            info!("Agent loop: iteration {}", iteration);

            // ADR-019 Phase 2: Build tool definitions dynamically each iteration
            let tool_defs = self.build_tool_definitions().await;

            // ADR-019 Phase 3: Rebuild system prompt dynamically
            if !messages.is_empty() && matches!(messages[0].role, MessageRole::System) {
                let fresh_prompt = self.build_system_prompt_fresh().await;
                messages[0] = ChatMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text { text: fresh_prompt }],
                    tool_calls: None,
                    tool_call_id: None,
                };
            }

            // Check if compaction is needed before LLM call (background)
            let estimated_tokens = crate::compaction::Compactor::estimate_tokens(&messages);

            // Start background compaction if needed and not already running
            if pending_compaction.is_none()
                && background_compactor
                    .should_request(estimated_tokens, &compaction_config)
                    .await
            {
                info!(
                    "Context window approaching limit ({} tokens), starting background compaction...",
                    estimated_tokens
                );
                on_event(AgenticEvent::Thinking {
                    run_id: run_id.clone(),
                    text: "Session is getting long. Summarizing older messages in background..."
                        .to_string(),
                    is_delta: false,
                    is_final: false,
                    signature: None,
                });

                let prev_summary = {
                    let s = session.read().await;
                    s.load_previous_compaction_summary().await.ok().flatten()
                };
                match background_compactor
                    .request_compaction(messages.clone(), prev_summary)
                    .await
                {
                    Ok(receiver) => {
                        pending_compaction = Some(receiver);
                    }
                    Err(e) => {
                        warn!("Failed to start background compaction: {}", e);
                    }
                }
            }

            // Check if background compaction has completed
            if let Some(ref mut receiver) = pending_compaction {
                match tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver).await
                {
                    Ok(Ok(response)) => {
                        match response {
                            crate::compaction::background::CompactionResponse::Completed(
                                result,
                            ) => {
                                messages = result.messages;
                                info!(
                                    "Background compaction #{} complete: {} messages → summary, saved {} tokens ({} → {})",
                                    result.entry.compaction_number,
                                    result.entry.messages_compacted,
                                    result.entry.tokens_before - result.entry.tokens_after,
                                    result.entry.tokens_before,
                                    result.entry.tokens_after
                                );

                                // Record compaction entry in session
                                {
                                    let mut s = session.write().await;
                                    if let Err(e) = s
                                        .record_compaction(
                                            &result.entry.summary,
                                            result.entry.messages_compacted,
                                            result.entry.tokens_before,
                                            result.entry.tokens_after,
                                            result.entry.compaction_number,
                                        )
                                        .await
                                    {
                                        warn!("Failed to record compaction entry: {}", e);
                                    }
                                }
                            }
                            crate::compaction::background::CompactionResponse::NotNeeded => {
                                debug!("Background compaction: not needed");
                            }
                            crate::compaction::background::CompactionResponse::Skipped(reason) => {
                                debug!("Background compaction skipped: {}", reason);
                            }
                            crate::compaction::background::CompactionResponse::Failed(err) => {
                                warn!("Background compaction failed: {}", err);
                            }
                        }
                        pending_compaction = None;
                    }
                    Ok(Err(_)) => {
                        warn!("Background compaction channel closed");
                        pending_compaction = None;
                    }
                    Err(_) => {
                        // Timeout - compaction still in progress, continue with LLM call
                    }
                }
            }

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
                    info!("Tool def [{}]: name={}, params={}", i, def.name, def.parameters);
                }
                match self.provider.stream_with_tools(&messages, &tool_defs, &options).await {
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
                self.synthesize_stream_from_blocking(&messages, &tool_defs, &options).await?
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
                messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    content: assistant_content,
                    tool_calls: None,
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
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } = tool_call
                    {
                        info!("Executing tool: {} (id: {})", name, id);

                        // Note: ToolStart was already emitted by StreamOrchestrator
                        // when processing StreamEvent::ToolCallEnd during streaming.
                        // Do NOT emit it again here to avoid duplicate "[Running tool: X]" output.

                        // Execute tool via ExtensionCore for unified execution (ADR-018a)
                        let start_time = std::time::Instant::now();

                        // Check if tool is registered in ExtensionCore
                        let tool_in_extension =
                            self.extension_core.get_tool_metadata(name).await.is_some();

                        let (tool_result_str, tool_result_json, success) = if tool_in_extension {
                            // Route through ExtensionCore for unified execution with panic isolation
                            let hook_point = HookPointBuilder::tool_execute(name);
                            let hook_input = HookInput::ToolCall {
                                tool_name: name.clone(),
                                params: arguments.clone(),
                                workspace: self
                                    .agent
                                    .config
                                    .workspace
                                    .as_ref()
                                    .map(|p| p.to_string_lossy().to_string()),
                            };

                            match self
                                .extension_core
                                .invoke_hook(hook_point, hook_input)
                                .await
                            {
                                HookResult::Continue(HookOutput::Json(result)) => {
                                    info!(
                                        "Tool '{}' executed successfully via ExtensionCore",
                                        name
                                    );
                                    let s = result.to_string();
                                    (s.clone(), result, true)
                                }
                                HookResult::Continue(HookOutput::Text(result)) => {
                                    info!(
                                        "Tool '{}' executed successfully via ExtensionCore",
                                        name
                                    );
                                    (result.clone(), serde_json::Value::String(result), true)
                                }
                                HookResult::Continue(HookOutput::Vec(outputs)) => {
                                    let result = outputs.iter().find_map(|o| match o {
                                        HookOutput::Json(v) => Some((v.to_string(), v.clone())),
                                        HookOutput::Text(t) => {
                                            Some((t.clone(), serde_json::Value::String(t.clone())))
                                        }
                                        _ => None,
                                    });
                                    if let Some((s, v)) = result {
                                        info!("Tool '{}' executed successfully via ExtensionCore (from {} outputs)", name, outputs.len());
                                        (s, v, true)
                                    } else {
                                        warn!(
                                            "Tool '{}' returned Vec with no Json/Text: {:?}",
                                            name, outputs
                                        );
                                        let s =
                                            "Error: Unexpected Vec output from tool".to_string();
                                        (s.clone(), serde_json::Value::String(s), false)
                                    }
                                }
                                HookResult::Continue(other) => {
                                    warn!(
                                        "Tool '{}' returned non-Json/Text output: {:?}",
                                        name,
                                        std::mem::discriminant(&other)
                                    );
                                    let s = "Error: Unexpected output type from tool".to_string();
                                    (s.clone(), serde_json::Value::String(s), false)
                                }
                                HookResult::PassThrough => {
                                    warn!("Tool '{}' not handled by ExtensionCore", name);
                                    let s = format!("Tool '{name}' not available");
                                    (s.clone(), serde_json::Value::String(s), false)
                                }
                                HookResult::Error(e) => {
                                    info!("Tool '{}' failed via ExtensionCore: {}", name, e);
                                    let s = format!("Error: {e}");
                                    (s.clone(), serde_json::Value::String(s), false)
                                }
                                HookResult::Handled => {
                                    warn!("Hook result for tool '{}' was Handled (consumed)", name);
                                    let s =
                                        "Error: Tool execution was consumed by handler".to_string();
                                    (s.clone(), serde_json::Value::String(s), false)
                                }
                                HookResult::Replace(output) => {
                                    warn!(
                                        "Hook result for tool '{}' was Replace: {:?}",
                                        name, output
                                    );
                                    let s = "Error: Tool execution was replaced".to_string();
                                    (s.clone(), serde_json::Value::String(s), false)
                                }
                            }
                        } else {
                            let s = format!("Tool '{name}' not found");
                            (s.clone(), serde_json::Value::String(s), false)
                        };

                        let duration_ms = start_time.elapsed().as_millis() as u64;

                        // Add to session
                        {
                            let mut s = session.write().await;
                            s.add_tool_result(id, name, &tool_result_str).await?;
                        }

                        on_event(AgenticEvent::ToolEnd {
                            run_id: run_id.clone(),
                            tool_id: id.clone(),
                            result: tool_result_json,
                            success,
                            duration_ms,
                        });

                        // Add tool result to messages
                        messages.push(ChatMessage {
                            role: MessageRole::Tool,
                            content: vec![ContentBlock::ToolResult {
                                tool_call_id: id.clone(),
                                name: name.clone(),
                                content: vec![ContentBlock::Text {
                                    text: tool_result_str.clone(),
                                }],
                                is_error: !success,
                            }],
                            tool_calls: None,
                            tool_call_id: Some(id.clone()),
                        });
                    }
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

    /// Build system prompt dynamically (ADR-019 Phase 3)
    ///
    /// Rebuilds the system prompt from current state, allowing:
    /// - SYSTEM.md changes to be reflected immediately
    /// - Tool list updates in prompt
    /// - Skill/extension changes to be picked up
    async fn build_system_prompt_fresh(&self) -> String {
        // Get current tool definitions from ExtensionCore
        let tool_defs = self.extension_core.list_tool_definitions().await;

        // Use configured workspace if specified
        let workspace_dir = self
            .agent
            .config
            .workspace
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));

        SystemPromptBuilder::new(self.agent.name())
            .with_mode(PromptMode::Full)
            .with_workspace(&workspace_dir)
            .with_extension_core(Arc::clone(&self.extension_core))
            .with_tool_definitions(tool_defs)
            .build()
    }

    /// Get the system prompt
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Fallback for providers without native tool support
    async fn fallback_chat_with_tools(
        &self,
        messages: &[ChatMessage],
        _prompt: &str,
    ) -> Result<crate::providers::ChatResponse> {
        // Convert messages to prompt
        let prompt = messages_to_prompt(messages);

        // Use legacy completion
        let response = self.provider.complete(&prompt).await?;

        // Parse response as JSON with content blocks (legacy format)
        let content_blocks = parse_legacy_response(&response);

        // Extract tool calls
        let mut tool_calls = Vec::new();
        let mut text_content = Vec::new();

        for block in &content_blocks {
            match block {
                ContentBlock::ToolCall { .. } => tool_calls.push(block.clone()),
                _ => text_content.push(block.clone()),
            }
        }

        let stop_reason = if tool_calls.is_empty() {
            StopReason::Stop
        } else {
            StopReason::ToolUse
        };

        Ok(crate::providers::ChatResponse {
            content: text_content,
            tool_calls,
            stop_reason,
            usage: crate::providers::TokenUsage::default(),
            provider: self.provider.name().to_string(),
            model: "default".to_string(),
        })
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
        session: Arc<RwLock<UnifiedSession>>,
        history: Option<Vec<ChatMessage>>,
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
                let mut msgs = vec![ChatMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: self.system_prompt.clone(),
                    }],
                    tool_calls: None,
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
            let msgs = vec![ChatMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: self.system_prompt.clone(),
                }],
                tool_calls: None,
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
        messages.push(ChatMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
            }],
            tool_calls: None,
            tool_call_id: None,
        });

        // Add user message to session
        {
            let mut s = session.write().await;
            s.add_user(prompt).await?;
        }

        // Run the streaming loop
        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }

    /// Synthesize a streaming response from a blocking provider.
    ///
    /// For providers that don't support SSE streaming, we call `chat_with_tools`
    /// and emit synthetic `StreamEvent`s so the unified loop can process them
    /// exactly like real streaming events.
    async fn synthesize_stream_from_blocking(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<crate::providers::StreamEvent>> + Send>>>
    {
        let response = self.provider.chat_with_tools(messages, tools, options).await?;

        let mut events: Vec<anyhow::Result<crate::providers::StreamEvent>> = Vec::new();

        events.push(Ok(crate::providers::StreamEvent::Start {
            provider: self.provider.name().to_string(),
            model: "default".to_string(),
        }));

        // Emit text content as a single delta
        let text: String = response
            .content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::Text { text } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !text.is_empty() {
            events.push(Ok(crate::providers::StreamEvent::TextDelta {
                content_index: 0,
                delta: text,
            }));
            events.push(Ok(crate::providers::StreamEvent::TextEnd {
                content_index: 0,
                content: String::new(),
            }));
        }

        // Emit thinking content
        let thinking: String = response
            .content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::Thinking { text, .. } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !thinking.is_empty() {
            events.push(Ok(crate::providers::StreamEvent::ThinkingDelta {
                content_index: 0,
                delta: thinking.clone(),
            }));
            events.push(Ok(crate::providers::StreamEvent::ThinkingEnd {
                content_index: 0,
                content: thinking,
            }));
        }

        // Emit tool calls
        for (i, tc) in response.tool_calls.iter().enumerate() {
            if let ContentBlock::ToolCall { .. } = tc {
                events.push(Ok(crate::providers::StreamEvent::ToolCallEnd {
                    content_index: i,
                    tool_call: tc.clone(),
                }));
            }
        }

        // Usage
        events.push(Ok(crate::providers::StreamEvent::Usage {
            input: response.usage.input,
            output: response.usage.output,
            total: response.usage.total,
        }));

        // Done
        events.push(Ok(crate::providers::StreamEvent::Done {
            stop_reason: response.stop_reason,
        }));

        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// Build system prompt from agent and tools using `SystemPromptBuilder`
/// Includes bootstrap file injection (AGENTS.md, SOUL.md, etc.) and skills
///
/// Skills are loaded via the `ExtensionCore` using the `SkillAdapter`.
async fn build_system_prompt(
    agent: &Agent,
    extension_core: &Arc<crate::extensions::ExtensionCore>,
) -> String {
    info!(
        "Building initial system prompt for agent '{}'",
        agent.name()
    );

    // Use configured workspace if specified, otherwise use default with team
    let workspace_dir = agent
        .config
        .workspace
        .clone()
        .or_else(|| {
            // Get team from config or default to "default"
            let team = agent.config.team.as_deref().unwrap_or("default");
            dirs::data_dir()
                .map(|d| {
                    d.join("pekobot")
                        .join("workspaces")
                        .join(team)
                        .join(agent.name())
                })
                .or_else(|| {
                    dirs::home_dir().map(|h| {
                        h.join(".pekobot")
                            .join("workspaces")
                            .join(team)
                            .join(agent.name())
                    })
                })
        })
        .unwrap_or_else(|| PathBuf::from("."));

    // Load enabled skills from the skills directory using ExtensionCore
    let path_resolver = crate::common::paths::PathResolver::new();
    let enabled_skills = agent
        .config
        .tools
        .as_ref()
        .map_or(&vec![], |t| &t.skills)
        .clone();

    // Load and register skills with ExtensionCore
    let _skills_loaded = load_and_register_skills(
        agent.name(),
        &enabled_skills,
        &path_resolver,
        extension_core,
    )
    .await;

    // Extract custom bootstrap files from agent config if specified
    let bootstrap_files = agent
        .config
        .prompt
        .as_ref()
        .and_then(|p| p.system.as_ref())
        .and_then(|s| s.files.clone());

    SystemPromptBuilder::new(agent.name())
        .with_mode(PromptMode::Full)
        .with_workspace(&workspace_dir)
        .with_extension_core(Arc::clone(extension_core))
        .with_system_files(bootstrap_files)
        .build()
}

/// Load enabled skills for an agent from the skills directory using `ExtensionCore`
///
/// This function discovers skills from the filesystem and registers them with the
/// `ExtensionCore`. Skills are then injected into the system prompt via the
/// `PromptSystemSection { section: "skills" }` hook point.
///
/// # Returns
/// The number of skills successfully registered with the `ExtensionCore`.
async fn load_and_register_skills(
    agent_name: &str,
    enabled_skills: &[String],
    path_resolver: &crate::common::paths::PathResolver,
    extension_core: &Arc<crate::extensions::ExtensionCore>,
) -> usize {
    // Use PathResolver for consistent path resolution
    let skills_dir = path_resolver.skills_dir();

    tracing::debug!("Loading skills from: {:?}", skills_dir);
    tracing::debug!(
        "Enabled skills for agent {}: {:?}",
        agent_name,
        enabled_skills
    );

    if !skills_dir.exists() {
        tracing::debug!("Skills directory does not exist: {:?}", skills_dir);
        return 0;
    }

    // Discover skills using the SkillAdapter (synchronous)
    let adapter = SkillAdapter::new();
    let all_skills = adapter.discover_skills(&skills_dir);

    tracing::debug!("Discovered {} skills from directory", all_skills.len());

    // Filter to only enabled skills
    let skills_to_register: Vec<_> = all_skills
        .into_iter()
        .filter(|s| {
            let is_enabled = enabled_skills
                .iter()
                .any(|e| e.eq_ignore_ascii_case(&s.manifest.name));
            tracing::debug!("Skill '{}' enabled: {}", s.manifest.name, is_enabled);
            is_enabled
        })
        .collect();

    if skills_to_register.is_empty() {
        tracing::info!("No enabled skills to register for agent {}", agent_name);
        return 0;
    }

    // Register skills with ExtensionCore
    let count = skills_to_register.len();
    let _ = register_skills_with_core(extension_core, skills_to_register).await;

    tracing::info!(
        "Registered {} enabled skills for agent {}",
        count,
        agent_name
    );
    count
}

/// Convert chat messages to prompt string (fallback)
fn messages_to_prompt(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::Tool => "Tool",
            };
            let content = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<String>();
            format!("{role}: {content}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse legacy JSON response format (fallback)
fn parse_legacy_response(response: &str) -> Vec<ContentBlock> {
    // Try to parse as JSON with content field
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(response.trim()) {
        if let Some(content) = json.get("content").and_then(|c| c.as_array()) {
            let mut blocks = Vec::new();
            for item in content {
                if let Some(block) = parse_content_block(item) {
                    blocks.push(block);
                }
            }
            if !blocks.is_empty() {
                return blocks;
            }
        }
    }

    // Fallback: treat as plain text
    vec![ContentBlock::Text {
        text: response.to_string(),
    }]
}

/// Parse a single content block from JSON
fn parse_content_block(value: &serde_json::Value) -> Option<ContentBlock> {
    let block_type = value.get("type")?.as_str()?;

    match block_type {
        "text" => {
            let text = value.get("text")?.as_str()?.to_string();
            Some(ContentBlock::Text { text })
        }
        "thinking" => {
            let text = value.get("thinking")?.as_str()?.to_string();
            let signature = value
                .get("signature")
                .and_then(|s| s.as_str())
                .map(std::string::ToString::to_string);
            Some(ContentBlock::Thinking { text, signature })
        }
        "tool_call" => {
            let id = value.get("id")?.as_str()?.to_string();
            let name = value.get("name")?.as_str()?.to_string();
            let arguments = value.get("arguments")?.clone();
            Some(ContentBlock::ToolCall {
                id,
                name,
                arguments,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {}
