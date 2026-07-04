//! Session introspection tools
//!
//! Provides `session` — a single unified tool for introspecting sessions.
//! Replaces `session_status`, `sessions_list`, `sessions_history` (Issue 013).
//!
//! ## Architecture
//!
//! ```text
//! SessionTool (LLM interface)
//!        │
//!        ▼
//! SessionRegistry (trait)
//!        │
//!        ├─ SessionIntrospector ──► SessionManager (real data)
//!        └─ SessionCache ─────────► In-memory (tests / placeholder)
//! ```

use crate::common::registry::SimpleRegistry;
use crate::common::types::message::{ContentBlock, LlmMessage};
use crate::session::jsonl::SessionStorage;
use crate::session::message_conversion::event_to_llm_message;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::tools::core::traits::Tool;

// ====================================================================================
// Data Types (shared across all actions)
// ====================================================================================

/// Session info
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub session_key: String,
    pub session_id: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub label: Option<String>,
    pub created_at: String,
    pub last_activity: String,
    pub message_count: usize,
    pub is_active: bool,
    /// Subject type ("user", "principal", or "public") — present when
    /// the underlying `SessionMetadata` was written with peer info.
    /// Branched sessions may have `None` here (see `branch_session_by_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_type: Option<String>,
    /// Subject ID (e.g. `"alice"` for `user:alice`). `None` when no
    /// peer is recorded for the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
}

/// Message in session history
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<Vec<ToolResultInfo>>,
    pub timestamp: String,
}

/// Tool call info
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool result info
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResultInfo {
    pub tool_call_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Usage stats
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub context_window: usize,
}

/// Session status result
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStatusResult {
    pub session_id: String,
    pub agent_name: String,
    pub created_at: String,
    pub last_activity: String,
    /// Current timestamp in ISO 8601 format (UTC)
    pub timestamp_utc: String,
    /// Current timestamp formatted for display (respects timezone parameter)
    pub timestamp: String,
    pub message_count: usize,
    pub usage: UsageStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

// ====================================================================================
// SessionRegistry Trait
// ====================================================================================

/// Registry for accessing session data
#[async_trait]
pub trait SessionRegistry: Send + Sync {
    /// List available sessions, optionally filtered.
    ///
    /// - `kinds`: filter by `SessionMetadata::trigger` (e.g. `["main", "branch"]`).
    /// - `peer`: filter to a single peer (`user:alice`, `principal:<did>`, or `public`).
    ///   When `None`, results span all peers (the cross-peer view).
    /// - `agent_id`: filter to a single agent name.
    /// - `limit`: cap on results returned.
    /// - `active_minutes`: only sessions updated within the last N minutes.
    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        peer: Option<&crate::subject::Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>>;

    /// Get session history
    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>>;

    /// Get session status
    async fn get_status(&self, session_key: &str) -> anyhow::Result<SessionStatusResult>;

    /// Get current session key
    fn current_session_key(&self) -> String;
}

// ====================================================================================
// SessionAction — serde-driven, extensible
// ====================================================================================

/// Actions supported by the `session` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionAction {
    Status,
    List,
    History,
}

// ====================================================================================
// SessionTool — unified interface
// ====================================================================================

/// Unified session introspection tool.
pub struct SessionTool {
    registry: Box<dyn SessionRegistry>,
}

impl SessionTool {
    #[must_use]
    pub fn new(registry: Box<dyn SessionRegistry>) -> Self {
        Self { registry }
    }

    // ------------------------------------------------------------------
    // Internal helpers — DRY across all actions
    // ------------------------------------------------------------------

    async fn get_status(&self, session_key: Option<&str>) -> anyhow::Result<SessionStatusResult> {
        let session_id = session_key
            .map(String::from)
            .unwrap_or_else(|| self.registry.current_session_key());
        self.registry.get_status(&session_id).await
    }

    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        peer: Option<&crate::subject::Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        self.registry
            .list_sessions(kinds, peer, agent_id, limit, active_minutes)
            .await
    }

    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        self.registry
            .get_history(session_key, limit, include_tools)
            .await
    }

    // ------------------------------------------------------------------
    // Response builders — pure functions, keep execute() readable
    // ------------------------------------------------------------------

    fn build_status_response(status: &SessionStatusResult) -> serde_json::Value {
        serde_json::to_value(status).unwrap_or_else(|_| json!({"error": "serialization failed"}))
    }

    fn build_list_response(sessions: Vec<SessionInfo>) -> serde_json::Value {
        json!({
            "total": sessions.len(),
            "sessions": sessions,
        })
    }

    fn build_history_response(
        session_key: &str,
        messages: Vec<HistoryMessage>,
    ) -> serde_json::Value {
        json!({
            "session_key": session_key,
            "total_messages": messages.len(),
            "messages": messages,
        })
    }
}

#[async_trait]
impl Tool for SessionTool {
    fn name(&self) -> &'static str {
        "session"
    }

    fn description(&self) -> String {
        r"Manage and introspect sessions: check status, list sessions, or view conversation history.

Parameters:
- action: 'status', 'list', or 'history' (required)
- session_key: Required for 'history'. Optional for 'status' (defaults to current session)
- kinds: Optional for 'list' — filter by session kinds (e.g., ['main', 'spawned'])
- peer: Optional for 'list' — filter to a single peer (e.g., 'user:alice', 'principal:<did>', or 'public'). Without it, results span all peers on this principal.
- agent_id: Optional for 'list' — filter to a single agent name
- limit: Optional — max results (default: 50 for list, 100 for history)
- active_minutes: Optional for 'list' — only sessions active in last N minutes
- include_tools: Optional for 'history' — include tool calls/results (default: true)
- timezone: Optional for 'status' — timezone for timestamp formatting (e.g., 'America/New_York', 'UTC')

Returns structured data appropriate to the action."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "history"],
                    "description": "What to do: status (get one session), list (query sessions), history (get messages)"
                },
                "session_key": {
                    "type": "string",
                    "description": "Required for 'history'. Optional for 'status' (defaults to current session)"
                },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional filter for 'list': e.g., ['main', 'spawned', 'cron']"
                },
                "peer": {
                    "type": "string",
                    "description": "Optional filter for 'list': cross-peer lookup, e.g. 'user:alice' or 'public'. When omitted, results span all peers."
                },
                "agent_id": {
                    "type": "string",
                    "description": "Optional filter for 'list': single agent name"
                },
                "limit": {
                    "type": "integer",
                    "default": 50,
                    "description": "Max results for 'list' or 'history'"
                },
                "active_minutes": {
                    "type": "integer",
                    "description": "Optional for 'list': only sessions active in last N minutes"
                },
                "include_tools": {
                    "type": "boolean",
                    "default": true,
                    "description": "Optional for 'history': include tool calls and results"
                },
                "timezone": {
                    "type": "string",
                    "description": "Optional for 'status': timezone for timestamp formatting (e.g., 'America/New_York', 'UTC')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let action: SessionAction = serde_json::from_value(
            params
                .get("action")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?,
        )
        .map_err(|e| anyhow::anyhow!("Invalid action: {e}"))?;

        match action {
            SessionAction::Status => {
                let session_key = params.get("session_key").and_then(|v| v.as_str());
                let timezone = params.get("timezone").and_then(|v| v.as_str());

                // Try to get existing status, or create minimal one for time queries
                let mut status = match self.get_status(session_key).await {
                    Ok(s) => s,
                    Err(_) => {
                        let session_id = session_key
                            .unwrap_or(&self.registry.current_session_key())
                            .to_string();
                        SessionStatusResult {
                            session_id,
                            agent_name: "unknown".to_string(),
                            created_at: chrono::Utc::now().to_rfc3339(),
                            last_activity: chrono::Utc::now().to_rfc3339(),
                            timestamp_utc: String::new(),
                            timestamp: String::new(),
                            message_count: 0,
                            usage: UsageStats {
                                prompt_tokens: 0,
                                completion_tokens: 0,
                                context_window: 0,
                            },
                            peer_type: None,
                            peer_id: None,
                            label: None,
                            parent_session: None,
                        }
                    }
                };

                // Add current timestamps
                let now_utc = chrono::Utc::now();
                status.timestamp_utc = now_utc.to_rfc3339();
                status.timestamp = if let Some(tz_str) = timezone {
                    match tz_str.parse::<chrono_tz::Tz>() {
                        Ok(tz) => now_utc
                            .with_timezone(&tz)
                            .format("%Y-%m-%d %H:%M:%S %Z")
                            .to_string(),
                        Err(_) => chrono::Local::now()
                            .format("%Y-%m-%d %H:%M:%S %Z")
                            .to_string(),
                    }
                } else {
                    chrono::Local::now()
                        .format("%Y-%m-%d %H:%M:%S %Z")
                        .to_string()
                };

                Ok(Self::build_status_response(&status))
            }
            SessionAction::List => {
                let kinds: Option<Vec<String>> = params
                    .get("kinds")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let peer_str = params.get("peer").and_then(|v| v.as_str());
                let peer = match peer_str {
                    Some(s) => Some(
                        s.parse::<crate::subject::Subject>()
                            .map_err(|e| anyhow::anyhow!("Invalid peer '{s}': {e}"))?,
                    ),
                    None => None,
                };
                let agent_id = params.get("agent_id").and_then(|v| v.as_str());
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let active_minutes = params.get("active_minutes").and_then(|v| v.as_i64());

                let kinds_ref = kinds.as_deref();
                let sessions = self
                    .list_sessions(kinds_ref, peer.as_ref(), agent_id, limit, active_minutes)
                    .await?;
                Ok(Self::build_list_response(sessions))
            }
            SessionAction::History => {
                let session_key = params
                    .get("session_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.registry.current_session_key())
                    .to_string();
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
                let include_tools = params
                    .get("include_tools")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let messages = self.get_history(&session_key, limit, include_tools).await?;
                Ok(Self::build_history_response(&session_key, messages))
            }
        }
    }
}

// ====================================================================================
// SessionIntrospector — backed by real SessionManager
// ====================================================================================

/// Session introspector backed by the real `SessionManager`.
///
/// Wraps `SessionManager` to provide the [`SessionRegistry`] trait for
/// session introspection tools (list, status, history).
pub struct SessionIntrospector {
    session_manager: std::sync::Arc<tokio::sync::RwLock<crate::session::SessionManager>>,
    current_session_id: std::sync::Arc<tokio::sync::RwLock<Option<String>>>,
}

impl SessionIntrospector {
    #[must_use]
    pub fn new(
        session_manager: std::sync::Arc<tokio::sync::RwLock<crate::session::SessionManager>>,
        current_session_id: std::sync::Arc<tokio::sync::RwLock<Option<String>>>,
    ) -> Self {
        Self {
            session_manager,
            current_session_id,
        }
    }
}

#[async_trait]
impl SessionRegistry for SessionIntrospector {
    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        peer: Option<&crate::subject::Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let mut manager = self.session_manager.write().await;
        let metadatas = manager.list_all_sessions(false).await?;

        let now = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = active_minutes.map(|m| now.saturating_sub(m as u64 * 60 * 1000));

        // Build the peer filter's expected (kind, id) pair so we can
        // match against the persisted `peer_type`/`peer_id` strings on
        // `SessionMetadata`. We accept sessions whose metadata peer info
        // is missing — `branch_session_by_id` does not currently copy
        // peer fields onto the new branch — by treating a `None`
        // metadata peer as wildcard.
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));

        let sessions: Vec<SessionInfo> = metadatas
            .into_iter()
            .filter(|m| {
                let kind_match = kinds.map_or(true, |k| k.contains(&m.trigger));
                let agent_match = agent_id.map_or(true, |a| m.agent_name == a);
                let active_match = cutoff_ms.map_or(true, |cutoff| m.updated_at as u64 >= cutoff);
                let peer_match = peer_filter.as_ref().map_or(true, |(want_kind, want_id)| {
                    // No peer recorded on the metadata — skip when the
                    // caller asked for a specific peer.
                    let (have_kind, have_id) = match (m.peer_type.as_deref(), m.peer_id.as_deref())
                    {
                        (Some(k), Some(i)) => (k, i),
                        _ => return false,
                    };
                    have_kind == want_kind.as_str() && have_id == want_id.as_str()
                });
                kind_match && peer_match && agent_match && active_match
            })
            .take(limit)
            .map(|m| SessionInfo {
                session_key: m.session_id.clone(),
                session_id: m.session_id,
                kind: m.trigger,
                agent_id: Some(m.agent_name),
                label: m.title,
                created_at: chrono::DateTime::from_timestamp_millis(m.created_at as i64)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                last_activity: chrono::DateTime::from_timestamp_millis(m.updated_at as i64)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                message_count: m.message_count,
                is_active: true,
                peer_type: m.peer_type,
                peer_id: m.peer_id,
            })
            .collect();

        Ok(sessions)
    }

    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        // Try to open the session to get a handle, then load history
        let llm_messages: Vec<LlmMessage> = {
            let mut manager = self.session_manager.write().await;
            if let Ok(Some(handle)) = manager.open_session(session_key).await {
                handle.load_history().await?
            } else {
                // Fallback: try loading directly from storage
                let sessions_dir = manager.sessions_dir().cloned();
                drop(manager); // drop lock before async storage ops

                if let Some(dir) = sessions_dir {
                    let storage = SessionStorage::new(dir);
                    let events = storage.load_events(session_key).await?;
                    events.iter().filter_map(event_to_llm_message).collect()
                } else {
                    vec![]
                }
            }
        };

        let messages: Vec<HistoryMessage> = llm_messages
            .iter()
            .filter_map(|m| llm_message_to_history(m, include_tools))
            .take(limit)
            .collect();

        Ok(messages)
    }

    async fn get_status(&self, session_id: &str) -> anyhow::Result<SessionStatusResult> {
        if session_id.is_empty() {
            return Err(anyhow::anyhow!("No current session available"));
        }

        let manager = self.session_manager.read().await;
        let metadata = manager.get_session_metadata(session_id).await?;

        Ok(SessionStatusResult {
            session_id: metadata.session_id,
            agent_name: metadata.agent_name,
            created_at: chrono::DateTime::from_timestamp_millis(metadata.created_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            last_activity: chrono::DateTime::from_timestamp_millis(metadata.updated_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            timestamp_utc: String::new(),
            timestamp: String::new(),
            message_count: metadata.message_count,
            usage: UsageStats {
                prompt_tokens: metadata.total_input_tokens as u64,
                completion_tokens: metadata.total_output_tokens as u64,
                context_window: metadata.context_window,
            },
            peer_type: metadata.peer_type,
            peer_id: metadata.peer_id,
            label: metadata.title,
            parent_session: metadata.parent_session_id,
        })
    }

    fn current_session_key(&self) -> String {
        self.current_session_id
            .try_read()
            .ok()
            .and_then(|id| id.clone())
            .unwrap_or_default()
    }
}

// ====================================================================================
// SessionCache — in-memory registry for testing and placeholder use
// ====================================================================================

/// In-memory session cache for testing and placeholder use.
#[derive(Debug)]
pub struct SessionCache {
    current_session: String,
    sessions: SimpleRegistry<String, SessionInfo>,
    histories: SimpleRegistry<String, Vec<HistoryMessage>>,
    statuses: SimpleRegistry<String, SessionStatusResult>,
}

impl SessionCache {
    /// Create a new in-memory session cache.
    #[must_use]
    pub fn new(current_session: impl Into<String>) -> Self {
        Self {
            current_session: current_session.into(),
            sessions: SimpleRegistry::new(),
            histories: SimpleRegistry::new(),
            statuses: SimpleRegistry::new(),
        }
    }

    /// Add a session with its history and status.
    pub fn add_session(
        &mut self,
        key: String,
        info: SessionInfo,
        history: Vec<HistoryMessage>,
        status: SessionStatusResult,
    ) {
        self.sessions.insert(key.clone(), info);
        self.histories.insert(key.clone(), history);
        self.statuses.insert(key, status);
    }
}

#[async_trait]
impl SessionRegistry for SessionCache {
    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        peer: Option<&crate::subject::Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = active_minutes.map(|m| now.saturating_sub(m as u64 * 60 * 1000));

        let filtered: Vec<SessionInfo> = self
            .sessions
            .values()
            .filter(|s| {
                let kind_match = kinds.map_or(true, |k| k.contains(&s.kind));
                let agent_match = agent_id.map_or(true, |a| s.agent_id.as_deref() == Some(a));
                let active_match = cutoff_ms.map_or(true, |_| {
                    chrono::DateTime::parse_from_rfc3339(&s.last_activity)
                        .map(|dt| dt.timestamp_millis() as u64 >= cutoff_ms.unwrap_or(0))
                        .unwrap_or(true)
                });
                let peer_match = peer_filter.as_ref().map_or(true, |(want_kind, want_id)| {
                    let (have_kind, have_id) = match (s.peer_type.as_deref(), s.peer_id.as_deref())
                    {
                        (Some(k), Some(i)) => (k, i),
                        _ => return false,
                    };
                    have_kind == want_kind.as_str() && have_id == want_id.as_str()
                });
                kind_match && peer_match && agent_match && active_match
            })
            .take(limit)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        _include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        let history = self
            .histories
            .get(&session_key.to_string())
            .cloned()
            .unwrap_or_default();
        Ok(history.into_iter().take(limit).collect())
    }

    async fn get_status(&self, session_key: &str) -> anyhow::Result<SessionStatusResult> {
        self.statuses
            .get(&session_key.to_string())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))
    }

    fn current_session_key(&self) -> String {
        self.current_session.clone()
    }
}

// ====================================================================================
// Helpers: LlmMessage → HistoryMessage conversion
// ====================================================================================

/// Convert an `LlmMessage` to a `HistoryMessage` for tool output.
fn llm_message_to_history(msg: &LlmMessage, include_tools: bool) -> Option<HistoryMessage> {
    let role = format!("{:?}", msg.role).to_lowercase();

    // Extract text content
    let content = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let (tool_calls, tool_results) = if include_tools {
        let calls: Vec<ToolCallInfo> = msg
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some(ToolCallInfo {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                _ => None,
            })
            .collect();

        let results: Vec<ToolResultInfo> = msg
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_call_id,
                    name,
                    content: result_content,
                    is_error,
                } => {
                    let result_text = result_content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some(ToolResultInfo {
                        tool_call_id: tool_call_id.clone(),
                        success: !is_error,
                        result: Some(json!({ "name": name, "content": result_text })),
                        error: if *is_error {
                            Some("Tool execution failed".to_string())
                        } else {
                            None
                        },
                    })
                }
                _ => None,
            })
            .collect();

        (
            if calls.is_empty() { None } else { Some(calls) },
            if results.is_empty() {
                None
            } else {
                Some(results)
            },
        )
    } else {
        (None, None)
    };

    Some(HistoryMessage {
        role,
        content,
        tool_calls,
        tool_results,
        timestamp: msg.timestamp.to_rfc3339(),
    })
}

// ====================================================================================
// Tests
// ====================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_registry() -> SessionCache {
        let mut registry = SessionCache::new("main");

        let session = SessionInfo {
            session_key: "test-session".to_string(),
            session_id: "abc123".to_string(),
            kind: "spawned".to_string(),
            agent_id: Some("test-agent".to_string()),
            label: Some("Test Session".to_string()),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 10,
            is_active: true,
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
        };

        let history = vec![
            HistoryMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                tool_calls: None,
                tool_results: None,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            HistoryMessage {
                role: "assistant".to_string(),
                content: "Hi there!".to_string(),
                tool_calls: None,
                tool_results: None,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            },
        ];

        let status = SessionStatusResult {
            session_id: "abc123".to_string(),
            agent_name: "test-agent".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            timestamp_utc: "2024-01-01T02:00:00Z".to_string(),
            timestamp: "2024-01-01 02:00:00 UTC".to_string(),
            message_count: 10,
            usage: UsageStats {
                prompt_tokens: 100,
                completion_tokens: 50,
                context_window: 1500,
            },
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
            label: Some("Test Session".to_string()),
            parent_session: Some("main".to_string()),
        };

        registry.add_session("test-session".to_string(), session, history, status);
        registry
    }

    #[tokio::test]
    async fn test_session_list() {
        let registry = create_test_registry();
        let tool = SessionTool::new(Box::new(registry));

        let result = tool
            .execute(json!({"action": "list", "limit": 10}))
            .await
            .unwrap();

        assert_eq!(result["total"], 1);
        assert_eq!(result["sessions"][0]["session_key"], "test-session");
    }

    #[tokio::test]
    async fn test_session_history() {
        let registry = create_test_registry();
        let tool = SessionTool::new(Box::new(registry));

        let result = tool
            .execute(json!({"action": "history", "session_key": "test-session", "limit": 10}))
            .await
            .unwrap();

        assert_eq!(result["total_messages"], 2);
        assert_eq!(result["messages"][0]["role"], "user");
        assert_eq!(result["messages"][0]["content"], "Hello");
    }

    #[tokio::test]
    async fn test_session_status() {
        let registry = create_test_registry();
        let tool = SessionTool::new(Box::new(registry));

        let result = tool
            .execute(json!({"action": "status", "session_key": "test-session"}))
            .await
            .unwrap();

        assert_eq!(result["session_id"], "abc123");
        assert_eq!(result["usage"]["context_window"], 1500);
        assert_eq!(result["peer_type"], "user");
        assert_eq!(result["peer_id"], "alice");
    }

    #[tokio::test]
    async fn test_session_status_defaults_to_current() {
        let mut registry = SessionCache::new("current-session");

        let status = SessionStatusResult {
            session_id: "current123".to_string(),
            agent_name: "main".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            timestamp_utc: "2024-01-01T02:00:00Z".to_string(),
            timestamp: "2024-01-01 02:00:00 UTC".to_string(),
            message_count: 5,
            usage: UsageStats {
                prompt_tokens: 50,
                completion_tokens: 25,
                context_window: 800,
            },
            peer_type: None,
            peer_id: None,
            label: None,
            parent_session: None,
        };

        let session = SessionInfo {
            session_key: "current-session".to_string(),
            session_id: "current123".to_string(),
            kind: "main".to_string(),
            agent_id: Some("main".to_string()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 5,
            is_active: true,
            peer_type: None,
            peer_id: None,
        };

        registry.add_session("current-session".to_string(), session, vec![], status);

        let tool = SessionTool::new(Box::new(registry));

        // Call without session_key - should default to current
        let result = tool.execute(json!({"action": "status"})).await.unwrap();

        assert_eq!(result["session_id"], "current123");
    }

    #[tokio::test]
    async fn test_session_list_empty() {
        let registry = SessionCache::new("main");
        let tool = SessionTool::new(Box::new(registry));

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 0);
    }

    #[tokio::test]
    async fn test_session_history_not_found() {
        let registry = SessionCache::new("main");
        let tool = SessionTool::new(Box::new(registry));

        let result = tool
            .execute(json!({"action": "history", "session_key": "missing"}))
            .await
            .unwrap();

        assert_eq!(result["total_messages"], 0);
    }

    #[tokio::test]
    async fn test_session_status_not_found_returns_minimal() {
        let registry = SessionCache::new("main");
        let tool = SessionTool::new(Box::new(registry));

        let result = tool
            .execute(json!({"action": "status", "session_key": "missing"}))
            .await
            .unwrap();

        assert_eq!(result["session_id"], "missing");
        assert_eq!(result["agent_name"], "unknown");
    }

    /// Helper: build a registry pre-loaded with three sessions spanning
    /// two peers (`user:alice`, `user:bob`) and two agents
    /// (`test-agent`, `other-agent`).
    fn cross_peer_registry() -> SessionCache {
        let mut registry = SessionCache::new("main");

        let alice_main = SessionInfo {
            session_key: "alice-1".to_string(),
            session_id: "alice-1".to_string(),
            kind: "main".to_string(),
            agent_id: Some("test-agent".to_string()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 5,
            is_active: true,
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
        };
        let alice_other = SessionInfo {
            session_key: "alice-2".to_string(),
            session_id: "alice-2".to_string(),
            kind: "spawned".to_string(),
            agent_id: Some("other-agent".to_string()),
            label: None,
            created_at: "2024-01-02T00:00:00Z".to_string(),
            last_activity: "2024-01-02T01:00:00Z".to_string(),
            message_count: 3,
            is_active: true,
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
        };
        let bob_main = SessionInfo {
            session_key: "bob-1".to_string(),
            session_id: "bob-1".to_string(),
            kind: "main".to_string(),
            agent_id: Some("test-agent".to_string()),
            label: None,
            created_at: "2024-01-03T00:00:00Z".to_string(),
            last_activity: "2024-01-03T01:00:00Z".to_string(),
            message_count: 7,
            is_active: true,
            peer_type: Some("user".to_string()),
            peer_id: Some("bob".to_string()),
        };

        registry.add_session(
            "alice-1".to_string(),
            alice_main,
            vec![],
            dummy_status("alice-1"),
        );
        registry.add_session(
            "alice-2".to_string(),
            alice_other,
            vec![],
            dummy_status("alice-2"),
        );
        registry.add_session("bob-1".to_string(), bob_main, vec![], dummy_status("bob-1"));
        registry
    }

    fn dummy_status(session_id: &str) -> SessionStatusResult {
        SessionStatusResult {
            session_id: session_id.to_string(),
            agent_name: "any".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            timestamp_utc: String::new(),
            timestamp: String::new(),
            message_count: 0,
            usage: UsageStats {
                prompt_tokens: 0,
                completion_tokens: 0,
                context_window: 0,
            },
            peer_type: None,
            peer_id: None,
            label: None,
            parent_session: None,
        }
    }

    #[tokio::test]
    async fn test_session_list_peer_filter_returns_only_that_peer() {
        let tool = SessionTool::new(Box::new(cross_peer_registry()));
        let result = tool
            .execute(json!({"action": "list", "peer": "user:alice"}))
            .await
            .unwrap();

        let ids: Vec<&str> = result["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["session_id"].as_str().unwrap())
            .collect();
        assert_eq!(result["total"], 2);
        assert!(ids.contains(&"alice-1"));
        assert!(ids.contains(&"alice-2"));
        assert!(!ids.contains(&"bob-1"));
    }

    #[tokio::test]
    async fn test_session_list_peer_unknown_returns_empty() {
        let tool = SessionTool::new(Box::new(cross_peer_registry()));
        let result = tool
            .execute(json!({"action": "list", "peer": "user:nobody"}))
            .await
            .unwrap();

        assert_eq!(result["total"], 0);
        assert!(result["sessions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_session_list_agent_id_filter() {
        let tool = SessionTool::new(Box::new(cross_peer_registry()));
        let result = tool
            .execute(json!({"action": "list", "agent_id": "test-agent"}))
            .await
            .unwrap();

        let ids: Vec<&str> = result["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["session_id"].as_str().unwrap())
            .collect();
        assert_eq!(result["total"], 2);
        assert!(ids.contains(&"alice-1"));
        assert!(ids.contains(&"bob-1"));
        assert!(!ids.contains(&"alice-2"));
    }

    #[tokio::test]
    async fn test_session_list_peer_and_kinds_combined() {
        let tool = SessionTool::new(Box::new(cross_peer_registry()));
        let result = tool
            .execute(json!({
                "action": "list",
                "peer": "user:alice",
                "kinds": ["spawned"],
            }))
            .await
            .unwrap();

        let ids: Vec<&str> = result["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["session_id"].as_str().unwrap())
            .collect();
        assert_eq!(result["total"], 1);
        assert_eq!(ids, vec!["alice-2"]);
    }

    #[tokio::test]
    async fn test_session_list_invalid_peer_returns_structured_error() {
        let tool = SessionTool::new(Box::new(cross_peer_registry()));
        let err = tool
            .execute(json!({"action": "list", "peer": "not-a-valid-peer"}))
            .await
            .expect_err("invalid peer must surface an error");
        assert!(err.to_string().contains("Invalid peer"));
    }

    #[tokio::test]
    async fn test_session_info_surfaces_peer_fields() {
        let tool = SessionTool::new(Box::new(cross_peer_registry()));
        let result = tool
            .execute(json!({"action": "list", "peer": "user:alice"}))
            .await
            .unwrap();

        for s in result["sessions"].as_array().unwrap() {
            assert_eq!(s["peer_type"], "user");
            assert_eq!(s["peer_id"], "alice");
        }
    }
}
