//! Agentic loop - core execution engine with tool calling
//!
//! Features:
//! - Tool execution with abort signals and progress monitoring
//! - Streaming events for tool lifecycle (start/update/end)
//! - Support for long-running tools with progress updates

use crate::agent::Agent;
use crate::prompt::{PromptMode, SystemPromptBuilder};
use crate::providers::Provider;
use crate::tools::{context::AbortSignal, Tool};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// The main agentic loop that runs an agent with tool calling
pub struct AgenticLoop {
    agent: Arc<Agent>,
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_iterations: usize,
    /// Global abort signal for stopping the entire loop
    abort_signal: Option<AbortSignal>,
}

impl AgenticLoop {
    /// Create a new agentic loop
    pub fn new(agent: Arc<Agent>, provider: Arc<dyn Provider>, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            agent,
            provider,
            tools,
            max_iterations: 10,
            abort_signal: None,
        }
    }

    /// Set maximum iterations for the loop
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set an abort signal for this loop
    ///
    /// When the abort signal is triggered, the current tool execution
    /// will be cancelled and the loop will exit.
    #[must_use]
    pub fn with_abort_signal(mut self, signal: AbortSignal) -> Self {
        self.abort_signal = Some(signal);
        self
    }

    /// Run the agentic loop with a user prompt
    pub async fn run(&self, prompt: &str) -> Result<AgenticResult> {
        info!("Starting agentic loop for agent: {}", self.agent.name());

        let mut iteration = 0;
        let mut context = vec![
            json!({
                "role": "system",
                "content": self.build_system_prompt()
            }),
            json!({
                "role": "user",
                "content": prompt
            }),
        ];

        let mut tool_calls_made: Vec<ToolCall> = vec![];

        loop {
            iteration += 1;
            if iteration > self.max_iterations {
                warn!("Max iterations ({}) reached", self.max_iterations);
                return Ok(AgenticResult {
                    success: false,
                    final_answer: "Max iterations reached".to_string(),
                    tool_calls: tool_calls_made,
                    iterations: iteration,
                });
            }

            // Check if aborted
            if let Some(ref signal) = self.abort_signal {
                if signal.is_aborted() {
                    info!("Agentic loop aborted");
                    return Ok(AgenticResult {
                        success: false,
                        final_answer: "Execution aborted".to_string(),
                        tool_calls: tool_calls_made,
                        iterations: iteration,
                    });
                }
            }

            debug!("Iteration {}: Calling provider", iteration);

            // Call the LLM
            let response = self
                .provider
                .complete(&self.format_messages(&context))
                .await
                .context("Failed to get completion from provider")?;

            debug!("Provider response: {}", response);

            // Parse the response
            if let Some(tool_call) = self.parse_tool_call(&response) {
                // Execute tool
                info!("Executing tool: {}", tool_call.name);
                let tool_result = self.execute_tool(&tool_call).await;
                tool_calls_made.push(tool_call);

                // Add tool call and result to context
                context.push(json!({
                    "role": "assistant",
                    "content": response
                }));
                context.push(json!({
                    "role": "user",
                    "content": format!("Tool result: {}", tool_result)
                }));
            } else if self.is_final_answer(&response) {
                // Final answer reached
                debug!("Final answer received after {} iterations", iteration);
                let answer = self.extract_final_answer(&response);
                return Ok(AgenticResult {
                    success: true,
                    final_answer: answer,
                    tool_calls: tool_calls_made,
                    iterations: iteration,
                });
            } else {
                // Continue the conversation
                context.push(json!({
                    "role": "assistant",
                    "content": response
                }));
            }
        }
    }

    /// Run with streaming support
    ///
    /// This version streams events back via the provided channel.
    /// Features:
    /// - Tool start/update/end events for monitoring
    /// - Abort signal support for cancelling long-running tools
    /// - Progress updates from tools that support them
    pub async fn run_streaming(
        &self,
        prompt: &str,
        event_tx: tokio::sync::mpsc::Sender<crate::engine::AgenticEvent>,
    ) -> Result<AgenticResult> {
        use crate::engine::{AgenticEvent, LifecyclePhase};
        use crate::tools::context::ToolContext;
        use tracing::error;

        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        // Emit start event
        let _ = event_tx.send(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        }).await;

        info!("Starting streaming agentic loop for agent: {}", self.agent.name());

        let mut iteration = 0;
        let mut context = vec![
            json!({
                "role": "system",
                "content": self.build_system_prompt()
            }),
            json!({
                "role": "user",
                "content": prompt
            }),
        ];

        let mut tool_calls_made: Vec<ToolCall> = vec![];

        loop {
            iteration += 1;
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

            // Check if aborted
            if let Some(ref signal) = self.abort_signal {
                if signal.is_aborted() {
                    info!("Agentic loop aborted");
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

            debug!("Iteration {}: Calling provider with streaming", iteration);

            // Emit running event
            let _ = event_tx.send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            }).await;

            // Call the LLM with streaming
            let (tx, mut rx) = tokio::sync::mpsc::channel::<AgenticEvent>(100);
            let provider = Arc::clone(&self.provider);
            let messages = self.format_messages(&context);
            let run_id_clone = run_id.clone();

            // Spawn streaming task
            let stream_handle = tokio::spawn(async move {
                provider.complete_stream(&messages, tx, run_id_clone).await
            });

            // Collect streamed response - FORWARD RAW EVENTS, NO CHUNKING
            let mut full_response = String::new();

            while let Some(event) = rx.recv().await {
                match &event {
                    AgenticEvent::Assistant { text, is_delta, is_final, .. } => {
                        if *is_delta {
                            // Forward raw delta to channel (no chunking here)
                            full_response.push_str(text);
                            let _ = event_tx.send(AgenticEvent::Assistant {
                                run_id: run_id.clone(),
                                text: text.clone(),
                                is_delta: true,
                                is_final: false,
                            }).await;
                        } else if *is_final {
                            // Forward final event
                            let _ = event_tx.send(AgenticEvent::Assistant {
                                run_id: run_id.clone(),
                                text: full_response.clone(),
                                is_delta: false,
                                is_final: true,
                            }).await;
                        }
                    }
                    AgenticEvent::Lifecycle { phase: LifecyclePhase::Error, error, .. } => {
                        if let Some(err) = error {
                            error!("Provider error during streaming: {}", err);
                            return Err(anyhow::anyhow!("Provider error: {}", err));
                        }
                    }
                    _ => {
                        // Forward other events (Lifecycle, etc.)
                        let _ = event_tx.send(event).await;
                    }
                }
            }

            // Wait for streaming to complete
            stream_handle.await??;

            debug!("Provider response: {}", full_response);

            // Parse the response
            if let Some(tool_call) = self.parse_tool_call(&full_response) {
                // Generate unique tool execution ID
                let tool_id = format!("{}_{}", tool_call.name, chrono::Utc::now().timestamp_millis());

                // Emit tool start event
                let _ = event_tx.send(AgenticEvent::ToolStart {
                    run_id: run_id.clone(),
                    tool_id: tool_id.clone(),
                    name: tool_call.name.clone(),
                    params: tool_call.parameters.clone(),
                }).await;

                info!("Executing tool: {} (id: {})", tool_call.name, tool_id);

                // Create abort signal for this tool execution
                let tool_abort_signal = AbortSignal::new();

                // Create tool context with event channel for progress updates
                let tool_ctx = tool_abort_signal.create_context_with_events(
                    &run_id,
                    &tool_id,
                    &tool_call.name,
                    event_tx.clone(),
                );

                // Execute tool with context (supports abort + progress)
                let tool_start_time = std::time::Instant::now();
                let tool_result = self.execute_tool_with_context(&tool_call, &tool_ctx).await;
                let duration_ms = tool_start_time.elapsed().as_millis() as u64;

                tool_calls_made.push(tool_call.clone());

                // Emit tool end event
                let success = !tool_result.starts_with("Error:");
                let _ = event_tx.send(AgenticEvent::ToolEnd {
                    run_id: run_id.clone(),
                    tool_id: tool_id.clone(),
                    result: json!(tool_result),
                    success,
                    duration_ms,
                }).await;

                // Add tool call and result to context
                context.push(json!({
                    "role": "assistant",
                    "content": full_response
                }));
                context.push(json!({
                    "role": "user",
                    "content": format!("Tool result: {}", tool_result)
                }));
            } else if self.is_final_answer(&full_response) {
                // Final answer reached
                debug!("Final answer received after {} iterations", iteration);
                let answer = self.extract_final_answer(&full_response);

                // Emit final assistant event
                let _ = event_tx.send(AgenticEvent::Assistant {
                    run_id: run_id.clone(),
                    text: answer.clone(),
                    is_delta: false,
                    is_final: true,
                }).await;

                // Emit end event
                let _ = event_tx.send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::End,
                    error: None,
                }).await;

                return Ok(AgenticResult {
                    success: true,
                    final_answer: answer,
                    tool_calls: tool_calls_made,
                    iterations: iteration,
                });
            } else {
                // Continue the conversation
                context.push(json!({
                    "role": "assistant",
                    "content": full_response
                }));
            }
        }
    }

    /// Build system prompt using the new builder
    fn build_system_prompt(&self) -> String {
        use crate::prompt::builder::{PromptMode, SystemPromptBuilder};

        // Get prompt mode from agent config
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

        // Get workspace from config or use agent-specific default
        let workspace = self
            .agent
            .config
            .workspace
            .clone()
            .unwrap_or_else(|| {
                dirs::data_dir()
                    .map(|h| h.join("pekobot").join("workspaces").join(&self.agent.config.name))
                    .unwrap_or_else(|| PathBuf::from(format!("./workspaces/{}", self.agent.config.name)))
            });

        // Build the prompt using the new builder
        SystemPromptBuilder::new(&self.agent.config.name)
            .with_mode(mode)
            .with_tools(self.tools.clone())
            .with_workspace(workspace)
            .build()
    }

    /// Format context messages for the provider
    fn format_messages(&self, context: &[serde_json::Value]) -> String {
        context
            .iter()
            .map(|m| {
                format!(
                    "{}: {}",
                    m.get("role").and_then(|r| r.as_str()).unwrap_or("unknown"),
                    m.get("content").and_then(|c| c.as_str()).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Parse a tool call from LLM response
    fn parse_tool_call(&self, response: &str) -> Option<ToolCall> {
        if let Some(start) = response.find("TOOL_CALL:") {
            let json_str = &response[start + "TOOL_CALL:".len()..];
            if let Ok(call) = serde_json::from_str::<ToolCall>(json_str.trim()) {
                return Some(call);
            }
        }
        None
    }

    /// Check if response is a final answer
    fn is_final_answer(&self, response: &str) -> bool {
        response.contains("FINAL_ANSWER:")
    }

    /// Extract final answer from response
    fn extract_final_answer(&self, response: &str) -> String {
        if let Some(start) = response.find("FINAL_ANSWER:") {
            response[start + "FINAL_ANSWER:".len()..].trim().to_string()
        } else {
            response.to_string()
        }
    }

    /// Execute a tool (basic version without streaming)
    async fn execute_tool(&self, call: &ToolCall) -> String {
        for tool in &self.tools {
            if tool.name() == call.name {
                match tool.execute(call.parameters.clone()).await {
                    Ok(result) => return serde_json::to_string(&result).unwrap_or_default(),
                    Err(e) => return format!("Error: {e}"),
                }
            }
        }
        format!("Tool '{}' not found", call.name)
    }

    /// Execute a tool with full context support (abort + progress + timeout)
    ///
    /// This is the enhanced version that supports:
    /// - Abort signals for cancelling long-running tools
    /// - Progress updates via ToolUpdate events
    /// - Timeout handling
    /// - Duration tracking
    async fn execute_tool_with_context(
        &self,
        call: &ToolCall,
        ctx: &crate::tools::context::ToolContext,
    ) -> String {
        use crate::tools::ToolError;

        for tool in &self.tools {
            if tool.name() == call.name {
                // Check if tool supports context-aware execution
                if tool.supports_progress() {
                    // Use the context-aware method for tools that support it
                    match tool.execute_with_context(call.parameters.clone(), ctx).await {
                        Ok(result) => return serde_json::to_string(&result).unwrap_or_default(),
                        Err(e) => {
                            // Check if this is a known tool error
                            match e.downcast_ref::<ToolError>() {
                                Some(ToolError::Aborted) => {
                                    return "Error: Tool execution aborted".to_string();
                                }
                                Some(ToolError::Timeout(d)) => {
                                    return format!("Error: Tool execution timed out after {:?}", d);
                                }
                                _ => return format!("Error: {e}"),
                            }
                        }
                    }
                } else {
                    // Fall back to basic execution for tools without progress support
                    // Still check abort before and after
                    if ctx.is_aborted() {
                        return "Error: Tool execution aborted".to_string();
                    }

                    ctx.report_status(format!("Running {}", tool.name())).await;

                    match tool.execute(call.parameters.clone()).await {
                        Ok(result) => {
                            if ctx.is_aborted() {
                                return "Error: Tool execution aborted".to_string();
                            }
                            ctx.report_status(format!("Completed {}", tool.name())).await;
                            return serde_json::to_string(&result).unwrap_or_default();
                        }
                        Err(e) => {
                            if ctx.is_aborted() {
                                return "Error: Tool execution aborted".to_string();
                            }
                            return format!("Error: {e}");
                        }
                    }
                }
            }
        }
        format!("Tool '{}' not found", call.name)
    }
}

/// Result of running the agentic loop
#[derive(Debug, Clone)]
pub struct AgenticResult {
    /// Whether execution succeeded
    pub success: bool,
    /// Final answer from the agent
    pub final_answer: String,
    /// Tool calls made during execution
    pub tool_calls: Vec<ToolCall>,
    /// Number of iterations
    pub iterations: usize,
}

/// A tool call parsed from LLM response
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCall {
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_call() {
        // Note: Would need async runtime and mock setup for full test
    }

    #[test]
    fn test_is_final_answer() {
        // Note: Would need async runtime and mock setup for full test
    }
}
