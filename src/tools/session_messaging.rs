//! Simple Session-based Messaging Tool
//! 
//! Provides lightweight agent-to-agent communication without the full
//! A2A protocol ceremony. For simple delegation and messaging.

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::{mpsc, Mutex};

use crate::tools::Tool;
use crate::types::agent::AgentInfo;

/// Simple session message
#[derive(Debug, Clone)]
pub struct SessionMessage {
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: u64,
}

/// Session registry for lightweight messaging
#[derive(Default)]
pub struct SessionRegistry {
    sessions: Mutex<HashMap<String, Vec<SessionMessage>>>,
    subscribers: Mutex<HashMap<String, mpsc::Sender<SessionMessage>>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent's session
    pub async fn register_session(&self, agent_did: String) {
        let mut sessions = self.sessions.lock().await;
        sessions.entry(agent_did).or_insert_with(Vec::new);
    }

    /// Send a message to an agent's session
    pub async fn send_message(&self, message: SessionMessage) -> anyhow::Result<()> {
        // Store in session history
        let mut sessions = self.sessions.lock().await;
        let inbox = sessions.entry(message.to.clone()).or_insert_with(Vec::new);
        inbox.push(message.clone());

        // Notify subscriber if online
        drop(sessions); // Release lock before awaiting
        
        let subscribers = self.subscribers.lock().await;
        if let Some(sender) = subscribers.get(&message.to) {
            let _ = sender.send(message).await;
        }

        Ok(())
    }

    /// Get messages for an agent
    pub async fn get_messages(&self, agent_did: &str) -> Vec<SessionMessage> {
        let sessions = self.sessions.lock().await;
        sessions.get(agent_did).cloned().unwrap_or_default()
    }

    /// List all active sessions
    pub async fn list_sessions(&self) -> Vec<String> {
        let sessions = self.sessions.lock().await;
        sessions.keys().cloned().collect()
    }

    /// Subscribe to real-time messages
    pub async fn subscribe(&self, agent_did: String) -> mpsc::Receiver<SessionMessage> {
        let (tx, rx) = mpsc::channel(100);
        let mut subscribers = self.subscribers.lock().await;
        subscribers.insert(agent_did, tx);
        rx
    }
}

/// Session tool for simple agent-to-agent messaging
pub struct SessionMessagingTool {
    registry: std::sync::Arc<SessionRegistry>,
    agent_did: String,
}

impl SessionMessagingTool {
    pub fn new(registry: std::sync::Arc<SessionRegistry>, agent_did: String) -> Self {
        Self { registry, agent_did }
    }
}

#[async_trait]
impl Tool for SessionMessagingTool {
    fn name(&self) -> &str {
        "session_messaging"
    }

    fn description(&self) -> &str {
        r#"Simple session-based messaging for agent-to-agent communication.

Use this for:
- Quick task delegation
- Simple request/response
- Status updates
- One-off commands

Commands:
- list: List all active agent sessions
- send: Send a message to another agent
- read: Read messages from your inbox

For complex negotiations with contracts and quotes, use the full A2A protocol instead.

Examples:
TOOL_CALL: {"name": "session_messaging", "parameters": {"command": "list"}}
TOOL_CALL: {"name": "session_messaging", "parameters": {"command": "send", "to": "did:pekobot:local:agent2", "message": "Please analyze this data"}}
TOOL_CALL: {"name": "session_messaging", "parameters": {"command": "read", "limit": 10}}"#
    }

    async fn execute(
        &self,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let command = params
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("list");

        match command {
            "list" => {
                let sessions = self.registry.list_sessions().await;
                Ok(json!({
                    "success": true,
                    "sessions": sessions,
                    "count": sessions.len()
                }))
            }

            "send" => {
                let to = params
                    .get("to")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'to' parameter"))?;

                let message = params
                    .get("message")
                    .and_then(|m| m.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?;

                let session_msg = SessionMessage {
                    from: self.agent_did.clone(),
                    to: to.to_string(),
                    content: message.to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs(),
                };

                self.registry.send_message(session_msg).await?;

                Ok(json!({
                    "success": true,
                    "message": format!("Message sent to {}", to),
                    "timestamp": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs()
                }))
            }

            "read" => {
                let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(10);
                let messages = self.registry.get_messages(&self.agent_did).await;
                
                // Get last N messages
                let recent: Vec<_> = messages.iter().rev().take(limit as usize).cloned().collect();

                Ok(json!({
                    "success": true,
                    "messages": recent.iter().map(|m| json!({
                        "from": m.from,
                        "content": m.content,
                        "timestamp": m.timestamp
                    })).collect::<Vec<_>>(),
                    "total": messages.len(),
                    "returned": recent.len()
                }))
            }

            _ => Err(anyhow::anyhow!("Unknown command: {}. Use 'list', 'send', or 'read'", command)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_registry() {
        let registry = std::sync::Arc::new(SessionRegistry::new());
        
        // Register two agents
        registry.register_session("did:pekobot:local:agent1".to_string()).await;
        registry.register_session("did:pekobot:local:agent2".to_string()).await;
        
        // Send message
        let msg = SessionMessage {
            from: "did:pekobot:local:agent1".to_string(),
            to: "did:pekobot:local:agent2".to_string(),
            content: "Hello!".to_string(),
            timestamp: 1234567890,
        };
        
        registry.send_message(msg.clone()).await.unwrap();
        
        // Read messages
        let messages = registry.get_messages("did:pekobot:local:agent2").await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Hello!");
        
        // List sessions
        let sessions = registry.list_sessions().await;
        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_session_tool() {
        let registry = std::sync::Arc::new(SessionRegistry::new());
        registry.register_session("did:pekobot:local:agent1".to_string()).await;
        registry.register_session("did:pekobot:local:agent2".to_string()).await;
        
        let tool = SessionMessagingTool::new(registry.clone(), "did:pekobot:local:agent1".to_string());
        
        // Test list
        let result = tool.execute(json!({"command": "list"})).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        
        // Test send
        let result = tool.execute(json!({
            "command": "send",
            "to": "did:pekobot:local:agent2",
            "message": "Test message"
        })).await.unwrap();
        
        assert!(result["success"].as_bool().unwrap());
    }
}
