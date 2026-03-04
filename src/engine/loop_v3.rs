//! Agentic loop v3 - with mandatory session persistence
//!
//! Based on v1's proven structure with mandatory session management.

use crate::agent::Agent;
use crate::engine::{
    AgenticEvent, AgenticResult, LifecyclePhase, ToolCall, SimpleSession,
};
use crate::prompt::{PromptMode, SystemPromptBuilder};
use crate::providers::Provider;
use crate::tools::{context::AbortSignal, Tool};
use anyhow::{Context as _, Result};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// v3 agentic loop with mandatory session persistence
pub struct AgenticLoopV3 {
    agent: Arc<Agent>,
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_iterations: usize,
    abort_signal: Option<AbortSignal>,
}

impl AgenticLoopV3 {
    /// Create a new v3 agentic loop
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
        let mut session = SimpleSession::create(&self.agent.name()).await
            .context("Failed to create session")?;
        
        // Add system prompt and user message
        let system_prompt = self.build_system_prompt();
        session.add_system(&system_prompt).await?;
        session.add_user(prompt).await?;

        // Emit start event
        let _ = event_tx.send(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        }).await;

        info!("Starting v3 agentic loop for agent: {} (session: {})", 
              self.agent.name(), session.id);

        let mut iteration = 0;
        let mut tool_calls_made: Vec<ToolCall> = vec![];

        loop {
            iteration += 1;
            info!("Agent loop: iteration {}", iteration);

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

            // Emit running event
            let _ = event_tx.send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            }).await;

            // Get context from session (last 50 entries)
            let context = session.get_context_text(50);
            debug!("Context length: {} chars", context.len());

            // Get LLM response
            let response = match self.provider.complete(&context).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Provider error: {}", e);
                    let _ = event_tx.send(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Error,
                        error: Some(e.to_string()),
                    }).await;
                    return Err(e);
                }
            };

            debug!("Provider response: {}", response);

            // Parse for tool calls
            if let Some(tool_call) = self.parse_tool_call(&response) {
                info!("Executing tool: {}", tool_call.name);

                // Add assistant message with tool call to session
                session.add_assistant(&response, Some(vec![tool_call.clone()])).await?;

                // Emit tool start event
                let tool_id = format!("{}_{}", tool_call.name, chrono::Utc::now().timestamp_millis());
                let _ = event_tx.send(AgenticEvent::ToolStart {
                    run_id: run_id.clone(),
                    tool_id: tool_id.clone(),
                    name: tool_call.name.clone(),
                    params: tool_call.parameters.clone(),
                }).await;

                // Find and execute tool
                let tool_result = if let Some(tool) = self.tools.iter().find(|t| t.name() == tool_call.name) {
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
                session.add_tool_result(&tool_id, &tool_call.name, &tool_result).await?;

                // Emit tool end event
                let _ = event_tx.send(AgenticEvent::ToolEnd {
                    run_id: run_id.clone(),
                    tool_id: tool_id.clone(),
                    result: serde_json::json!(&tool_result),
                    success: !tool_result.starts_with("Error:"),
                    duration_ms: 0,
                }).await;

                tool_calls_made.push(tool_call);
                
                // Continue to next iteration to get final answer
                continue;
            }

            // No tool call - this is the final answer
            info!("Final answer received after {} iterations", iteration);
            
            // Add final answer to session
            session.add_assistant(&response, None).await?;
            
            // Emit final assistant event
            let _ = event_tx.send(AgenticEvent::Assistant {
                run_id: run_id.clone(),
                text: response.clone(),
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
                final_answer: response,
                tool_calls: tool_calls_made,
                iterations: iteration,
            });
        }
    }

    /// Parse a tool call from LLM response
    fn parse_tool_call(&self,
        response: &str,
    ) -> Option<ToolCall> {
        if let Some(start) = response.find("TOOL_CALL:") {
            let json_str = response[start + "TOOL_CALL:".len()..].trim();
            
            // Try JSON object format first: {"name": "...", "parameters": {...}}
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
                    let params = json.get("parameters")
                        .or_else(|| json.get("arguments"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    return Some(ToolCall {
                        name: name.to_string(),
                        parameters: params,
                    });
                }
            }
            
            // Try legacy format: tool_name({...})
            if let Some(paren) = json_str.find('(') {
                let name = json_str[..paren].trim();
                if let Some(close) = json_str.find(')') {
                    let args_str = &json_str[paren + 1..close];
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
    fn test_parse_tool_call_json() {
        let loop_v3 = AgenticLoopV3::new(
            Arc::new(Agent::default()),
            Arc::new(crate::providers::MockProvider::new()),
            vec![],
        );

        let response = r#"I'll search for that.
TOOL_CALL: {"name": "web_search", "parameters": {"query": "rust async"}}"#;

        let tool_call = loop_v3.parse_tool_call(response);
        assert!(tool_call.is_some());
        let tc = tool_call.unwrap();
        assert_eq!(tc.name, "web_search");
    }

    #[test]
    fn test_parse_tool_call_legacy() {
        let loop_v3 = AgenticLoopV3::new(
            Arc::new(Agent::default()),
            Arc::new(crate::providers::MockProvider::new()),
            vec![],
        );

        let response = "TOOL_CALL: filesystem({\"path\": \"/tmp\"})";
        
        let tool_call = loop_v3.parse_tool_call(response);
        assert!(tool_call.is_some());
        let tc = tool_call.unwrap();
        assert_eq!(tc.name, "filesystem");
    }
}
