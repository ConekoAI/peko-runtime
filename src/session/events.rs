//! Session Event Types
//!
//! Defines all 13 event types per DATA_MODEL.md §5.3:
//! - session.created, user.message, assistant.message, thinking
//! - tool.call, tool.result, spawn.request, spawn.result
//! - a2a.sent, a2a.received, hook.trigger, system, session.ended

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Event envelope - every line in JSONL shares this structure
///
/// Note: The `type` field comes from the SessionEvent enum tag, not from here.
/// This avoids serialization conflicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Unique event ID (UUID)
    pub id: String,
    /// ISO 8601 timestamp
    pub ts: DateTime<Utc>,
    /// Session ID (for backward compatibility, not used in new writes)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,
    /// Sequence number (deprecated, kept for backward compatibility)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub seq: Option<u64>,
}

/// Trigger type for session creation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTrigger {
    /// Interactive session started by a user
    User,
    /// Started by a cron hook
    Cron,
    /// Started by a webhook delivery
    Webhook,
    /// Started by an event bus message
    Event,
    /// Started by a file watch trigger
    FileWatch,
    /// Created from a branch of another session
    Branch,
    /// Created as a subagent by a parent session
    Spawn,
}

/// session.created - First line of every new session file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreatedEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Instance ID that owns this session
    pub instance_id: String,
    /// Image digest at session creation time
    pub image_digest: String,
    /// Parent session ID (for branched sessions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// What triggered this session creation
    pub trigger: SessionTrigger,
}

/// Message source for user.message events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    /// Typed by a human
    User,
    /// Injected by a hook trigger
    Hook,
    /// Sent by another agent via the event bus
    A2a,
    /// Sent by the spawning parent agent
    SpawnParent,
}

/// user.message - A message sent by the user or hook trigger
///
/// DEPRECATED: Use `LlmMessageEvent` instead for new code.
/// This type is kept for backward compatibility when reading old sessions.
#[deprecated(since = "0.9.0", note = "Use LlmMessageEvent instead")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Message ID
    pub message_id: String,
    /// Message content
    pub content: String,
    /// Source of the message
    pub source: MessageSource,
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

/// assistant.message - Final complete text response from LLM
///
/// DEPRECATED: Use `LlmMessageEvent` instead for new code.
/// This type is kept for backward compatibility when reading old sessions.
#[deprecated(since = "0.9.0", note = "Use LlmMessageEvent instead")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessageEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Message ID
    pub message_id: String,
    /// Response content
    pub content: String,
    /// Token usage for the entire turn
    pub usage: TokenUsage,
}

/// thinking - Extended thinking content from reasoning models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Thinking content
    pub content: String,
}

/// tool.call - A tool invocation requested by the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Tool call ID for matching with result
    pub tool_call_id: String,
    /// Tool name
    pub tool: String,
    /// Arguments as JSON object
    pub args: serde_json::Value,
    /// Whether this is an async call
    pub async_: bool,
    /// Timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

/// tool.result - Result returned by a tool invocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Tool call ID this result corresponds to
    pub tool_call_id: String,
    /// Output text (null if error)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Error message (null if success)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Duration in milliseconds
    pub duration_ms: u64,
}

/// spawn.request - This agent spawned a subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequestEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Tool call ID that triggered the spawn
    pub tool_call_id: String,
    /// Child image reference
    pub child_image: String,
    /// Child instance ID created
    pub child_instance_id: String,
    /// Child session ID created
    pub child_session_id: String,
    /// Task description
    pub task: String,
    /// Whether spawn is async
    pub async_: bool,
}

/// spawn.result - The spawned subagent completed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResultEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Tool call ID this result corresponds to
    pub tool_call_id: String,
    /// Child instance ID
    pub child_instance_id: String,
    /// Child session ID
    pub child_session_id: String,
    /// Output from the subagent (null if error)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Error message (null if success)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Duration in milliseconds
    pub duration_ms: u64,
}

/// A2A message type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum A2aMessageType {
    /// Task assignment
    Task,
    /// Task result
    TaskResult,
    /// Status update
    Status,
    /// Control message
    Control,
    /// Custom message
    Custom,
}

/// a2a.sent - Agent sent a message to the team event bus
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aSentEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// A2A message type
    pub message_type: A2aMessageType,
    /// Topic/channel sent to
    pub topic: String,
    /// Target instance ID
    pub to: String,
    /// Message payload
    pub payload: serde_json::Value,
}

/// a2a.received - Agent received a message from the team event bus
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aReceivedEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// A2A message type
    pub message_type: A2aMessageType,
    /// Topic/channel received from
    pub topic: String,
    /// Source instance ID
    pub from: String,
    /// Message payload
    pub payload: serde_json::Value,
}

/// Hook type for hook.trigger events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookType {
    /// Cron-based trigger
    Cron,
    /// Webhook trigger
    Webhook,
    /// Event bus trigger
    Event,
    /// File watch trigger
    FileWatch,
}

/// hook.trigger - Session was activated by a hook
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookTriggerEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Type of hook
    pub hook_type: HookType,
    /// Schedule (for cron) or path (for webhook/file_watch)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Payload data (decoded request body for webhooks, file info for file_watch)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// system - System-level annotation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Event name (e.g., "context_truncated", "session_resumed")
    pub event: String,
    /// Event details
    pub detail: serde_json::Value,
}

/// system.message - A system prompt message
///
/// This is the new unified format for system messages, replacing the legacy
/// format that used SessionEntry::Message with role="system".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessageEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// System prompt content
    pub content: String,
}

/// Unified message format - LLM-native storage
///
/// Stores messages in the same format providers use, eliminating
/// conversion overhead. This is the preferred format for new sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Message role: system, user, assistant, tool
    pub role: String,
    /// Message content blocks (serialized JSON array of ContentBlock)
    pub content: String,
    /// Tool calls (for assistant messages with tool calls)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<String>,
    /// Tool call ID (for tool messages)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    /// Token usage (for assistant messages)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub usage: Option<TokenUsage>,
}

/// LLM-native message event - Full fidelity storage
///
/// This event type stores messages with their complete content blocks
/// (text, tool calls, thinking, images) in native format, enabling:
/// - Accurate session resumption across providers
/// - Complete audit trails
/// - Exact replay for testing
/// - Provider-switching without format loss
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessageEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Message ID (UUID)
    pub message_id: String,
    /// Message role: system, user, assistant, tool
    pub role: String,
    /// Content blocks in native format
    pub content_blocks: Vec<crate::types::message::ContentBlock>,
    /// Tool calls (for assistant messages with tool_use blocks)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<ToolCallBlock>>,
    /// Tool call ID (for tool messages)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    /// Thinking/reasoning content (for reasoning models)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thinking: Option<ThinkingBlock>,
    /// Provider that generated this message
    pub provider: String,
    /// Model used
    pub model: String,
    /// Token usage for this turn
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub usage: Option<TokenUsage>,
}

/// Tool call block for LLM-native storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallBlock {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Thinking block for reasoning models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
}

/// Session end reason
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    /// User explicitly ended the session
    UserClosed,
    /// Instance was stopped while session was active
    InstanceStopped,
    /// Session exceeded session_timeout_seconds
    IdleTimeout,
    /// Hard token limit hit
    MaxTokensReached,
}

/// session.ended - Final line when session is explicitly closed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEndedEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Reason for session ending
    pub reason: SessionEndReason,
    /// Total turn count
    pub turn_count: u32,
    /// Total tokens used in session
    pub total_tokens: u32,
}

/// Unified session event enum for serialization/deserialization
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    #[serde(rename = "session.created")]
    SessionCreated(SessionCreatedEvent),
    #[serde(rename = "user.message")]
    UserMessage(UserMessageEvent),
    #[serde(rename = "assistant.message")]
    AssistantMessage(AssistantMessageEvent),
    /// System message in new unified format (preferred)
    #[serde(rename = "system.message")]
    SystemMessage(SystemMessageEvent),
    /// Unified message format (LLM-native, preferred for new code)
    #[serde(rename = "message")]
    Message(MessageEvent),
    /// LLM-native message with full content block fidelity
    #[serde(rename = "llm.message")]
    LlmMessage(LlmMessageEvent),
    #[serde(rename = "thinking")]
    Thinking(ThinkingEvent),
    #[serde(rename = "tool.call")]
    ToolCall(ToolCallEvent),
    #[serde(rename = "tool.result")]
    ToolResult(ToolResultEvent),
    #[serde(rename = "spawn.request")]
    SpawnRequest(SpawnRequestEvent),
    #[serde(rename = "spawn.result")]
    SpawnResult(SpawnResultEvent),
    #[serde(rename = "a2a.sent")]
    A2aSent(A2aSentEvent),
    #[serde(rename = "a2a.received")]
    A2aReceived(A2aReceivedEvent),
    #[serde(rename = "hook.trigger")]
    HookTrigger(HookTriggerEvent),
    #[serde(rename = "system")]
    System(SystemEvent),
    #[serde(rename = "session.ended")]
    SessionEnded(SessionEndedEvent),
}

impl SessionEvent {
    /// Get the event envelope
    pub fn envelope(&self) -> &EventEnvelope {
        match self {
            SessionEvent::SessionCreated(e) => &e.envelope,
            SessionEvent::UserMessage(e) => &e.envelope,
            SessionEvent::AssistantMessage(e) => &e.envelope,
            SessionEvent::SystemMessage(e) => &e.envelope,
            SessionEvent::Message(e) => &e.envelope,
            SessionEvent::LlmMessage(e) => &e.envelope,
            SessionEvent::Thinking(e) => &e.envelope,
            SessionEvent::ToolCall(e) => &e.envelope,
            SessionEvent::ToolResult(e) => &e.envelope,
            SessionEvent::SpawnRequest(e) => &e.envelope,
            SessionEvent::SpawnResult(e) => &e.envelope,
            SessionEvent::A2aSent(e) => &e.envelope,
            SessionEvent::A2aReceived(e) => &e.envelope,
            SessionEvent::HookTrigger(e) => &e.envelope,
            SessionEvent::System(e) => &e.envelope,
            SessionEvent::SessionEnded(e) => &e.envelope,
        }
    }

    /// Get the event type as string (from the enum variant)
    pub fn event_type(&self) -> &'static str {
        match self {
            SessionEvent::SessionCreated(_) => "session.created",
            SessionEvent::UserMessage(_) => "user.message",
            SessionEvent::AssistantMessage(_) => "assistant.message",
            SessionEvent::SystemMessage(_) => "system.message",
            SessionEvent::Message(_) => "message",
            SessionEvent::LlmMessage(_) => "llm.message",
            SessionEvent::Thinking(_) => "thinking",
            SessionEvent::ToolCall(_) => "tool.call",
            SessionEvent::ToolResult(_) => "tool.result",
            SessionEvent::SpawnRequest(_) => "spawn.request",
            SessionEvent::SpawnResult(_) => "spawn.result",
            SessionEvent::A2aSent(_) => "a2a.sent",
            SessionEvent::A2aReceived(_) => "a2a.received",
            SessionEvent::HookTrigger(_) => "hook.trigger",
            SessionEvent::System(_) => "system",
            SessionEvent::SessionEnded(_) => "session.ended",
        }
    }

    /// Check if this is a session.ended event
    pub fn is_session_ended(&self) -> bool {
        matches!(self, SessionEvent::SessionEnded(_))
    }

    /// Check if this is an assistant.message event (for title generation)
    pub fn is_assistant_message(&self) -> bool {
        matches!(self, SessionEvent::AssistantMessage(_))
    }

    /// Get content from assistant message (for title generation)
    pub fn assistant_content(&self) -> Option<&str> {
        match self {
            SessionEvent::AssistantMessage(e) => Some(&e.content),
            _ => None,
        }
    }
}

/// Generate a new event ID (UUID-based)
pub fn generate_event_id() -> String {
    format!(
        "evt_{}",
        uuid::Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .chars()
            .take(8)
            .collect::<String>()
    )
}

/// Generate a new message ID
pub fn generate_message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', ""))
}

/// Generate a new tool call ID
pub fn generate_tool_call_id() -> String {
    format!(
        "tc_{}",
        uuid::Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .chars()
            .take(8)
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_created_serialization() {
        // Wrap in SessionEvent enum to get the type tag
        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            instance_id: "inst_7k2mxp3q".to_string(),
            image_digest: "sha256:a3b5c7d9".to_string(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"session.created\""));
        assert!(json.contains("inst_7k2mxp3q"));
    }

    #[test]
    fn test_user_message_serialization() {
        // Wrap in SessionEvent enum to get the type tag
        let event = SessionEvent::UserMessage(UserMessageEvent {
            envelope: EventEnvelope {
                id: "evt_002".to_string(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            message_id: "msg_3xpwqr7n".to_string(),
            content: "Hello, world!".to_string(),
            source: MessageSource::User,
        });

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"user.message\""));
        assert!(json.contains("Hello, world!"));
    }

    #[test]
    fn test_session_event_enum() {
        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
            },
            instance_id: "inst_7k2mxp3q".to_string(),
            image_digest: "sha256:a3b5c7d9".to_string(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });

        assert_eq!(event.event_type(), "session.created");
        assert!(!event.is_session_ended());
    }

    #[test]
    fn test_generate_ids() {
        let event_id = generate_event_id();
        assert!(event_id.starts_with("evt_"));

        let msg_id = generate_message_id();
        assert!(msg_id.starts_with("msg_"));

        let tc_id = generate_tool_call_id();
        assert!(tc_id.starts_with("tc_"));
    }

    #[test]
    fn test_all_event_types() {
        // Test that all 13 event types can be serialized and deserialized
        let ts = Utc::now();
        let session_id = "sess_test".to_string();

        // 1. session.created
        let created = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            instance_id: "inst_001".to_string(),
            image_digest: "sha256:abc".to_string(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });
        assert_eq!(created.event_type(), "session.created");

        // 2. user.message
        let user_msg = SessionEvent::UserMessage(UserMessageEvent {
            envelope: EventEnvelope {
                id: "evt_002".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            message_id: "msg_001".to_string(),
            content: "Hello".to_string(),
            source: MessageSource::User,
        });
        assert_eq!(user_msg.event_type(), "user.message");

        // 3. assistant.message
        let assistant_msg = SessionEvent::AssistantMessage(AssistantMessageEvent {
            envelope: EventEnvelope {
                id: "evt_003".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            message_id: "msg_002".to_string(),
            content: "Hi there".to_string(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
        });
        assert_eq!(assistant_msg.event_type(), "assistant.message");
        assert!(assistant_msg.is_assistant_message());
        assert_eq!(assistant_msg.assistant_content(), Some("Hi there"));

        // 4. thinking
        let thinking = SessionEvent::Thinking(ThinkingEvent {
            envelope: EventEnvelope {
                id: "evt_004".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            content: "Thinking...".to_string(),
        });
        assert_eq!(thinking.event_type(), "thinking");

        // 5. tool.call
        let tool_call = SessionEvent::ToolCall(ToolCallEvent {
            envelope: EventEnvelope {
                id: "evt_005".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            tool_call_id: "tc_001".to_string(),
            tool: "web_search".to_string(),
            args: serde_json::json!({"query": "test"}),
            async_: false,
            timeout_seconds: Some(30),
        });
        assert_eq!(tool_call.event_type(), "tool.call");

        // 6. tool.result
        let tool_result = SessionEvent::ToolResult(ToolResultEvent {
            envelope: EventEnvelope {
                id: "evt_006".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            tool_call_id: "tc_001".to_string(),
            output: Some("Results found".to_string()),
            error: None,
            duration_ms: 1500,
        });
        assert_eq!(tool_result.event_type(), "tool.result");

        // 7. spawn.request
        let spawn_req = SessionEvent::SpawnRequest(SpawnRequestEvent {
            envelope: EventEnvelope {
                id: "evt_007".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            tool_call_id: "tc_002".to_string(),
            child_image: "researcher:v1".to_string(),
            child_instance_id: "inst_child".to_string(),
            child_session_id: "sess_child".to_string(),
            task: "Research task".to_string(),
            async_: false,
        });
        assert_eq!(spawn_req.event_type(), "spawn.request");

        // 8. spawn.result
        let spawn_res = SessionEvent::SpawnResult(SpawnResultEvent {
            envelope: EventEnvelope {
                id: "evt_008".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            tool_call_id: "tc_002".to_string(),
            child_instance_id: "inst_child".to_string(),
            child_session_id: "sess_child".to_string(),
            output: Some("Research complete".to_string()),
            error: None,
            duration_ms: 5000,
        });
        assert_eq!(spawn_res.event_type(), "spawn.result");

        // 9. a2a.sent
        let a2a_sent = SessionEvent::A2aSent(A2aSentEvent {
            envelope: EventEnvelope {
                id: "evt_009".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            message_type: A2aMessageType::Task,
            topic: "team.tasks".to_string(),
            to: "inst_other".to_string(),
            payload: serde_json::json!({"task": "do something"}),
        });
        assert_eq!(a2a_sent.event_type(), "a2a.sent");

        // 10. a2a.received
        let a2a_recv = SessionEvent::A2aReceived(A2aReceivedEvent {
            envelope: EventEnvelope {
                id: "evt_010".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            message_type: A2aMessageType::TaskResult,
            topic: "team.results".to_string(),
            from: "inst_other".to_string(),
            payload: serde_json::json!({"result": "done"}),
        });
        assert_eq!(a2a_recv.event_type(), "a2a.received");

        // 11. hook.trigger
        let hook = SessionEvent::HookTrigger(HookTriggerEvent {
            envelope: EventEnvelope {
                id: "evt_011".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            hook_type: HookType::Cron,
            schedule: Some("0 0 * * *".to_string()),
            payload: None,
        });
        assert_eq!(hook.event_type(), "hook.trigger");

        // 12. system
        let system = SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: "evt_012".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            event: "context_truncated".to_string(),
            detail: serde_json::json!({"reason": "too long"}),
        });
        assert_eq!(system.event_type(), "system");

        // 13. session.ended
        let ended = SessionEvent::SessionEnded(SessionEndedEvent {
            envelope: EventEnvelope {
                id: "evt_013".to_string(),
                ts,
                session_id: None,
                seq: None,
            },
            reason: SessionEndReason::UserClosed,
            turn_count: 5,
            total_tokens: 1000,
        });
        assert_eq!(ended.event_type(), "session.ended");
        assert!(ended.is_session_ended());

        // Test round-trip serialization for each event type
        for event in [
            created,
            user_msg,
            assistant_msg,
            thinking,
            tool_call,
            tool_result,
            spawn_req,
            spawn_res,
            a2a_sent,
            a2a_recv,
            hook,
            system,
            ended,
        ] {
            let json = serde_json::to_string(&event).expect("Failed to serialize");
            let deserialized: SessionEvent =
                serde_json::from_str(&json).expect("Failed to deserialize");
            assert_eq!(event.event_type(), deserialized.event_type());
        }
    }

    #[test]
    fn test_session_trigger_variants() {
        use serde_json;

        // Test all trigger types serialize correctly
        let triggers = vec![
            SessionTrigger::User,
            SessionTrigger::Cron,
            SessionTrigger::Webhook,
            SessionTrigger::Event,
            SessionTrigger::FileWatch,
            SessionTrigger::Branch,
            SessionTrigger::Spawn,
        ];

        for trigger in triggers {
            let json = serde_json::to_string(&trigger).expect("Failed to serialize trigger");
            let deserialized: SessionTrigger =
                serde_json::from_str(&json).expect("Failed to deserialize trigger");
            assert!(
                matches!(trigger, _ if std::mem::discriminant(&trigger) == std::mem::discriminant(&deserialized))
            );
        }
    }

    #[test]
    fn test_session_end_reason_variants() {
        use serde_json;

        let reasons = vec![
            SessionEndReason::UserClosed,
            SessionEndReason::InstanceStopped,
            SessionEndReason::IdleTimeout,
            SessionEndReason::MaxTokensReached,
        ];

        for reason in reasons {
            let json = serde_json::to_string(&reason).expect("Failed to serialize reason");
            let deserialized: SessionEndReason =
                serde_json::from_str(&json).expect("Failed to deserialize reason");
            assert!(
                matches!(reason, _ if std::mem::discriminant(&reason) == std::mem::discriminant(&deserialized))
            );
        }
    }
}
