//! Agentic loop v4 - with native tool calling via provider APIs
//!
//! This is a complete rewrite using the native tool calling approach:
//! - No text parsing for tool calls
//! - Structured `ContentBlock` types throughout
//! - Unified event callback API (no separate `run/run_streaming`)
//! - Streaming support with incremental tool call construction

use crate::agent::Agent;
use crate::engine::{AgenticEvent, LifecyclePhase, TaskManager};
use crate::prompt::{PromptMode, SystemPromptBuilder};
use crate::providers::{ChatMessage, ChatOptions, MessageRole, StopReason, ToolDefinition};
use crate::session::UnifiedSession;
use crate::tools::Tool;
use crate::types::message::ContentBlock;
use anyhow::Result;
use std::path::PathBuf;
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
    provider: Arc<dyn crate::providers::Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_iterations: usize,
    system_prompt: String,
    /// Task manager for tool execution
    task_manager: Arc<TaskManager>,
}

impl AgenticLoopV4 {
    /// Create a new v4 agentic loop
    pub fn new(
        agent: Arc<Agent>,
        provider: Arc<dyn crate::providers::Provider>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        let system_prompt = build_system_prompt(&agent, &tools);

        Self {
            agent,
            provider,
            tools,
            max_iterations: 10,
            system_prompt,
            task_manager: Arc::new(TaskManager::new()),
        }
    }

    /// Set maximum iterations
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run the agent with a user prompt, optionally resuming from an existing session.
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
            "Starting v4 agentic loop for agent: {} (session: {})",
            self.agent.name(),
            session_id
        );

        // Build messages - either from history or fresh start
        let mut messages = if let Some(h) = history {
            info!("Loaded {} messages from history", h.len());
            // Check if history already has a system message at the start
            let has_system = h
                .first()
                .map(|m| matches!(m.role, MessageRole::System))
                .unwrap_or(false);
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

        // Continue with the rest of the run logic
        self.run_loop(messages, session, on_event, run_id).await
    }

    /// Run the agent with streaming support, optionally resuming from an existing session.
    ///
    /// This is the streaming version of `run_with_resume` that uses `run_streaming_loop`
    /// instead of `run_loop` for real-time token emission.
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
                .map(|m| matches!(m.role, MessageRole::System))
                .unwrap_or(false);
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

        // Continue with the streaming run logic
        self.run_streaming_loop(messages, session, on_event, run_id, streaming_config).await
    }

    /// Original run method - creates new session via SessionManager
    pub async fn run(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResult> {
        use crate::session::manager::SessionManager;
        use crate::session::types::Peer;

        // Create session via SessionManager
        let mut session_manager = SessionManager::new()
            .with_registry(self.agent.name())
            .await?;
        let peer = Peer::User("default".to_string());
        let session = session_manager
            .get_or_create_base(self.agent.name(), &peer)
            .await?;

        self.run_with_resume(prompt, on_event, session, None).await
    }

    /// Main agent loop logic
    async fn run_loop(
        &self,
        mut messages: Vec<ChatMessage>,
        session: Arc<RwLock<UnifiedSession>>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        run_id: String,
    ) -> Result<AgenticResult> {
        // Main agent loop
        // Sequence counter for AssistantText events
        let mut sequence_counter: usize = 0;
        // Build tool definitions
        let tool_defs = self.build_tool_definitions();

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
            info!("Agent loop: iteration {}", iteration);

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

            // Chat with tools
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

            let response = if self.provider.supports_native_tools() {
                // Use native tool calling
                self.provider
                    .chat_with_tools(&messages, &tool_defs, &options)
                    .await?
            } else {
                // Fallback to legacy text-based approach
                self.fallback_chat_with_tools(&messages, "").await?
            };

            // Accumulate usage
            total_usage.input += response.usage.input;
            total_usage.output += response.usage.output;
            total_usage.total += response.usage.total;

            // Process response content
            let mut text_parts = Vec::new();
            let mut thinking_parts = Vec::new();
            let mut assistant_content = Vec::new();

            for block in &response.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_parts.push(text.clone());
                        assistant_content.push(block.clone());
                    }
                    ContentBlock::Thinking { text, .. } => {
                        thinking_parts.push(text.clone());
                        assistant_content.push(block.clone());
                    }
                    _ => {}
                }
            }

            // Emit thinking/reasoning text BEFORE tool calls
            let thinking_text = thinking_parts.join(" ").trim().to_string();
            let assistant_text = text_parts.join(" ").trim().to_string();

            // Only emit thinking event if there's actual thinking content
            // Don't emit assistant text as thinking - that causes duplication
            if !thinking_text.is_empty() {
                on_event(AgenticEvent::Thinking {
                    run_id: run_id.clone(),
                    text: thinking_text.clone(),
                    is_delta: false,
                    is_final: true,
                    signature: None,
                });
            }

            // Handle tool calls
            if !response.tool_calls.is_empty() {
                info!("Processing {} tool calls", response.tool_calls.len());

                // Add tool call blocks to assistant_content for proper serialization
                for tool_call in &response.tool_calls {
                    assistant_content.push(tool_call.clone());
                }

                // Add assistant message with tool calls to history
                let assistant_msg = ChatMessage {
                    role: MessageRole::Assistant,
                    content: assistant_content,
                    tool_calls: None, // We include tool calls in content now
                    tool_call_id: None,
                };
                messages.push(assistant_msg.clone());

                // Add to session with original tool call IDs (using LLM-native format)
                let tool_call_blocks: Vec<crate::session::events::ToolCallBlock> = response
                    .tool_calls
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
                if !assistant_text.is_empty() {
                    content_blocks.push(ContentBlock::Text {
                        text: assistant_text.clone(),
                    });
                }
                for tool_call in &response.tool_calls {
                    content_blocks.push(tool_call.clone());
                }

                // Build thinking block if thinking content was captured
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
                        Some(response.usage.clone()),
                    )
                    .await?;
                }

                // Emit assistant text BEFORE tool calls so user sees what's happening
                if !assistant_text.is_empty() {
                    sequence_counter += 1;
                    on_event(AgenticEvent::AssistantText {
                        run_id: run_id.clone(),
                        text: assistant_text.clone(),
                        sequence: sequence_counter,
                        is_interstitial: true, // Text before tool calls
                    });
                }

                // Execute each tool
                let mut tool_results = Vec::new();

                for tool_call in &response.tool_calls {
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } = tool_call
                    {
                        info!("Executing tool: {} (id: {})", name, id);

                        // Emit tool start event
                        on_event(AgenticEvent::ToolStart {
                            run_id: run_id.clone(),
                            tool_id: id.clone(),
                            name: name.clone(),
                            params: arguments.clone(),
                        });

                        // Find and execute tool using the task manager
                        let start_time = std::time::Instant::now();
                        let tool_result =
                            if let Some(tool) = self.tools.iter().find(|t| t.name() == name) {
                                // Execute tool synchronously with default timeout
                                match self
                                    .task_manager
                                    .execute(
                                        Arc::clone(tool),
                                        arguments.clone(),
                                        None, // Use default timeout
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        info!("Tool '{}' executed successfully", name);
                                        result.to_string()
                                    }
                                    Err(e) => {
                                        // Tool errors are informational - agent can handle them
                                        info!("Tool '{}' failed: {}", name, e);
                                        format!("Error: {e}")
                                    }
                                }
                            } else {
                                format!("Tool '{name}' not found")
                            };

                        let duration_ms = start_time.elapsed().as_millis() as u64;

                        // Add tool result to session
                        {
                            let mut s = session.write().await;
                            s.add_tool_result(id, name, &tool_result).await?;
                        }

                        // Emit tool end event
                        on_event(AgenticEvent::ToolEnd {
                            run_id: run_id.clone(),
                            tool_id: id.clone(),
                            result: serde_json::json!(&tool_result),
                            success: !tool_result.starts_with("Error:"),
                            duration_ms,
                        });

                        // Build tool result message using proper ToolResult block
                        let result_msg = ChatMessage {
                            role: MessageRole::Tool,
                            content: vec![ContentBlock::ToolResult {
                                tool_call_id: id.clone(),
                                name: name.clone(),
                                content: vec![ContentBlock::Text {
                                    text: tool_result.clone(),
                                }],
                                is_error: tool_result.starts_with("Error:"),
                            }],
                            tool_calls: None,
                            tool_call_id: Some(id.clone()),
                        };
                        tool_results.push(result_msg);
                    }
                }

                // Add tool results to messages
                info!("Adding {} tool results to messages", tool_results.len());
                messages.extend(tool_results);
                info!(
                    "Messages now has {} items: {:?}",
                    messages.len(),
                    messages
                        .iter()
                        .map(|m| format!("{:?}", m.role))
                        .collect::<Vec<_>>()
                );

                // Continue to next iteration for final answer
                continue;
            }

            // No tool calls - this is the final answer
            let final_text = text_parts.join(" ");
            info!("Final answer received after {} iterations", iteration);

            // Add final answer to session
            {
                let mut s = session.write().await;
                s.add_assistant(&final_text, None, Some(response.usage.clone())).await?;
            }

            // Emit final assistant event
            sequence_counter += 1;
            on_event(AgenticEvent::AssistantText {
                run_id: run_id.clone(),
                text: final_text.clone(),
                sequence: sequence_counter,
                is_interstitial: false, // This is the final answer
            });

            // Emit usage event
            on_event(AgenticEvent::Usage {
                run_id: run_id.clone(),
                prompt_tokens: total_usage.input as u32,
                completion_tokens: total_usage.output as u32,
                total_tokens: total_usage.total as u32,
            });

            // Emit end event
            on_event(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::End,
                error: None,
            });

            return Ok(AgenticResult {
                success: true,
                final_answer: final_text,
                tool_calls: response.tool_calls,
                iterations: iteration,
                usage: total_usage,
            });
        }
    }

    /// Build tool definitions from tool registry
    fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|tool| ToolDefinition {
                name: tool.name().to_string(),
                description: tool.llm_description(),
                parameters: tool.parameters(),
            })
            .collect()
    }

    /// Get the task manager
    #[must_use]
    pub fn task_manager(&self) -> &Arc<TaskManager> {
        &self.task_manager
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
                .map(|m| matches!(m.role, MessageRole::System))
                .unwrap_or(false);
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
        self.run_streaming_loop(messages, session, on_event, run_id, streaming_config)
            .await
    }

    /// Main streaming agent loop logic
    async fn run_streaming_loop(
        &self,
        mut messages: Vec<ChatMessage>,
        session: Arc<RwLock<UnifiedSession>>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        run_id: String,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        use futures::StreamExt;

        // Build tool definitions
        let tool_defs = self.build_tool_definitions();

        let mut iteration = 0;
        let mut total_usage = crate::providers::TokenUsage::default();

        loop {
            iteration += 1;
            info!("Streaming agent loop: iteration {}", iteration);

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

            // Check if provider supports streaming
            if !self.provider.supports_native_tools() {
                // Fall back to non-streaming mode
                warn!("Provider doesn't support streaming, falling back to blocking mode");
                return self.run_loop(messages, session, on_event, run_id).await;
            }

            // Create orchestrator for this iteration
            let mut orchestrator =
                crate::engine::StreamOrchestrator::new(&run_id, streaming_config.clone());

            // Get streaming response
            let mut stream = self
                .provider
                .stream_with_tools(&messages, &tool_defs, &options)
                .await?;

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
                                    crate::providers::StreamEvent::ToolCallEnd { tool_call, .. } => {
                                        tool_calls.push(tool_call);
                                    }
                                    crate::providers::StreamEvent::Done {
                                        stop_reason: reason,
                                    } => {
                                        stop_reason = reason;
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
                        None, // TODO: Track usage from streaming
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

                        on_event(AgenticEvent::ToolStart {
                            run_id: run_id.clone(),
                            tool_id: id.clone(),
                            name: name.clone(),
                            params: arguments.clone(),
                        });

                        let start_time = std::time::Instant::now();
                        let tool_result =
                            if let Some(tool) = self.tools.iter().find(|t| t.name() == name) {
                                match self
                                    .task_manager
                                    .execute(Arc::clone(tool), arguments.clone(), None)
                                    .await
                                {
                                    Ok(result) => {
                                        info!("Tool '{}' executed successfully", name);
                                        result.to_string()
                                    }
                                    Err(e) => {
                                        info!("Tool '{}' failed: {}", name, e);
                                        format!("Error: {e}")
                                    }
                                }
                            } else {
                                format!("Tool '{name}' not found")
                            };

                        let duration_ms = start_time.elapsed().as_millis() as u64;

                        // Add to session
                        {
                            let mut s = session.write().await;
                            s.add_tool_result(id, name, &tool_result).await?;
                        }

                        on_event(AgenticEvent::ToolEnd {
                            run_id: run_id.clone(),
                            tool_id: id.clone(),
                            result: serde_json::json!(&tool_result),
                            success: !tool_result.starts_with("Error:"),
                            duration_ms,
                        });

                        // Add tool result to messages
                        messages.push(ChatMessage {
                            role: MessageRole::Tool,
                            content: vec![ContentBlock::ToolResult {
                                tool_call_id: id.clone(),
                                name: name.clone(),
                                content: vec![ContentBlock::Text {
                                    text: tool_result.clone(),
                                }],
                                is_error: tool_result.starts_with("Error:"),
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
                s.add_assistant(&accumulated_text, None, None).await?;
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
}

/// Build system prompt from agent and tools using `SystemPromptBuilder`
/// Includes bootstrap file injection (AGENTS.md, SOUL.md, etc.) and skills
fn build_system_prompt(agent: &Agent, tools: &[Arc<dyn Tool>]) -> String {
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

    // Load skills from the skills directory
    let skills = load_agent_skills(agent.name());

    SystemPromptBuilder::new(agent.name())
        .with_mode(PromptMode::Full)
        .with_workspace(&workspace_dir)
        .with_tools(tools.to_vec())
        .with_skills(skills)
        .build()
}

/// Load skills for an agent from the skills directory
fn load_agent_skills(agent_name: &str) -> Vec<crate::skills::Skill> {
    // Try agent-specific skills first, then global skills
    let skills_dir = dirs::home_dir()
        .map(|h| h.join(".pekobot").join("skills"))
        .or_else(|| dirs::data_dir().map(|d| d.join("pekobot").join("skills")))
        .unwrap_or_else(|| PathBuf::from("./skills"));

    let mut registry = crate::skills::SkillsRegistry::new(&skills_dir);

    if let Err(e) = registry.load_all() {
        tracing::warn!("Failed to load skills for agent {}: {}", agent_name, e);
        return Vec::new();
    }

    registry.list().into_iter().cloned().collect()
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

/// Extract tool calls from `ContentBlock` for session storage
fn extract_tool_calls(blocks: &[ContentBlock]) -> Vec<ToolCall> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolCall {
                id: _,
                name,
                arguments,
            } => Some(ToolCall {
                name: name.clone(),
                parameters: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
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
mod tests {
    use super::*;

    #[test]
    fn test_extract_tool_calls() {
        let blocks = vec![
            ContentBlock::Text {
                text: "Let me search".to_string(),
            },
            ContentBlock::ToolCall {
                id: "call_1".to_string(),
                name: "web_search".to_string(),
                arguments: serde_json::json!({"query": "test"}),
            },
        ];

        let calls = extract_tool_calls(&blocks);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].parameters, serde_json::json!({"query": "test"}));
    }
}
