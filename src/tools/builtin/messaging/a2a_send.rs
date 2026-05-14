//! A2A Send Tool — Minimal agent-to-agent messaging
//!
//! Implements ADR-023: delegates to StatelessAgentService, reusing the same
//! execution path as `peko send` and the HTTP API.
//!
//! ## Parameters
//! ```json
//! {
//!   "target_agent": "analyzer",
//!   "message": "Review this code for bugs",
//!   "session_id": "optional-session-to-resume",
//!   "team": "optional-team"
//! }
//! ```
//!
//! ## Response (blocking)
//! ```json
//! {
//!   "success": true,
//!   "response": "I found 3 issues...",
//!   "session_id": "agent:analyzer:session:xyz",
//!   "iterations": 2,
//!   "tool_calls": [...]
//! }
//! ```
//!
//! Async execution (`_async: true`) and timeout (`_timeout: N`) are handled
//! by the framework-level `AsyncExecutionRouter` via reserved parameters.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::agent::stateless_service::{MessageRequest, StatelessAgentService};
use crate::tools::core::Tool;

/// Arguments for the `a2a_send` tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aSendArgs {
    /// Target agent name
    pub target_agent: String,
    /// Message content to send
    pub message: String,
    /// Optional session ID to resume
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional team for the target agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

/// Result of an `a2a_send` execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aSendResult {
    pub success: bool,
    pub response: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iterations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A2A Send tool — send a message to another agent and receive its response
///
/// This tool delegates to StatelessAgentService, reusing the exact same
/// execution path as `peko send` and the HTTP API.
pub struct A2aSendTool {
    agent_service: Arc<StatelessAgentService>,
    /// Optional caller agent name for annotation
    caller_agent: Option<String>,
}

impl A2aSendTool {
    /// Create a new A2A send tool
    #[must_use]
    pub fn new(agent_service: Arc<StatelessAgentService>) -> Self {
        Self {
            agent_service,
            caller_agent: None,
        }
    }

    /// Set the caller agent name for message annotation
    #[must_use]
    pub fn with_caller(mut self, caller: impl Into<String>) -> Self {
        self.caller_agent = Some(caller.into());
        self
    }

    /// Build the message with optional caller annotation
    #[allow(dead_code)]
    fn build_message(&self, message: &str) -> String {
        match &self.caller_agent {
            Some(caller) => format!("[Message from agent: {caller}]\n\n{message}"),
            None => message.to_string(),
        }
    }
}

#[async_trait]
impl Tool for A2aSendTool {
    fn name(&self) -> &'static str {
        "a2a_send"
    }

    fn description(&self) -> String {
        r#"## Purpose
Send a message to another agent and receive its response. This is the primary mechanism for agent-to-agent (A2A) communication.

## When to Use
- Delegate a subtask to another agent
- Request analysis, review, or specialized work from a peer agent
- Resume a conversation with another agent using a known session_id

## When NOT to Use
- For human-to-agent communication (use the CLI/API instead)
- For fire-and-forget notifications (A2A send is request/response)

## Parameters
```json
{
  "target_agent": "analyzer",
  "message": "Review this code for bugs",
  "session_id": "optional-session-to-resume",
  "team": "optional-team"
}
```

## Response
```json
{
  "success": true,
  "response": "I found 3 issues...",
  "session_id": "agent:analyzer:session:xyz",
  "iterations": 2
}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target_agent": {
                    "type": "string",
                    "description": "Name of the target agent to send the message to"
                },
                "message": {
                    "type": "string",
                    "description": "Message content to send to the target agent"
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional session ID to resume an existing conversation"
                },
                "team": {
                    "type": "string",
                    "description": "Optional team name for the target agent"
                }
            },
            "required": ["target_agent", "message"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: A2aSendArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        // Use caller_agent as the user identity so each (caller, target) pair
        // gets its own isolated session namespace. Without this, all A2A calls
        // would share the "default" user session.
        let caller = self.caller_agent.as_deref().unwrap_or("default");
        let request = MessageRequest::new(&args.target_agent, args.message)
            .with_session_opt(args.session_id)
            .with_team_opt(args.team)
            .with_user(caller)
            .with_caller_agent_opt(self.caller_agent.clone());

        let result = self.agent_service.execute_message(request).await;

        match result {
            Ok(msg_result) => {
                let tool_calls: Vec<serde_json::Value> = msg_result
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "name": tc.name,
                            "parameters": tc.parameters,
                            "result": tc.result
                        })
                    })
                    .collect();

                let response = A2aSendResult {
                    success: msg_result.success,
                    response: msg_result.content,
                    session_id: msg_result.session_id,
                    iterations: Some(msg_result.iterations),
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    duration_ms: Some(msg_result.duration_ms),
                    error: msg_result.error,
                };

                Ok(serde_json::to_value(response)?)
            }
            Err(e) => {
                let response = A2aSendResult {
                    success: false,
                    response: String::new(),
                    session_id: String::new(),
                    iterations: None,
                    tool_calls: None,
                    duration_ms: None,
                    error: Some(e.to_string()),
                };
                Ok(serde_json::to_value(response)?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a2a_send_args_parsing() {
        let json = r#"{
            "target_agent": "analyzer",
            "message": "Review this code",
            "session_id": "sess_123"
        }"#;

        let args: A2aSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_agent, "analyzer");
        assert_eq!(args.message, "Review this code");
        assert_eq!(args.session_id, Some("sess_123".to_string()));
        assert_eq!(args.team, None);
    }

    #[test]
    fn test_a2a_send_args_minimal() {
        let json = r#"{
            "target_agent": "helper",
            "message": "Hello"
        }"#;

        let args: A2aSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_agent, "helper");
        assert_eq!(args.message, "Hello");
        assert_eq!(args.session_id, None);
    }

    #[test]
    fn test_caller_annotation_format() {
        // Verify the caller annotation format used by StatelessAgentService::execute_inner.
        // The service prepends this format when caller_agent is set on ExecutionRequest.
        let caller = "researcher";
        let msg = "Do this task";
        let annotated = format!("[Message from agent: {caller}]\n\n{msg}");
        assert!(annotated.contains("researcher"));
        assert!(annotated.ends_with(msg));
        assert!(annotated.starts_with("[Message from agent:"));
    }

    #[test]
    fn test_a2a_send_result_serialization() {
        let result = A2aSendResult {
            success: true,
            response: "All good".to_string(),
            session_id: "agent:test:session:abc".to_string(),
            iterations: Some(1),
            tool_calls: None,
            duration_ms: Some(1500),
            error: None,
        };

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["response"], "All good");
        assert_eq!(json["session_id"], "agent:test:session:abc");
        assert!(json.get("tool_calls").is_none());
    }
}
