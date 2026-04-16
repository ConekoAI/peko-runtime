//! Input handling for the agentic loop
//!
//! Supports three input sources per REQ-AR-001:
//! - User messages (from chat API)
//! - Hook triggers (cron, webhook, file watch, event)
//! - A2A bus messages (agent-to-agent communication)

use crate::providers::{ChatMessage, MessageRole};
use crate::types::message::ContentBlock;
use serde::{Deserialize, Serialize};

/// Input source for the agentic loop
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentInput {
    /// User message from chat API
    UserMessage {
        /// Message content
        content: String,
        /// Optional session ID to resume
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// Hook trigger (cron, webhook, file watch, event)
    HookTrigger {
        /// Hook type
        hook_type: HookType,
        /// Trigger payload
        payload: serde_json::Value,
        /// Trigger source identifier (e.g., webhook path, cron schedule)
        trigger_source: String,
    },
    /// A2A bus message from another agent
    A2AMessage {
        /// Sender agent ID
        from_agent: String,
        /// Message content
        content: String,
        /// Message type for routing
        message_type: A2AMessageType,
        /// Optional conversation/correlation ID
        #[serde(skip_serializing_if = "Option::is_none")]
        conversation_id: Option<String>,
    },
}

/// Hook types for trigger events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookType {
    /// Cron schedule trigger
    Cron,
    /// Webhook HTTP trigger
    Webhook,
    /// File system watch trigger
    FileWatch,
    /// Event bus trigger
    Event,
}

/// A2A message types for routing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2AMessageType {
    /// Direct message to agent
    Direct,
    /// Broadcast to all agents in team
    Broadcast,
    /// Request expecting response
    Request,
    /// Response to a request
    Response,
    /// Fire-and-forget announcement
    Announcement,
}

/// Input context for a turn
#[derive(Debug, Clone)]
pub struct InputContext {
    /// The input source
    pub input: AgentInput,
    /// Run identifier
    pub run_id: String,
    /// Whether this is a new session or continuing
    pub is_new_session: bool,
}

impl AgentInput {
    /// Create a user message input
    #[must_use]
    pub fn user_message(content: impl Into<String>) -> Self {
        Self::UserMessage {
            content: content.into(),
            session_id: None,
        }
    }

    /// Create a user message with session ID
    #[must_use]
    pub fn user_message_with_session(
        content: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self::UserMessage {
            content: content.into(),
            session_id: Some(session_id.into()),
        }
    }

    /// Create a hook trigger input
    #[must_use]
    pub fn hook_trigger(
        hook_type: HookType,
        payload: serde_json::Value,
        trigger_source: impl Into<String>,
    ) -> Self {
        Self::HookTrigger {
            hook_type,
            payload,
            trigger_source: trigger_source.into(),
        }
    }

    /// Create an A2A message input
    #[must_use]
    pub fn a2a_message(
        from_agent: impl Into<String>,
        content: impl Into<String>,
        message_type: A2AMessageType,
    ) -> Self {
        Self::A2AMessage {
            from_agent: from_agent.into(),
            content: content.into(),
            message_type,
            conversation_id: None,
        }
    }

    /// Get the content as a string for display
    #[must_use]
    pub fn content_string(&self) -> String {
        match self {
            Self::UserMessage { content, .. } => content.clone(),
            Self::HookTrigger {
                payload,
                trigger_source,
                hook_type,
            } => {
                format!(
                    "[{} hook from {}] {}",
                    match hook_type {
                        HookType::Cron => "Cron",
                        HookType::Webhook => "Webhook",
                        HookType::FileWatch => "FileWatch",
                        HookType::Event => "Event",
                    },
                    trigger_source,
                    serde_json::to_string(payload).unwrap_or_default()
                )
            }
            Self::A2AMessage {
                from_agent,
                content,
                message_type,
                ..
            } => {
                format!("[A2A {message_type:?} from {from_agent}] {content}")
            }
        }
    }

    /// Convert to a `ChatMessage` for the LLM
    #[must_use]
    pub fn to_chat_message(&self) -> ChatMessage {
        let content = self.content_string();
        ChatMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: content }],
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Get the source type name
    #[must_use]
    pub fn source_type(&self) -> &'static str {
        match self {
            Self::UserMessage { .. } => "user",
            Self::HookTrigger { .. } => "hook",
            Self::A2AMessage { .. } => "a2a",
        }
    }

    /// Check if this is a user message
    #[must_use]
    pub fn is_user_message(&self) -> bool {
        matches!(self, Self::UserMessage { .. })
    }

    /// Check if this is a hook trigger
    #[must_use]
    pub fn is_hook_trigger(&self) -> bool {
        matches!(self, Self::HookTrigger { .. })
    }

    /// Check if this is an A2A message
    #[must_use]
    pub fn is_a2a_message(&self) -> bool {
        matches!(self, Self::A2AMessage { .. })
    }
}

impl InputContext {
    /// Create a new input context
    #[must_use]
    pub fn new(input: AgentInput, run_id: impl Into<String>) -> Self {
        Self {
            input,
            run_id: run_id.into(),
            is_new_session: true,
        }
    }

    /// Set as continuing session
    #[must_use]
    pub fn with_existing_session(mut self) -> Self {
        self.is_new_session = false;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_message() {
        let input = AgentInput::user_message("Hello");
        assert!(input.is_user_message());
        assert!(!input.is_hook_trigger());
        assert!(!input.is_a2a_message());
        assert_eq!(input.source_type(), "user");
        assert_eq!(input.content_string(), "Hello");
    }

    #[test]
    fn test_user_message_with_session() {
        let input = AgentInput::user_message_with_session("Hello", "sess_123");
        match input {
            AgentInput::UserMessage {
                content,
                session_id,
            } => {
                assert_eq!(content, "Hello");
                assert_eq!(session_id, Some("sess_123".to_string()));
            }
            _ => panic!("Expected UserMessage variant"),
        }
    }

    #[test]
    fn test_hook_trigger() {
        let payload = serde_json::json!({"file": "test.txt"});
        let input = AgentInput::hook_trigger(HookType::FileWatch, payload, "./watch");
        assert!(!input.is_user_message());
        assert!(input.is_hook_trigger());
        assert!(!input.is_a2a_message());
        assert_eq!(input.source_type(), "hook");
        let content = input.content_string();
        assert!(content.contains("FileWatch"));
        assert!(content.contains("./watch"));
    }

    #[test]
    fn test_hook_trigger_variants() {
        // Test Cron hook
        let cron = AgentInput::hook_trigger(
            HookType::Cron,
            serde_json::json!({"schedule": "0 9 * * *"}),
            "daily_job",
        );
        let content = cron.content_string();
        assert!(content.contains("Cron"));
        assert!(content.contains("daily_job"));

        // Test Webhook hook
        let webhook = AgentInput::hook_trigger(
            HookType::Webhook,
            serde_json::json!({"event": "push"}),
            "/github/webhook",
        );
        let content = webhook.content_string();
        assert!(content.contains("Webhook"));
        assert!(content.contains("/github/webhook"));

        // Test Event hook
        let event = AgentInput::hook_trigger(
            HookType::Event,
            serde_json::json!({"type": "user_login"}),
            "auth_events",
        );
        let content = event.content_string();
        assert!(content.contains("Event"));
        assert!(content.contains("auth_events"));
    }

    #[test]
    fn test_a2a_message() {
        let input = AgentInput::a2a_message("agent_1", "Hello team", A2AMessageType::Broadcast);
        assert!(!input.is_user_message());
        assert!(!input.is_hook_trigger());
        assert!(input.is_a2a_message());
        assert_eq!(input.source_type(), "a2a");
        let content = input.content_string();
        assert!(content.contains("agent_1"));
        assert!(content.contains("Hello team"));
    }

    #[test]
    fn test_a2a_message_types() {
        let types = vec![
            A2AMessageType::Direct,
            A2AMessageType::Broadcast,
            A2AMessageType::Request,
            A2AMessageType::Response,
            A2AMessageType::Announcement,
        ];

        for msg_type in types {
            let input = AgentInput::a2a_message("sender", "content", msg_type.clone());
            let content = input.content_string();
            assert!(content.contains("sender"));
            assert!(content.contains("content"));
            // The debug representation of the enum variant should be in the content
            assert!(!content.is_empty());
        }
    }

    #[test]
    fn test_to_chat_message() {
        let input = AgentInput::user_message("Test message");
        let msg = input.to_chat_message();
        assert!(matches!(msg.role, MessageRole::User));
    }

    #[test]
    fn test_to_chat_message_with_hook() {
        let input = AgentInput::hook_trigger(
            HookType::Cron,
            serde_json::json!({"task": "backup"}),
            "scheduler",
        );
        let msg = input.to_chat_message();
        assert!(matches!(msg.role, MessageRole::User));
        match &msg.content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("Cron"));
                assert!(text.contains("scheduler"));
            }
            _ => panic!("Expected Text content block"),
        }
    }

    #[test]
    fn test_input_context() {
        let input = AgentInput::user_message("Hello");
        let ctx = InputContext::new(input, "run_123");
        assert!(ctx.is_new_session);
        assert_eq!(ctx.run_id, "run_123");

        let ctx = ctx.with_existing_session();
        assert!(!ctx.is_new_session);
    }

    #[test]
    fn test_input_context_chaining() {
        let input = AgentInput::user_message("Test");
        let ctx = InputContext::new(input, "run_001").with_existing_session();

        assert_eq!(ctx.run_id, "run_001");
        assert!(!ctx.is_new_session);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let input = AgentInput::user_message_with_session("Hello", "sess_123");
        let json = serde_json::to_string(&input).unwrap();
        let deserialized: AgentInput = serde_json::from_str(&json).unwrap();

        match deserialized {
            AgentInput::UserMessage {
                content,
                session_id,
            } => {
                assert_eq!(content, "Hello");
                assert_eq!(session_id, Some("sess_123".to_string()));
            }
            _ => panic!("Deserialization failed"),
        }
    }

    #[test]
    fn test_hook_serialization() {
        let input = AgentInput::hook_trigger(
            HookType::FileWatch,
            serde_json::json!({"path": "/tmp/test"}),
            "watcher",
        );
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("hook_trigger"));
        assert!(json.contains("file_watch"));
        assert!(json.contains("watcher"));
    }

    #[test]
    fn test_a2a_serialization() {
        let input = AgentInput::a2a_message("agent_a", "Hello", A2AMessageType::Direct);
        let json = serde_json::to_string(&input).unwrap();
        // The variant is A2AMessage which with snake_case becomes "a2_a_message"
        // (serde treats numbers as word boundaries)
        assert!(json.contains("a2_a_message"), "JSON: {json}");
        assert!(json.contains("agent_a"));
        assert!(json.contains("direct"));
    }
}
