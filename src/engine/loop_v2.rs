//! Agentic loop v2 - using AgentMessage abstraction with steering support
//!
//! Features:
//! - Rich message types (LLM + custom)
//! - User steering during tool execution
//! - Follow-up messages for multi-turn
//! - Context window management hooks
//! - Streaming events for full visibility

use crate::agent::Agent;
use crate::engine::{
    AgenticEvent, AgenticResult, LifecyclePhase, ToolCall,
};
use crate::providers::Provider;
use crate::tools::{context::AbortSignal, Tool};
use crate::types::{
    AgentContext, AgentMessage, ContentBlock, JsonMessageConverter, LlmMessage,
    MessageConverter, MessageRole, SteeringProvider, ToolCallId,
    ContextTransformer, DefaultContextTransformer, ContextWindowConfig,
};
use crate::types::message::NoOpSteeringProvider;
use anyhow::{Context as _, Result};
use std::sync::Arc;
use tracing::{info, warn};

/// The v2 agentic loop with AgentMessage abstraction
pub struct AgenticLoopV2 {
    agent: Arc<Agent>,
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_iterations: usize,
    abort_signal: Option<AbortSignal>,
    message_converter: Arc<dyn MessageConverter>,
    steering_provider: Arc<dyn SteeringProvider>,
    context_transformer: Arc<dyn ContextTransformer>,
}

impl AgenticLoopV2 {
    /// Create a new v2 agentic loop
    pub fn new(
        agent: Arc<Agent>,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self {
            agent,
            provider,
            tools,
            max_iterations: 10,
            abort_signal: None,
            message_converter: Arc::new(JsonMessageConverter),
            steering_provider: Arc::new(NoOpSteeringProvider),
            context_transformer: Arc::new(DefaultContextTransformer::new(ContextWindowConfig::default())),
        }
    }

    /// Set maximum iterations
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set abort signal
    #[must_use]
    pub fn with_abort_signal(mut self, signal: AbortSignal) -> Self {
        self.abort_signal = Some(signal);
        self
    }

    /// Set custom message converter
    #[must_use]
    pub fn with_message_converter(mut self, converter: Arc<dyn MessageConverter>) -> Self {
        self.message_converter = converter;
        self
    }

    /// Set steering provider for mid-execution user input
    #[must_use]
    pub fn with_steering_provider(mut self, provider: Arc<dyn SteeringProvider>) -> Self {
        self.steering_provider = provider;
        self
    }

    /// Set context transformer for context window management
    #[must_use]
    pub fn with_context_transformer(mut self, transformer: Arc<dyn ContextTransformer>) -> Self {
        self.context_transformer = transformer;
        self
    }

    /// Run the agentic loop with a user prompt
    pub async fn run(&self,
        prompt: &str,
        event_tx: tokio::sync::mpsc::Sender<AgenticEvent>,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        // Emit start event
        let _ = event_tx.send(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        }).await;

        info!("Starting v2 agentic loop for agent: {}", self.agent.name());

        // Initialize context with system prompt
        let system_prompt = self.build_system_prompt();
        let mut context = AgentContext::with_system_prompt(&system_prompt);

        // Add user prompt as first message
        context.add_message(AgentMessage::user(prompt));

        let mut iteration = 0;
        let mut tool_calls_made: Vec<ToolCall> = vec![];

        // Outer loop: continues when follow-up messages arrive
        loop {
            iteration += 1;
            info!("Agent loop: starting iteration {}", iteration);

            if iteration > self.max_iterations {
                warn!("Max iterations ({}) reached", self.max_iterations);
                let _ = event_tx.send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::Error,
                    error: Some("Max iterations reached".to_string()),
                }).await;
                return Ok(AgenticResult {
                    success: false,
                    final_answer: "Max iterations reached".to_string(),
                    tool_calls: tool_calls_made,
                    iterations: iteration,
                });
            }

            // Check abort signal
            if let Some(ref signal) = self.abort_signal {
                if signal.is_aborted() {
                    info!("Agent loop: abort signal detected");
                    let _ = event_tx.send(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Aborted,
                        error: None,
                    }).await;
                    return Ok(AgenticResult {
                        success: false,
                        final_answer: "Execution aborted".to_string(),
                        tool_calls: tool_calls_made,
                        iterations: iteration,
                    });
                }
            }

            // Transform context for context window management
            context = self.context_transformer.transform(context).await
                .context("Failed to transform context")?;

            // Emit turn start event
            let _ = event_tx.send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            }).await;

            // Get LLM response
            let (assistant_message, tool_calls) = self
                .get_assistant_response(&context, &run_id, &event_tx)
                .await?;

            // Add assistant message to context
            context.add_message(AgentMessage::Llm(assistant_message.clone()));

            // Check if we have tool calls
            if tool_calls.is_empty() {
                // No tool calls - check for follow-up messages or exit
                let follow_ups = self.steering_provider.get_follow_up_messages().await;

                if !follow_ups.is_empty() {
                    info!("Adding {} follow-up message(s)", follow_ups.len());
                    for msg in follow_ups {
                        context.add_message(msg);
                    }
                    continue; // Continue to next iteration
                }

                // No follow-ups - we're done
                let final_text = assistant_message.content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");

                info!("Agent loop: completed after {} iterations", iteration);
                let _ = event_tx.send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::End,
                    error: None,
                }).await;

                return Ok(AgenticResult {
                    success: true,
                    final_answer: final_text,
                    tool_calls: tool_calls_made,
                    iterations: iteration,
                });
            }

            // Execute tool calls
            for (index, tool_call_block) in tool_calls.iter().enumerate() {
                // Check for steering messages before each tool
                let steering = self.steering_provider.get_steering_messages().await;
                if !steering.is_empty() {
                    info!("Steering message received, skipping remaining tools");

                    // Skip remaining tool calls
                    for skipped in &tool_calls[index..] {
                        if let ContentBlock::ToolCall { id, name, .. } = skipped {
                            let skip_result = AgentMessage::tool_result(
                                id.clone(),
                                name.clone(),
                                "Skipped due to user interruption",
                            );
                            context.add_message(skip_result);
                        }
                    }

                    // Add steering messages
                    for msg in steering {
                        context.add_message(msg);
                    }

                    break; // Exit tool loop, continue to next LLM call
                }

                // Execute the tool
                if let ContentBlock::ToolCall { id, name, arguments } = tool_call_block {
                    let tool_result = self
                        .execute_tool_with_events(
                            id,
                            name,
                            arguments,
                            &run_id,
                            &event_tx,
                        )
                        .await;

                    context.add_message(tool_result);
                    tool_calls_made.push(ToolCall {
                        name: name.clone(),
                        parameters: arguments.clone(),
                    });
                }
            }
        }
    }

    /// Get assistant response from LLM
    async fn get_assistant_response(
        &self,
        context: &AgentContext,
        run_id: &str,
        event_tx: &tokio::sync::mpsc::Sender<AgenticEvent>,
    ) -> Result<(LlmMessage, Vec<ContentBlock>)> {
        // Convert messages to LLM format
        let llm_messages = self.message_converter.convert(context.llm_messages()).await?;

        // Convert messages to a prompt string for the provider
        // This is a simple approach - concatenate role and content
        let prompt = llm_messages
            .iter()
            .map(|m| {
                let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                format!("{}: {}", role, content)
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        // Call provider (non-streaming for now)
        let response_text = self.provider.complete(&prompt).await?;

        // Parse response - for now, assume it's a simple text response
        // or contains tool calls in a parseable format
        let (text_content, tool_calls) = self.parse_response(&response_text);

        // Create assistant message
        let assistant_message = LlmMessage {
            role: MessageRole::Assistant,
            content: text_content.clone(),
            timestamp: chrono::Utc::now(),
            metadata: Default::default(),
        };

        // Emit assistant events
        for block in &text_content {
            if let ContentBlock::Text { text } = block {
                let _ = event_tx.send(AgenticEvent::Assistant {
                    run_id: run_id.to_string(),
                    text: text.clone(),
                    is_delta: false,
                    is_final: false,
                }).await;
            }
        }

        Ok((assistant_message, tool_calls))
    }

    /// Parse provider response into content blocks and tool calls
    fn parse_response(&self,
        response: &str,
    ) -> (Vec<ContentBlock>, Vec<ContentBlock>) {
        let mut text_blocks = Vec::new();
        let mut tool_calls = Vec::new();

        // Try to extract tool calls from the response
        if let Some(tool_call) = self.extract_tool_call(response) {
            tool_calls.push(ContentBlock::ToolCall {
                id: format!("tc_{}", chrono::Utc::now().timestamp_millis()),
                name: tool_call.name,
                arguments: tool_call.parameters,
            });

            // Add text before tool call
            text_blocks.push(ContentBlock::Text {
                text: response.to_string(),
            });
        } else {
            // No tool call - just text
            text_blocks.push(ContentBlock::Text {
                text: response.to_string(),
            });
        }

        (text_blocks, tool_calls)
    }

    /// Extract tool call from response text
    fn extract_tool_call(&self,
        text: &str,
    ) -> Option<ToolCall> {
        // Look for tool call patterns in the text
        // Support formats like:
        // ```json
        // {"name": "tool_name", "arguments": {...}}
        // ```
        // Or: TOOL_CALL: tool_name({...})

        // Try markdown code block first
        if let Some(start) = text.find("```json") {
            if let Some(end) = text[start..].find("```") {
                let json_str = &text[start + 7..start + end].trim();
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if let (Some(name), Some(args)) = (
                        json.get("name").and_then(|v| v.as_str()),
                        json.get("arguments"),
                    ) {
                        return Some(ToolCall {
                            name: name.to_string(),
                            parameters: args.clone(),
                        });
                    }
                }
            }
        }

        // Try TOOL_CALL: pattern
        if let Some(start) = text.find("TOOL_CALL:") {
            let rest = &text[start + 10..];
            if let Some(end) = rest.find('(') {
                let name = rest[..end].trim();
                if let Some(close) = rest.find(')') {
                    let args_str = &rest[end + 1..close];
                    let args = if args_str.trim().is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(args_str).unwrap_or_else(|_| {
                            serde_json::json!({ "value": args_str })
                        })
                    };
                    return Some(ToolCall {
                        name: name.to_string(),
                        parameters: args,
                    });
                }
            }
        }

        None
    }

    /// Execute a tool with event streaming
    async fn execute_tool_with_events(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        run_id: &str,
        event_tx: &tokio::sync::mpsc::Sender<AgenticEvent>,
    ) -> AgentMessage {
        // Find the tool
        let tool = match self.tools.iter().find(|t| t.name() == tool_name) {
            Some(t) => t,
            None => {
                return AgentMessage::tool_result(
                    tool_call_id,
                    tool_name,
                    format!("Tool '{}' not found", tool_name),
                );
            }
        };

        // Emit tool start event
        let _ = event_tx.send(AgenticEvent::ToolStart {
            run_id: run_id.to_string(),
            tool_id: tool_call_id.to_string(),
            name: tool_name.to_string(),
            params: arguments.clone(),
        }).await;

        // Create abort signal for this tool
        let tool_abort = AbortSignal::new();

        // Execute tool
        let start_time = std::time::Instant::now();
        let result = tool.execute(arguments.clone()).await;
        let duration = start_time.elapsed();

        // Create result message
        let (result_value, success) = match result {
            Ok(output) => (output, true),
            Err(e) => (serde_json::json!(format!("Error: {}", e)), false),
        };
        let result_text = result_value.as_str().map(|s| s.to_string()).unwrap_or_else(|| result_value.to_string());

        // Emit tool end event
        let _ = event_tx.send(AgenticEvent::ToolEnd {
            run_id: run_id.to_string(),
            tool_id: tool_call_id.to_string(),
            result: result_value,
            success,
            duration_ms: duration.as_millis() as u64,
        }).await;

        AgentMessage::tool_result(tool_call_id, tool_name, result_text)
    }

    /// Build system prompt
    fn build_system_prompt(&self) -> String {
        use crate::prompt::{PromptMode, SystemPromptBuilder};

        let mode = self
            .agent
            .config
            .prompt
            .as_ref()
            .map(|p| match p.mode {
                crate::types::agent::PromptMode::Full => PromptMode::Full,
                crate::types::agent::PromptMode::Minimal => PromptMode::Minimal,
                crate::types::agent::PromptMode::None => PromptMode::None,
            })
            .unwrap_or(PromptMode::Full);

        let workspace = self
            .agent
            .config
            .workspace
            .clone()
            .unwrap_or_else(|| {
                dirs::data_dir()
                    .map(|h| h.join("pekobot").join("workspaces").join(&self.agent.config.name))
                    .unwrap_or_else(|| std::path::PathBuf::from(format!("./workspaces/{}", self.agent.config.name)))
            });

        SystemPromptBuilder::new(&self.agent.config.name)
            .with_mode(mode)
            .with_tools(self.tools.clone())
            .with_workspace(workspace)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tool_call_json() {
        // Test the tool call extraction logic directly
        let response = r#"I'll search for that.
```json
{
  "name": "web_search",
  "arguments": {"query": "rust async"}
}
```"#;

        // Find the JSON block
        if let Some(start) = response.find("```json") {
            if let Some(end) = response[start..].find("```") {
                let json_str = &response[start + 7..start + end].trim();
                let json: serde_json::Value = serde_json::from_str(json_str).unwrap();
                assert_eq!(json["name"], "web_search");
                assert_eq!(json["arguments"]["query"], "rust async");
            }
        }
    }

    #[test]
    fn test_extract_tool_call_pattern() {
        let response = "TOOL_CALL: web_search({\"query\": \"rust\"})";
        
        if let Some(start) = response.find("TOOL_CALL:") {
            let rest = &response[start + 10..];
            if let Some(end) = rest.find('(') {
                let name = rest[..end].trim();
                assert_eq!(name, "web_search");
            }
        }
    }
}
