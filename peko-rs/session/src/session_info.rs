//! `SessionInfo` and `HistoryEvent` — the user-facing DTOs that
//! power `peko log`, the IPC `read_principal_log` endpoint, and
//! desktop chat surfaces.
//!
//! Phase 7 lifted these out of `src/common/services/session_service.rs`
//! into `peko-session`. The runtime-side `SessionService` is a
//! composition-layer concern (it mixes session persistence + IPC +
//! CLI), so it stays in root. The data types it exposes
//! (`SessionInfo`, `HistoryEvent`, `HistoryQuery`, `HistoryResult`,
//! `BranchResult`, `SessionDetails`, `HistorySummary`) live here
//! because every consumer is reading session data and the
//! `From<SessionEntry> for SessionInfo` impl needs `SessionEntry`
//! (which already lives in `peko-session::index`).
//!
//! Wire shape for `HistoryEvent` is the contract between
//! `peko-runtime` and the desktop
//! (`peko-desktop/src/types/index.ts:88`). The enum tag is `kind`
//! (not `type`) with snake_case variant names; field names are
//! camelCase. Earlier versions used `tag = "type"` with PascalCase
//! variants and snake_case fields — the desktop type declared
//! `kind: "session" | "message" | "tool_call" | ...` with
//! camelCase fields, so every event silently failed the kind
//! discriminator on the frontend and was filtered out by
//! `historyEventsToChatItems`. `rename_all_fields`
//! (serde ≥1.0.197) is what lets us declare the canonical Rust
//! names without writing a per-field `rename =` annotation.

use crate::events::SessionEvent;
use crate::index::SessionEntry;
use peko_message::MessageRole;

/// Session information
///
/// **Issue #24 review #4:** `peer_type` and `peer_id` are populated
/// for sessions whose metadata carries the principal-aware
/// attribution (post-#24 a2a-spawned sessions, future post-#24 paths).
/// For sessions created by human-originated call paths (CLI, IPC,
/// tunnel) BEFORE the runtime populates `SessionEntry.peer_type` /
/// `peer_id` for non-a2a paths, both fields are `None` and the JSON
/// output omits them (via `skip_serializing_if`). Pre-existing JSON
/// consumers stay stable. A future follow-up populates these for
/// non-a2a paths (out of scope for this PR).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    #[serde(rename = "session_id")]
    pub id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub turn_count: u32,
    pub message_count: usize,
    /// `total_tokens` reported by the provider on the most recent
    /// assistant turn. NOT the model's context window size — see
    /// `model_context_limit` for that.
    pub last_total_tokens: usize,
    /// Cumulative input tokens across all assistant messages
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    pub total_output_tokens: usize,
    /// The model's maximum context window size, in tokens, if known.
    /// `None` for legacy entries and sessions opened without a
    /// provider/model reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context_limit: Option<usize>,
    pub parent_session_id: Option<String>,
    pub title: Option<String>,
    /// Subject type (`"user"`, `"agent"`, or `"public"`).
    ///
    /// Reflects the `Subject` kind on the session's peer after
    /// ADR-039. For a2a-spawned sessions this is `"agent"` (issue #24).
    /// `None` for sessions whose on-disk metadata hasn't been
    /// populated yet (see struct-level note above).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_type: Option<String>,
    /// Subject id (bare id, e.g. `"helper"`, not the formatted
    /// `"agent:helper"` form).
    ///
    /// `None` for sessions whose on-disk metadata hasn't been
    /// populated yet (see struct-level note above).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
}

impl From<SessionEntry> for SessionInfo {
    fn from(entry: SessionEntry) -> Self {
        Self {
            id: entry.session_id,
            agent_name: entry.agent_name,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            turn_count: entry.turn_count,
            message_count: entry.message_count,
            last_total_tokens: entry.last_total_tokens,
            total_input_tokens: entry.total_input_tokens,
            total_output_tokens: entry.total_output_tokens,
            model_context_limit: entry.model_context_limit,
            parent_session_id: entry.parent_session_id,
            title: entry.title,
            peer_type: entry.peer_type,
            peer_id: entry.peer_id,
        }
    }
}

/// History event types
///
/// Wire shape is the contract between `peko-runtime` and the desktop
/// (`peko-desktop/src/types/index.ts:88`). The enum tag is `kind`
/// (not `type`) with snake_case variant names; field names are
/// camelCase. Earlier versions used `tag = "type"` with PascalCase
/// variants and snake_case fields — the desktop type declared
/// `kind: "session" | "message" | "tool_call" | ...` with camelCase
/// fields, so every event silently failed the kind discriminator on
/// the frontend and was filtered out by `historyEventsToChatItems`.
/// `rename_all_fields` (serde ≥1.0.197) is what lets us declare the
/// canonical Rust names without writing a per-field `rename =`
/// annotation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HistoryEvent {
    /// Session-start marker. Carries the session id (e.g.
    /// `"root:user:local"`) and the wall-clock time the session was
    /// created so the desktop Activity route can render a header row
    /// without joining against the response envelope.
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "startedAt")]
        started_at: String,
    },
    Message {
        role: String,
        content: String,
        timestamp: String,
    },
    ToolCall {
        tool_name: String,
        args: serde_json::Value,
        tool_call_id: String,
        timestamp: String,
    },
    ToolResult {
        tool_call_id: String,
        output: Option<String>,
        error: Option<String>,
        timestamp: String,
    },
    Thinking {
        content: String,
        timestamp: String,
    },
    ModelChange {
        provider: String,
        model_id: String,
        timestamp: String,
    },
    Compaction {
        summary: String,
        timestamp: String,
    },
    Custom {
        custom_type: String,
        timestamp: String,
    },
}

/// History query parameters
#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    pub include_tool_calls: bool,
    pub include_thinking: bool,
    pub limit: usize,
    pub cursor: Option<String>,
}

impl HistoryQuery {
    #[must_use]
    pub fn default() -> Self {
        Self {
            include_tool_calls: true,
            include_thinking: false,
            limit: 100,
            cursor: None,
        }
    }
}

/// History result
#[derive(Debug, Clone)]
pub struct HistoryResult {
    pub session_id: String,
    pub events: Vec<HistoryEvent>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// Branch result
#[derive(Debug, Clone, serde::Serialize)]
pub struct BranchResult {
    pub new_session_id: String,
    pub parent_session_id: String,
    pub label: Option<String>,
}

/// Session details with full metadata
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionDetails {
    pub info: SessionInfo,
    pub history_summary: HistorySummary,
}

/// History summary for session overview
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HistorySummary {
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub thinking_blocks: usize,
}

/// Convert one `SessionEvent` into a `HistoryEvent` for the user-
/// facing log view.
///
/// Single canonical converter. Both `SessionService::get_history` (via
/// `convert_event`) and `IpcServer::read_principal_log` use this — they
/// previously each carried their own copy of the same match body, which
/// had drifted (the IPC variant constructed owned `String`s, the
/// service variant cloned fields via `&self`). Filtering on
/// `HistoryQuery` (`include_tool_calls`, `include_thinking`) is applied
/// at the call site, not here.
///
/// `session_id` and `session_started_at` are passed through so the
/// `HistoryEvent::Session` marker can carry them — the desktop's
/// `HistoryEvent` union has `{ kind: "session", sessionId, startedAt }`
/// and the desktop renders those without joining the response envelope.
///
/// Returns `None` for events that have no display representation
/// (a2a traffic, spawn-request internals, session-end markers).
pub fn session_event_to_history(
    event: &SessionEvent,
    session_id: &str,
    session_started_at: &str,
) -> Option<HistoryEvent> {
    Some(match event {
        SessionEvent::SessionCreated(e) => HistoryEvent::Session {
            session_id: session_id.to_string(),
            started_at: if session_started_at.is_empty() {
                e.envelope.ts.to_rfc3339()
            } else {
                session_started_at.to_string()
            },
        },
        SessionEvent::MessageV2(msg) => {
            // System prompts and other LLM-only instructions are persisted so
            // the runtime can resume a session, but they are not part of the
            // user-facing conversation. Exposing them here caused desktop chat
            // bubbles to render the root-agent prompt as an assistant message
            // and to merge adjacent non-user chunks (e.g. model-change JSON)
            // into the assistant reply.
            if matches!(msg.role(), MessageRole::System) {
                return None;
            }
            HistoryEvent::Message {
                role: match msg.role() {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::System => "system",
                    MessageRole::Tool => "tool",
                }
                .to_string(),
                content: msg.text_content(),
                timestamp: msg.envelope.ts.to_rfc3339(),
            }
        }
        SessionEvent::ToolCall(e) => HistoryEvent::ToolCall {
            tool_name: e.tool.clone(),
            args: e.args.clone(),
            tool_call_id: e.tool_call_id.clone(),
            timestamp: e.envelope.ts.to_rfc3339(),
        },
        SessionEvent::ToolResult(e) => HistoryEvent::ToolResult {
            tool_call_id: e.tool_call_id.clone(),
            output: e.output.clone(),
            error: e.error.clone(),
            timestamp: e.envelope.ts.to_rfc3339(),
        },
        SessionEvent::Thinking(e) => HistoryEvent::Thinking {
            content: e.content.clone(),
            timestamp: e.envelope.ts.to_rfc3339(),
        },
        SessionEvent::System(e) => match e.event.as_str() {
            "model_change" => {
                let provider = e
                    .detail
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let model_id = e
                    .detail
                    .get("model_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                HistoryEvent::ModelChange {
                    provider,
                    model_id,
                    timestamp: e.envelope.ts.to_rfc3339(),
                }
            }
            "compaction" => {
                let summary = e
                    .detail
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                HistoryEvent::Compaction {
                    summary,
                    timestamp: e.envelope.ts.to_rfc3339(),
                }
            }
            _ => return None,
        },
        SessionEvent::HookTrigger(e) => HistoryEvent::Custom {
            custom_type: format!("hook:{:?}", e.hook_type),
            timestamp: e.envelope.ts.to_rfc3339(),
        },
        SessionEvent::SpawnRequest(_)
        | SessionEvent::SpawnResult(_)
        | SessionEvent::A2aSent(_)
        | SessionEvent::A2aReceived(_)
        | SessionEvent::SessionEnded(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_query_default() {
        let query = HistoryQuery::default();
        assert!(query.include_tool_calls);
        assert!(!query.include_thinking);
        assert_eq!(query.limit, 100);
        assert!(query.cursor.is_none());
    }
}
