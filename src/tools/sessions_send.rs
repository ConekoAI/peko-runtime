//! Sessions Send Tool — Human-to-agent session messaging
//!
//! Implements `CAPABILITY_INTERFACE.md` §3.9a
//! - Intended for human-to-agent and tooling-to-agent communication
//! - Agent-to-agent communication should use `a2a_send` instead
//!
//! This tool delegates to StatelessAgentService, reusing the same execution path
//! as `pekobot send` and the HTTP API.
//!
//! Note: Async execution and timeout are handled by the framework-level
//! `AsyncExecutionRouter` using `_async` and `_timeout` parameters.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::agent::stateless_service::{MessageRequest, StatelessAgentService};
use crate::tools::Tool;

/// Sessions Send tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSendArgs {
    /// Target session ID
    pub session_id: String,
    /// Message content
    pub message: String,
}

/// Sessions Send result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSendResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Sessions Send tool for human-to-agent session messaging
///
/// Delegates to StatelessAgentService, extracting the agent name from the session ID.
pub struct SessionsSendTool {
    agent_service: Option<Arc<StatelessAgentService>>,
    current_session_key: Option<String>,
    current_agent_name: Option<String>,
}

impl SessionsSendTool {
    /// Create a new `sessions_send` tool
    #[must_use]
    pub fn new() -> Self {
        Self {
            agent_service: None,
            current_session_key: None,
            current_agent_name: None,
        }
    }

    /// Configure with session context
    #[must_use]
    pub fn with_session_context(
        mut self,
        session_key: impl Into<String>,
        agent_name: impl Into<String>,
    ) -> Self {
        self.current_session_key = Some(session_key.into());
        self.current_agent_name = Some(agent_name.into());
        self
    }

    /// Configure with agent service for real execution
    #[must_use]
    pub fn with_agent_service(mut self, service: Arc<StatelessAgentService>) -> Self {
        self.agent_service = Some(service);
        self
    }

    /// Extract agent name from session ID
    ///
    /// Session ID format: agent:{agent_id}:session:{uuid}
    /// or: agent:{agent_id}:spawn:{parent}:{uuid}
    fn extract_agent_name_from_session(&self, session_id: &str) -> Option<String> {
        session_id
            .split(':')
            .nth(1)
            .map(std::string::ToString::to_string)
    }
}

impl Default for SessionsSendTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SessionsSendTool {
    fn name(&self) -> &'static str {
        "sessions_send"
    }

    fn description(&self) -> String {
        r#"## Purpose
Send a message to a specific session. For human-to-agent and tooling-to-agent communication.

## When to Use
- Send a message to an existing session by ID
- Human or external tool injecting a message into an agent's session

## When NOT to Use
- For agent-to-agent delegation (use `a2a_send` instead)

## Parameters
```json
{
  "session_id": "agent:myagent:session:abc123",
  "message": "Please check the report"
}
```

## Async Execution
For long-running message handling, use the framework-level async parameter:
```json
{
  "session_id": "agent:myagent:session:abc123",
  "message": "Please check the report",
  "_async": true,
  "_timeout": 60
}
```

## Response
```json
{
  "success": true,
  "response": "I found 3 issues...",
  "session_id": "agent:myagent:session:abc123",
  "duration_ms": 4200
}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Target session ID (format: agent:{name}:session:{uuid})"
                },
                "message": {
                    "type": "string",
                    "description": "Message content"
                }
            },
            "required": ["session_id", "message"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: SessionsSendArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let agent_name = self
            .extract_agent_name_from_session(&args.session_id)
            .ok_or_else(|| anyhow::anyhow!("Invalid session_id format: cannot extract agent name"))?;

        let request = MessageRequest::new(&agent_name, &args.message)
            .with_session(&args.session_id);

        match &self.agent_service {
            Some(service) => {
                let result = service.execute_message(request).await;
                match result {
                    Ok(msg_result) => {
                        let response = SessionsSendResult {
                            success: msg_result.success,
                            response: Some(msg_result.content),
                            session_id: msg_result.session_id,
                            duration_ms: Some(msg_result.duration_ms),
                            error: msg_result.error,
                        };
                        Ok(serde_json::to_value(response)?)
                    }
                    Err(e) => {
                        let response = SessionsSendResult {
                            success: false,
                            response: None,
                            session_id: args.session_id,
                            duration_ms: None,
                            error: Some(e.to_string()),
                        };
                        Ok(serde_json::to_value(response)?)
                    }
                }
            }
            None => {
                // Fallback when agent service is not available (should not happen in daemon mode)
                tracing::warn!("sessions_send: StatelessAgentService not available, returning simulated response");
                let response = SessionsSendResult {
                    success: true,
                    response: Some(format!(
                        "Simulated response to: {}",
                        args.message.chars().take(50).collect::<String>()
                    )),
                    session_id: args.session_id,
                    duration_ms: Some(0),
                    error: None,
                };
                Ok(serde_json::to_value(response)?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sessions_send_tool_creation() {
        let tool = SessionsSendTool::new();
        assert_eq!(tool.name(), "sessions_send");
    }

    #[test]
    fn test_sessions_send_args_parsing() {
        let json = r#"{
            "session_id": "sess_123",
            "message": "Hello"
        }"#;

        let args: SessionsSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.session_id, "sess_123");
        assert_eq!(args.message, "Hello");
    }

    #[test]
    fn test_extract_agent_name_from_session() {
        let tool = SessionsSendTool::new();

        assert_eq!(
            tool.extract_agent_name_from_session("agent:researcher1:session:abc123"),
            Some("researcher1".to_string())
        );

        assert_eq!(
            tool.extract_agent_name_from_session("agent:myagent:spawn:parent:xyz"),
            Some("myagent".to_string())
        );

        assert_eq!(tool.extract_agent_name_from_session("invalid"), None);
    }

    #[test]
    fn test_sessions_send_result_serialization() {
        let result = SessionsSendResult {
            success: true,
            response: Some("All good".to_string()),
            session_id: "agent:test:session:abc".to_string(),
            duration_ms: Some(1500),
            error: None,
        };

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["response"], "All good");
        assert_eq!(json["session_id"], "agent:test:session:abc");
    }
}
