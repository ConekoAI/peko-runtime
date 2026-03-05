//! Agentic loop v4 - with native tool calling via provider APIs
//!
//! This is a complete rewrite using the native tool calling approach:
//! - No text parsing for tool calls
//! - Structured ContentBlock types throughout
//! - Unified event callback API (no separate run/run_streaming)
//! - Streaming support with incremental tool call construction

use crate::agent::Agent;
use crate::engine::{AgenticEvent, LifecyclePhase, SimpleSession};
use crate::providers::{
    ChatMessage, ChatOptions, MessageRole, StopReason, StreamEvent, ToolDefinition,
};
use crate::tools::Tool;
use crate::types::message::{ContentBlock, LlmMessage};
use anyhow::{Context as _, Result};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

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
        }
    }

    /// Set maximum iterations
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run the agent with a user prompt.
    ///
    /// The `on_event` callback receives streaming events (thinking tokens,
    /// tool calls, etc.). Use `|_| {}` if you don't need streaming.
    ///
    /// Returns the final result including the answer and tool calls made.
    pub async fn run(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        // Create session (mandatory persistence)
        let mut session = SimpleSession::create(&self.agent.name())
            .await
            .context("Failed to create session")?;

        // Emit start event
        on_event(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        });

        info!(
            "Starting v4 agentic loop for agent: {} (session: {})",
            self.agent.name(),
            session.id
        );

        // Build initial messages
        let mut messages = vec![ChatMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: self.system_prompt.clone(),
            }],
            tool_calls: None,
            tool_call_id: None,
        }];

        // Add user message
        messages.push(ChatMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: prompt.to_string() }],
            tool_calls: None,
            tool_call_id: None,
        });

        // Add user message to session
        session.add_user(prompt).await?;

        // Build tool definitions
        let tool_defs = self.build_tool_definitions();

        let mut iteration = 0;
        let mut total_usage = crate::providers::TokenUsage::default();

        loop {
            iteration += 1;
            info!("Agent loop: iteration {}", iteration);

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
                let content_preview: String = msg.content.iter().map(|b| match b {
                    crate::types::message::ContentBlock::Text { text } => format!("[Text: {}]", text.chars().take(50).collect::<String>()),
                    crate::types::message::ContentBlock::ToolCall { id, name, .. } => format!("[ToolCall: {} ({})]", name, id),
                    crate::types::message::ContentBlock::ToolResult { tool_call_id, name, .. } => format!("[ToolResult: {} -> {}]", tool_call_id, name),
                    _ => "[Other]".to_string(),
                }).collect();
                debug!("  [{}] {:?}: {}", i, msg.role, content_preview);
            }

            let response = if self.provider.supports_native_tools() {
                // Use native tool calling
                self.provider
                    .chat_with_tools(&messages, &tool_defs, &options)
                    .await?
            } else {
                // Fallback to legacy text-based approach
                self.fallback_chat_with_tools(&messages, prompt).await?
            };

            // Accumulate usage
            total_usage.input += response.usage.input;
            total_usage.output += response.usage.output;
            total_usage.total += response.usage.total;

            // Process response content
            let mut text_parts = Vec::new();
            let mut assistant_content = Vec::new();

            for block in &response.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_parts.push(text.clone());
                        assistant_content.push(block.clone());
                    }
                    ContentBlock::Thinking { text, .. } => {
                        // Collect thinking content but don't emit event here
                        // It will be emitted as AgenticEvent::Thinking before tool calls
                        assistant_content.push(block.clone());
                    }
                    _ => {}
                }
            }

            // Handle tool calls
            if !response.tool_calls.is_empty() {
                info!("Processing {} tool calls", response.tool_calls.len());

                // Emit thinking text BEFORE tool calls
                let assistant_text = text_parts.join(" ");
                if !assistant_text.is_empty() {
                    // Emit thinking event for listeners (CLI will display with 💭)
                    on_event(AgenticEvent::Thinking {
                        run_id: run_id.clone(),
                        text: assistant_text.clone(),
                        is_delta: false, // Complete text, not incremental
                        is_final: true,
                        signature: None,
                    });
                }

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

                // Add to session with original tool call IDs
                let assistant_text = text_parts.join(" ");
                let tool_call_blocks: Vec<ContentBlock> = response
                    .tool_calls
                    .iter()
                    .filter_map(|tc| {
                        if let ContentBlock::ToolCall { id, name, arguments } = tc {
                            Some(ContentBlock::ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();
                session
                    .add_assistant_with_tool_calls(&assistant_text, tool_call_blocks)
                    .await?;

                // Execute each tool
                let mut tool_results = Vec::new();

                for tool_call in &response.tool_calls {
                    if let ContentBlock::ToolCall { id, name, arguments } = tool_call {
                        info!("Executing tool: {} (id: {})", name, id);

                        // Emit tool start event
                        on_event(AgenticEvent::ToolStart {
                            run_id: run_id.clone(),
                            tool_id: id.clone(),
                            name: name.clone(),
                            params: arguments.clone(),
                        });

                        // Find and execute tool
                        let start_time = std::time::Instant::now();
                        let tool_result = if let Some(tool) =
                            self.tools.iter().find(|t| t.name() == name)
                        {
                            match tool.execute(arguments.clone()).await {
                                Ok(result) => {
                                    info!("Tool '{}' executed successfully", name);
                                    result.to_string()
                                }
                                Err(e) => {
                                    error!("Tool '{}' failed: {}", name, e);
                                    format!("Error: {}", e)
                                }
                            }
                        } else {
                            format!("Tool '{}' not found", name)
                        };

                        let duration_ms = start_time.elapsed().as_millis() as u64;

                        // Add tool result to session
                        session
                            .add_tool_result(id, name, &tool_result)
                            .await?;

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
                info!("Messages now has {} items: {:?}", messages.len(), messages.iter().map(|m| format!("{:?}", m.role)).collect::<Vec<_>>());

                // Continue to next iteration for final answer
                continue;
            }

            // No tool calls - this is the final answer
            let final_text = text_parts.join(" ");
            info!("Final answer received after {} iterations", iteration);

            // Add final answer to session
            session.add_assistant(&final_text, None).await?;

            // Emit final assistant event
            on_event(AgenticEvent::Assistant {
                run_id: run_id.clone(),
                text: final_text.clone(),
                is_delta: false,
                is_final: true,
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
                description: tool.description().to_string(),
                parameters: tool.parameters(),
            })
            .collect()
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
}

/// Build system prompt from agent and tools
fn build_system_prompt(agent: &Agent, tools: &[Arc<dyn Tool>]) -> String {
    let mut prompt = format!(
        "You are {}, an AI assistant running in the Pekobot agent runtime.\n\n",
        agent.name()
    );

    if !tools.is_empty() {
        prompt.push_str("## Available Tools\n\n");
        for tool in tools {
            prompt.push_str(&format!("- `{}`: {}\n", tool.name(), tool.description()));
        }
        prompt.push_str("\n");
    }

    prompt.push_str("## Instructions\n\n");
    prompt.push_str("Think step by step. When you need to use a tool, the system will handle it.\n");
    prompt.push_str("After receiving tool results, provide a final answer to the user.\n");
    prompt.push_str("If a tool returns empty results, do not retry the same tool - just inform the user.\n");
    prompt.push_str("Provide clear, accurate answers. Ask for clarification when uncertain.\n");

    prompt
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
                .collect::<Vec<_>>()
                .join("");
            format!("{}: {}", role, content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract tool calls from ContentBlock for session storage
fn extract_tool_calls(blocks: &[ContentBlock]) -> Vec<ToolCall> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolCall { id: _, name, arguments } => {
                Some(ToolCall {
                    name: name.clone(),
                    parameters: arguments.clone(),
                })
            }
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
                .map(|s| s.to_string());
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
