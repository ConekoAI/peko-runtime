//! Agentic loop - core execution engine with tool calling

use crate::agent::Agent;
use crate::providers::Provider;
use crate::tools::Tool;
use anyhow::{Context, Result};
use serde_json::json;
use tracing::{debug, info, warn};

/// The main agentic loop that runs an agent with tool calling
pub struct AgenticLoop {
    agent: Agent,
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    max_iterations: usize,
}

impl AgenticLoop {
    /// Create a new agentic loop
    pub fn new(agent: Agent, provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Self {
        Self {
            agent,
            provider,
            tools,
            max_iterations: 10,
        }
    }

    /// Set maximum iterations for the loop
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run the agentic loop with a user prompt
    pub async fn run(&mut self, prompt: &str) -> Result<AgenticResult> {
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

        loop {
            iteration += 1;
            if iteration > self.max_iterations {
                warn!("Max iterations ({}) reached", self.max_iterations);
                return Ok(AgenticResult {
                    success: false,
                    final_answer: "Max iterations reached".to_string(),
                    tool_calls: vec![],
                    iterations: iteration,
                });
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
                info!("Final answer received after {} iterations", iteration);
                return Ok(AgenticResult {
                    success: true,
                    final_answer: response,
                    tool_calls: vec![],
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

    /// Build system prompt with available tools
    fn build_system_prompt(&self) -> String {
        let tool_descriptions: Vec<String> = self
            .tools
            .iter()
            .map(|t| format!("- {}: {}", t.name(), t.description()))
            .collect();

        format!(
            r#"You are an autonomous agent. You have access to the following tools:

{}

When you need to use a tool, respond in this exact format:
TOOL_CALL: {{"name": "tool_name", "parameters": {{"key": "value"}}}}

When you have a final answer, respond with:
FINAL_ANSWER: your answer here

Think step by step. Use tools when needed. Always end with FINAL_ANSWER."#,
            tool_descriptions.join("\n")
        )
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

    /// Execute a tool
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
}

/// Result of running the agentic loop
#[derive(Debug)]
pub struct AgenticResult {
    pub success: bool,
    pub final_answer: String,
    pub tool_calls: Vec<ToolCall>,
    pub iterations: usize,
}

/// A tool call parsed from LLM response
#[derive(Debug, serde::Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub parameters: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Needs refactoring for async Agent::new"]
    fn test_parse_tool_call() {
        // Test would require async runtime setup
        // let loop_ = AgenticLoop::new(...)
    }

    #[test]
    #[ignore = "Needs refactoring for async Agent::new"]
    fn test_is_final_answer() {
        // Test would require async runtime setup
        // let loop_ = AgenticLoop::new(...)
        // assert!(loop_.is_final_answer("FINAL_ANSWER: The answer is 42"));
        // assert!(!loop_.is_final_answer("Let me think about this..."));
    }
}

// Mock provider for tests
struct MockProvider;

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn complete(&self, _prompt: &str) -> Result<String> {
        Ok("FINAL_ANSWER: test".to_string())
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok(format!("FINAL_ANSWER: {message}"))
    }
}
