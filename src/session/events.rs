//! Session Event Types
//!
//! Defines all event types per `DATA_MODEL.md` §5.3:
//! - session.created, user.message, assistant.message, thinking
//! - tool.call, tool.result, spawn.request, spawn.result
//! - a2a.sent, a2a.received, hook.trigger, system, session.ended
//!
//! # Migration Note: `MessageV2`
//!
//! The `MessageV2` variant with `SessionMessage` is the unified format
//! for all messages. Legacy formats have been removed in Phase 5.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::common::types::message::TokenUsage;
/// Re-export unified message types
pub use crate::session::message::{MessageSource, RoleMetadata, SessionMessage};

/// Event envelope - every line in JSONL shares this structure
///
/// Note: The `type` field comes from the `SessionEvent` enum tag, not from here.
/// This avoids serialization conflicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Unique event ID (UUID)
    pub id: String,
    /// ISO 8601 timestamp
    pub ts: DateTime<Utc>,
}

impl EventEnvelope {
    /// Create a new event envelope with current timestamp
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: generate_event_id(),
            ts: Utc::now(),
        }
    }
}

impl Default for EventEnvelope {
    fn default() -> Self {
        Self::new()
    }
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

/// a2a.sent - Agent sent an A2A message.
///
/// Pre-#29 this struct modeled a topic-pubsub broadcast
/// (`topic` + `to`). Post-#29 it is the canonical audit trail
/// for both same-runtime (`Local`) and cross-runtime
/// (`RemoteByDid`/`RemoteByHandle`) sends. The legacy
/// `topic`/`to` fields stay populated for the topic-pubsub
/// path; the new `*_did` / `runtime_id_*` fields are populated
/// on the cross-runtime path. Event consumers that only care
/// about the topic-pubsub path can ignore the new fields;
/// consumers that care about cross-runtime a2a can read them
/// directly without parsing the envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aSentEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// A2A message type
    pub message_type: A2aMessageType,
    /// Topic/channel sent to. Same-runtime topic-pubsub path
    /// (pre-#29) populates this; the cross-runtime path leaves
    /// it empty rather than synthesizing a fake topic.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// Target instance ID. Same-runtime topic-pubsub path
    /// populates this; the cross-runtime path leaves it empty
    /// (the `runtime_id_target` + `target_did` fields are the
    /// authoritative identifiers in that case).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    /// Message payload
    pub payload: serde_json::Value,
    /// Issue #29: cross-runtime a2a audit fields. All four are
    /// `Some` on the cross-runtime path and `None` on the
    /// legacy same-runtime topic-pubsub path so a single event
    /// consumer can distinguish the two with a single check.
    /// The combination `(caller_did, runtime_id_caller)` is the
    /// caller's identity; `(target_did, runtime_id_target)` is
    /// the target's. Together they disambiguate "who sent what
    /// to whom" even when the agent names are reused across
    /// runtimes.
    /// Caller agent's stable DID (issue #28 form:
    /// `did:peko:agent:<keyhash>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_did: Option<String>,
    /// Caller runtime's `did:key` (from the `TunnelMessage`'s
    /// `caller_runtime_id` field on the inbound side; from
    /// `ctx.caller_runtime_id` on the outbound side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id_caller: Option<String>,
    /// Target agent's stable DID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_did: Option<String>,
    /// Target runtime's `did:key` (from the pekohub directory
    /// response's `runtimeId` field on the outbound side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id_target: Option<String>,
    /// `request_id` (UUIDv4) of the `AgentToAgentRequest` /
    /// `AgentToAgentResponse` pair, when this event is part of
    /// a cross-runtime round-trip. `None` on the same-runtime
    /// topic-pubsub path (where request correlation is
    /// per-session, not per-message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// a2a.received - Agent received an A2A message.
///
/// Same shape evolution as `A2aSentEvent`: legacy `topic` /
/// `from` fields stay populated for the same-runtime path;
/// the new `*_did` / `runtime_id_*` fields carry the
/// cross-runtime audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aReceivedEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// A2A message type
    pub message_type: A2aMessageType,
    /// Topic/channel received from. Same-runtime topic-pubsub
    /// path populates this; the cross-runtime path leaves it
    /// empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// Source instance ID. Same-runtime topic-pubsub path
    /// populates this; the cross-runtime path leaves it empty
    /// (the `runtime_id_caller` + `caller_did` fields are the
    /// authoritative identifiers in that case).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    /// Message payload
    pub payload: serde_json::Value,
    /// Issue #29: cross-runtime a2a audit fields. See
    /// `A2aSentEvent` for the field semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id_caller: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id_target: Option<String>,
    /// `request_id` of the inbound `AgentToAgentRequest` /
    /// `AgentToAgentResponse`, when this event is part of a
    /// cross-runtime round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
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
    /// Schedule (for cron) or path (for `webhook/file_watch`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Payload data (decoded request body for webhooks, file info for `file_watch`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// system - System-level annotation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Event name (e.g., "`context_truncated`", "`session_resumed`")
    pub event: String,
    /// Event details
    pub detail: serde_json::Value,
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
    /// Session exceeded `session_timeout_seconds`
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

    /// Unified message format - PREFERRED FOR NEW CODE
    #[serde(rename = "message.v2")]
    MessageV2(SessionMessage),

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
    #[must_use]
    pub fn envelope(&self) -> &EventEnvelope {
        match self {
            SessionEvent::SessionCreated(e) => &e.envelope,
            SessionEvent::MessageV2(e) => &e.envelope,
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
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            SessionEvent::SessionCreated(_) => "session.created",
            SessionEvent::MessageV2(_) => "message.v2",
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
    #[must_use]
    pub fn is_session_ended(&self) -> bool {
        matches!(self, SessionEvent::SessionEnded(_))
    }

    /// Check if this is a message event
    #[must_use]
    pub fn is_message(&self) -> bool {
        matches!(self, SessionEvent::MessageV2(_))
    }

    /// Check if this is an assistant message
    #[must_use]
    pub fn is_assistant_message(&self) -> bool {
        match self {
            SessionEvent::MessageV2(m) => {
                m.role() == crate::common::types::message::MessageRole::Assistant
            }
            _ => false,
        }
    }

    /// Get content from assistant message (for title generation)
    #[must_use]
    pub fn assistant_content(&self) -> Option<String> {
        match self {
            SessionEvent::MessageV2(m)
                if m.role() == crate::common::types::message::MessageRole::Assistant =>
            {
                Some(m.text_content())
            }
            _ => None,
        }
    }

    /// Get the `SessionMessage` if this is a message event
    #[must_use]
    pub fn as_message(&self) -> Option<SessionMessage> {
        match self {
            SessionEvent::MessageV2(m) => Some(m.clone()),
            _ => None,
        }
    }

    // ====================================================================================
    // Display/View Methods
    // ====================================================================================

    /// Get display type for CLI/API (replaces `HistoryEvent` type discrimination)
    ///
    /// Returns a stable string identifier for the event type, suitable for
    /// display in CLI tools and API responses.
    #[must_use]
    pub fn display_type(&self) -> &str {
        match self {
            SessionEvent::SessionCreated(_) => "session.created",
            SessionEvent::MessageV2(m) => match m.role() {
                crate::common::types::message::MessageRole::User => "user",
                crate::common::types::message::MessageRole::Assistant => "assistant",
                crate::common::types::message::MessageRole::System => "system",
                crate::common::types::message::MessageRole::Tool => "tool",
            },
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
}

// ====================================================================================
// ID Generation
// ====================================================================================

/// Generate a unique event ID
#[must_use]
pub fn generate_event_id() -> String {
    format!("evt_{}", uuid::Uuid::new_v4().simple())
}

/// Generate a unique message ID
#[must_use]
pub fn generate_message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().simple())
}

/// Generate a unique tool call ID
#[must_use]
pub fn generate_tool_call_id() -> String {
    format!("call_{}", uuid::Uuid::new_v4().simple())
}
