//! `SessionManagerRuntime` — root-side adapter for the `SessionRuntime` port.
//!
//! Phase 10d lifts the unified `SessionTool` into
//! `peko_tools_builtin::session`. The tool surface there speaks to a
//! [`peko_tools_builtin::session::SessionRuntime`] port trait so the
//! built-in crate can stay free of root-only deps
//! (`crate::SessionManager`, `crate::jsonl::*`, the
//! LlmMessage event-conversion helpers, etc.). This file is the
//! production adapter: it preserves the exact behaviour of the
//! legacy root-side [`crate::tools::builtin::session::SessionIntrospector`]
//! but routes through the new peko_tools_builtin port trait so the
//! tool side has no `crate::*` dependency.
//!
//! Behaviour preserved:
//! - `list_sessions` uses the manager's `list_all_sessions(false)` and
//!   applies the same kind / agent / active_minutes / peer filters;
//!   sessions without recorded peer info are skipped when a peer
//!   filter is supplied (so `branch_session_by_id` branches don't
//!   bleed across principals).
//! - `get_history` opens the session, calls `load_history` on the
//!   handle, and converts the resulting `LlmMessage`s through the
//!   shared `llm_message_to_history` helper. If opening fails, falls
//!   back to `SessionStorage::load_events` (preserves the legacy
//!   storage-only path).
//! - `get_status` reads session metadata and converts it into the
//!   `SessionStatusResult` DTO. Empty `session_id` is rejected up
//!   front.
//! - `current_session_key` reads the active principal's current
//!   session id from the shared `Arc<RwLock<Option<String>>>`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use peko_message::LlmMessage;
use peko_subject::Subject;
use peko_tools_builtin::session::{
    HistoryMessage, SessionInfo, SessionRuntime, SessionStatusResult, ToolCallInfo, ToolResultInfo,
    UsageStats,
};

use peko_message::ContentBlock;
use peko_session::jsonl::SessionStorage;
use peko_session::message_conversion::event_to_llm_message;
use peko_session::SessionManager;

/// Adapter that exposes the real `SessionManager` through the
/// `SessionRuntime` port trait.
pub struct SessionManagerRuntime {
    session_manager: Arc<tokio::sync::RwLock<SessionManager>>,
    current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
}

impl SessionManagerRuntime {
    /// Build a new runtime wrapping the supplied manager.
    #[must_use]
    pub fn new(
        session_manager: Arc<tokio::sync::RwLock<SessionManager>>,
        current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
    ) -> Self {
        Self {
            session_manager,
            current_session_id,
        }
    }
}

#[async_trait]
impl SessionRuntime for SessionManagerRuntime {
    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        peer: Option<&Subject>,
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
                last_total_tokens: metadata.last_total_tokens as u64,
                model_context_limit: metadata.model_context_limit,
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

/// Convert an `LlmMessage` to a `HistoryMessage` for tool output.
///
/// Mirrors the legacy root-side `llm_message_to_history` so the
/// runtime adapter and the production adapter produce identical
/// shapes; a JSON-roundtrip pin test in `peko_tools_builtin::session`
/// catches any drift between the two.
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
