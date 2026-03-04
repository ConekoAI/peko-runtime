//! Agentic loop v3 - with native JSON tool calling and content blocks
//!
//! Based on v1's proven structure with content block support.

use crate::agent::Agent;
use crate::engine::{AgenticEvent, AgenticResult, LifecyclePhase, SimpleSession, ToolCall};
use crate::prompt::{PromptMode, SystemPromptBuilder};
use crate::providers::Provider;
use crate::tools::{context::AbortSignal, Tool};
use crate::types::ContentBlock;
use anyhow::{Context as _, Result};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// v3 agentic loop with native JSON tool calling
pub struct AgenticLoopV3 {
    agent: Arc<Agent>,
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_iterations: usize,
    abort_signal: Option<AbortSignal>,
}

impl AgenticLoopV3 {
    /// Create a new v3 agentic loop
    pub fn new(agent: Arc<Agent>, provider: Arc<dyn Provider>, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            agent,
            provider,
            tools,
            max_iterations: 10,
            abort_signal: None,
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

    /// Run with streaming support and session persistence
    pub async fn run_streaming(
        &self,
        prompt: &str,
        event_tx: tokio::sync::mpsc::Sender<AgenticEvent>,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        // Create session (mandatory persistence)
        let mut session = SimpleSession::create(&self.agent.name())
            .await
            .context("Failed to create session")?;

        // Add system prompt and user message
        let system_prompt = self.build_system_prompt();
        session.add_system(&system_prompt).await?;
        session.add_user(prompt).await?;

        // Emit start event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Start,
                error: None,
            })
            .await;

        info!(
            "Starting v3 agentic loop for agent: {} (session: {})",
            self.agent.name(),
            session.id
        );

        let mut iteration = 0;
        let mut tool_calls_made: Vec<ToolCall> = vec![];

        loop {
            iteration += 1;
            info!("Agent loop: iteration {}", iteration);

            // Check abort signal
            if let Some(ref signal) = self.abort_signal {
                if signal.is_aborted() {
                    info!("Agent loop: abort signal detected");
                    let _ = event_tx
                        .send(AgenticEvent::Lifecycle {
                            run_id: run_id.clone(),
                            phase: LifecyclePhase::Aborted,
                            error: None,
                        })
                        .await;
                    return Ok(AgenticResult {
                        success: false,
                        final_answer: "Execution aborted".to_string(),
                        tool_calls: tool_calls_made,
                        iterations: iteration,
                    });
                }
            }

            if iteration > self.max_iterations {
                warn!("Max iterations ({}) reached", self.max_iterations);
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Error,
                        error: Some("Max iterations reached".to_string()),
                    })
                    .await;
                return Ok(AgenticResult {
                    success: false,
                    final_answer: "Max iterations reached".to_string(),
                    tool_calls: tool_calls_made,
                    iterations: iteration,
                });
            }

            // Emit running event
            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::Running,
                    error: None,
                })
                .await;

            // Get context from session (last 50 entries)
            let context = session.get_context_text(50).await;
            debug!("Context length: {} chars", context.len());

            // Get LLM response
            let response_text = match self.provider.complete(&context).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Provider error: {}", e);
                    let _ = event_tx
                        .send(AgenticEvent::Lifecycle {
                            run_id: run_id.clone(),
                            phase: LifecyclePhase::Error,
                            error: Some(e.to_string()),
                        })
                        .await;
                    return Err(e);
                }
            };

            debug!("Provider response: {}", response_text);

            // Parse response as JSON with content blocks
            let content_blocks = self.parse_response(&response_text);

            // Process content blocks
            let mut thinking_text = String::new();
            let mut text_response = String::new();
            let mut tool_calls: Vec<ToolCall> = vec![];

            for block in &content_blocks {
                match block {
                    ContentBlock::Thinking { text, .. } => {
                        // Stream thinking to user
                        let _ = event_tx
                            .send(AgenticEvent::Assistant {
                                run_id: run_id.clone(),
                                text: text.clone(),
                                is_delta: true,
                                is_final: false,
                            })
                            .await;
                        thinking_text.push_str(text);
                    }
                    ContentBlock::Text { text } => {
                        text_response.push_str(text);
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        tool_calls.push(ToolCall {
                            name: name.clone(),
                            parameters: arguments.clone(),
                        });
                        // Store tool call ID for later matching
                        let _ = id; // TODO: use for matching
                    }
                    _ => {}
                }
            }

            // If we have tool calls, execute them
            if !tool_calls.is_empty() {
                // Add assistant message with tool calls to session
                let assistant_content = if thinking_text.is_empty() {
                    response_text.clone()
                } else {
                    thinking_text.clone()
                };
                session
                    .add_assistant(&assistant_content, Some(tool_calls.clone()))
                    .await?;

                // Execute each tool
                for tool_call in &tool_calls {
                    info!("Executing tool: {}", tool_call.name);

                    // Emit tool start event
                    let tool_id = format!(
                        "{}_{}",
                        tool_call.name,
                        chrono::Utc::now().timestamp_millis()
                    );
                    let _ = event_tx
                        .send(AgenticEvent::ToolStart {
                            run_id: run_id.clone(),
                            tool_id: tool_id.clone(),
                            name: tool_call.name.clone(),
                            params: tool_call.parameters.clone(),
                        })
                        .await;

                    // Find and execute tool
                    let tool_result = if let Some(tool) =
                        self.tools.iter().find(|t| t.name() == tool_call.name)
                    {
                        match tool.execute(tool_call.parameters.clone()).await {
                            Ok(result) => {
                                info!("Tool '{}' executed successfully", tool_call.name);
                                result.to_string()
                            }
                            Err(e) => {
                                error!("Tool '{}' failed: {}", tool_call.name, e);
                                format!("Error: {}", e)
                            }
                        }
                    } else {
                        format!("Tool '{}' not found", tool_call.name)
                    };

                    // Add tool result to session
                    session
                        .add_tool_result(&tool_id, &tool_call.name, &tool_result)
                        .await?;

                    // Emit tool end event
                    let _ = event_tx
                        .send(AgenticEvent::ToolEnd {
                            run_id: run_id.clone(),
                            tool_id: tool_id.clone(),
                            result: serde_json::json!(&tool_result),
                            success: !tool_result.starts_with("Error:"),
                            duration_ms: 0,
                        })
                        .await;

                    tool_calls_made.push(tool_call.clone());
                }

                // Continue to next iteration to get final answer
                continue;
            }

            // No tool calls - this is the final answer
            info!("Final answer received after {} iterations", iteration);

            // Add final answer to session
            session.add_assistant(&text_response, None).await?;

            // Emit final assistant event
            let _ = event_tx
                .send(AgenticEvent::Assistant {
                    run_id: run_id.clone(),
                    text: text_response.clone(),
                    is_delta: false,
                    is_final: true,
                })
                .await;

            // Emit end event
            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::End,
                    error: None,
                })
                .await;

            return Ok(AgenticResult {
                success: true,
                final_answer: text_response,
                tool_calls: tool_calls_made,
                iterations: iteration,
            });
        }
    }

    /// Parse response as JSON with content blocks
    fn parse_response(&self, response: &str) -> Vec<ContentBlock> {
        // Strip markdown code fences if present
        let cleaned = response
            .trim()
            .strip_prefix("```json")
            .or_else(|| response.trim().strip_prefix("```"))
            .and_then(|s| s.strip_suffix("```"))
            .map(|s| s.trim())
            .unwrap_or(response.trim());

        // Try to parse as JSON with content field
        if let Ok(json) = serde_json::from_str::<Value>(cleaned) {
            // Check for content array
            if let Some(content) = json.get("content").and_then(|c| c.as_array()) {
                let mut blocks = vec![];
                for item in content {
                    if let Some(block) = self.parse_content_block(item) {
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
    fn parse_content_block(&self, value: &Value) -> Option<ContentBlock> {
        let block_type = value.get("type")?.as_str()?;

        match block_type {
            "text" => {
                let text = value.get("text")?.as_str()?.to_string();
                Some(ContentBlock::Text { text })
            }
            "thinking" => {
                let thinking = value.get("thinking")?.as_str()?.to_string();
                let signature = value
                    .get("signature")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                Some(ContentBlock::Thinking {
                    text: thinking,
                    signature,
                })
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
            "tool_result" => {
                let tool_call_id = value.get("tool_call_id")?.as_str()?.to_string();
                let name = value.get("name")?.as_str()?.to_string();
                let content = vec![]; // Simplified for now
                let is_error = value
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(ContentBlock::ToolResult {
                    tool_call_id,
                    name,
                    content,
                    is_error,
                })
            }
            _ => None,
        }
    }

    /// Build system prompt
    fn build_system_prompt(&self) -> String {
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

        let workspace = self.agent.config.workspace.clone().unwrap_or_else(|| {
            dirs::data_dir()
                .map(|h| {
                    h.join("pekobot")
                        .join("workspaces")
                        .join(&self.agent.config.name)
                })
                .unwrap_or_else(|| {
                    std::path::PathBuf::from(format!("./workspaces/{}", self.agent.config.name))
                })
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
    fn test_parse_response_with_content_blocks() {
        let loop_v3 = AgenticLoopV3::new(
            Arc::new(Agent::default()),
            Arc::new(crate::providers::MockProvider::new()),
            vec![],
        );

        let response = r#"{"content": [{"type": "thinking", "thinking": "Let me search..."}, {"type": "tool_call", "id": "call_123", "name": "web_search", "arguments": {"query": "rust"}}]}"#;

        let blocks = loop_v3.parse_response(response);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(blocks[1], ContentBlock::ToolCall { .. }));
    }

    #[test]
    fn test_parse_plain_text_fallback() {
        let loop_v3 = AgenticLoopV3::new(
            Arc::new(Agent::default()),
            Arc::new(crate::providers::MockProvider::new()),
            vec![],
        );

        let response = "Just a plain text response";

        let blocks = loop_v3.parse_response(response);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], ContentBlock::Text { .. }));
    }
}
