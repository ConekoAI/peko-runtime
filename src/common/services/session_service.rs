//! Session Service
//!
//! Provides unified session management for both CLI and HTTP API.
//! Handles session listing, history retrieval, branching, and deletion.

use crate::common::paths::PathResolver;
use crate::session::events::SessionEvent;
use crate::session::metadata_controller::MetadataController;
use crate::session::sync::SyncSessionStorage;
use crate::session::types::Peer;
use crate::session::SessionEntry;
use crate::session::SessionManager;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::{debug, info};

/// Session information
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub turn_count: u32,
    pub message_count: usize,
    /// Current context window size (`total_tokens` from last assistant message)
    pub context_window: usize,
    /// Cumulative input tokens across all assistant messages
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    pub total_output_tokens: usize,
    pub parent_session_id: Option<String>,
    pub title: Option<String>,
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
            context_window: entry.context_window,
            total_input_tokens: entry.total_input_tokens,
            total_output_tokens: entry.total_output_tokens,
            parent_session_id: entry.parent_session_id,
            title: entry.title,
        }
    }
}

/// History event types
#[derive(Debug, Clone)]
pub enum HistoryEvent {
    Session {
        timestamp: String,
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
    },
    ToolResult {
        tool_call_id: String,
        output: Option<String>,
        error: Option<String>,
    },
    Thinking {
        content: String,
    },
    ModelChange {
        provider: String,
        model_id: String,
    },
    Compaction {
        summary: String,
    },
    Custom {
        custom_type: String,
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
#[derive(Debug, Clone)]
pub struct SessionDetails {
    pub info: SessionInfo,
    pub history_summary: HistorySummary,
}

/// History summary for session overview
#[derive(Debug, Clone, Default)]
pub struct HistorySummary {
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub thinking_blocks: usize,
}

/// Unified session service
pub struct SessionService {
    path_resolver: PathResolver,
}

impl SessionService {
    /// Create a new session service
    #[must_use]
    pub fn new(path_resolver: PathResolver) -> Self {
        Self { path_resolver }
    }

    /// List sessions for an agent
    pub async fn list_sessions(
        &self,
        agent_name: &str,
        team: Option<&str>,
    ) -> Result<Vec<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        if !sessions_dir.exists() {
            return Ok(vec![]);
        }

        let mut controller = MetadataController::new(&sessions_dir);
        let entries = controller
            .list_all_from_index()
            .await
            .with_context(|| format!("Failed to list sessions for agent '{agent_name}'"))?;

        // Filter to only sessions for this agent and convert
        let sessions: Vec<SessionInfo> = entries
            .into_iter()
            .filter(|e| e.agent_name == agent_name)
            .map(std::convert::Into::into)
            .collect();

        debug!(
            "Found {} sessions for agent '{}'",
            sessions.len(),
            agent_name
        );

        Ok(sessions)
    }

    /// Get session info by ID
    pub async fn get_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
    ) -> Result<Option<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        if !sessions_dir.exists() {
            return Ok(None);
        }

        let mut controller = MetadataController::new(&sessions_dir);
        let entry = controller
            .get_entry_from_index(session_id)
            .await
            .with_context(|| format!("Failed to get session '{session_id}'"))?;

        Ok(entry.map(std::convert::Into::into))
    }

    /// Get session history
    pub async fn get_history(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
        query: HistoryQuery,
    ) -> Result<HistoryResult> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;
        let storage = SyncSessionStorage::new(sessions_dir);

        // Verify session exists
        if !storage.session_exists(session_id).await {
            anyhow::bail!("Session '{session_id}' not found");
        }

        // Load events
        let events = storage
            .load_events(session_id)
            .await
            .with_context(|| format!("Failed to load events for session '{session_id}'"))?;

        // Convert and filter
        let mut history_events: Vec<HistoryEvent> = events
            .iter()
            .filter_map(|event| self.convert_event(event, &query))
            .collect();

        // Apply pagination (newest first)
        history_events.reverse();
        let total = history_events.len();
        let limit = query.limit.min(100);
        let offset = query
            .cursor
            .as_ref()
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0);

        let events: Vec<HistoryEvent> = history_events
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();

        let has_more = offset + events.len() < total;
        let cursor = if has_more {
            Some((offset + events.len()).to_string())
        } else {
            None
        };

        Ok(HistoryResult {
            session_id: session_id.to_string(),
            events,
            cursor,
            has_more,
        })
    }

    /// Branch a session
    pub async fn branch_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        parent_session_id: &str,
        label: Option<String>,
    ) -> Result<BranchResult> {
        // Use SessionManager for branching
        let mut manager =
            SessionManager::for_cli(self.path_resolver.clone(), agent_name, team, "default");

        // Verify parent exists
        let _parent_metadata = manager
            .get_session_metadata(parent_session_id)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Parent session '{parent_session_id}' not found for agent '{agent_name}'"
                )
            })?;

        // Perform branch
        let new_session_id = manager
            .branch_session_by_id(parent_session_id, label.clone())
            .await?;

        info!(
            "Branched session '{}' -> '{}' for agent '{}'",
            parent_session_id, new_session_id, agent_name
        );

        Ok(BranchResult {
            new_session_id,
            parent_session_id: parent_session_id.to_string(),
            label,
        })
    }

    /// Delete a session
    ///
    /// Removes both the session JSONL file and its metadata from the index.
    pub async fn delete_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
    ) -> Result<bool> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        // Use SyncSessionStorage for deletion
        let storage = SyncSessionStorage::new(sessions_dir.clone());

        // Check if session exists
        if !storage.session_exists(session_id).await {
            anyhow::bail!("Session '{session_id}' not found for agent '{agent_name}'");
        }

        // Delete the session file
        storage
            .delete_session(session_id)
            .await
            .with_context(|| format!("Failed to delete session '{session_id}'"))?;

        // CRITICAL: Remove from index so it doesn't appear in listings
        let mut controller = MetadataController::new(&sessions_dir);
        controller.delete_metadata(session_id).await?;

        // Note: If this was the active session for a peer, peers.json will still
        // reference it. The next auto-resume will create a new session.
        // SessionManager::switch_session() should be used to explicitly change active sessions.

        info!(
            "Deleted session '{}' for agent '{}'",
            session_id, agent_name
        );

        Ok(true)
    }

    /// Get session details with history summary
    pub async fn get_session_details(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
    ) -> Result<Option<SessionDetails>> {
        let info = match self.get_session(agent_name, team, session_id).await? {
            Some(info) => info,
            None => return Ok(None),
        };

        // Get history for summary
        let history = self
            .get_history(
                agent_name,
                team,
                session_id,
                HistoryQuery {
                    include_tool_calls: true,
                    include_thinking: true,
                    limit: 10000, // Get all for summary
                    cursor: None,
                },
            )
            .await?;

        let mut summary = HistorySummary::default();
        for event in &history.events {
            match event {
                HistoryEvent::Message { role, .. } => {
                    if role == "user" {
                        summary.user_messages += 1;
                    } else if role == "assistant" {
                        summary.assistant_messages += 1;
                    }
                }
                HistoryEvent::ToolCall { .. } => summary.tool_calls += 1,
                HistoryEvent::Thinking { .. } => summary.thinking_blocks += 1,
                _ => {}
            }
        }

        Ok(Some(SessionDetails {
            info,
            history_summary: summary,
        }))
    }

    /// Check if a session exists
    pub async fn session_exists(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
    ) -> Result<bool> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        if !sessions_dir.exists() {
            return Ok(false);
        }

        let storage = SyncSessionStorage::new(sessions_dir);
        Ok(storage.session_exists(session_id).await)
    }

    /// Resolve a session ID, falling back to the active session if none provided
    pub async fn resolve_session_id(
        &self,
        agent_name: &str,
        team: Option<&str>,
        user: &str,
        session_id: Option<String>,
    ) -> Result<String> {
        match session_id {
            Some(id) => Ok(id),
            None => {
                let mut manager =
                    SessionManager::for_cli(self.path_resolver.clone(), agent_name, team, user);
                let peer = Peer::User(user.to_string());
                match manager.get_active_session_id(&peer).await? {
                    Some(id) => Ok(id),
                    None => Err(anyhow::anyhow!(
                        "No active session for agent '{agent_name}'. \
                         Run 'pekobot session list {agent_name}' to see available sessions, \
                         or specify a session ID explicitly."
                    )),
                }
            }
        }
    }

    /// Open a session by ID (returns the unified Session)
    pub async fn open_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
        user: &str,
    ) -> Result<crate::session::unified::Session> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;
        let peer = Peer::User(user.to_string());
        crate::session::unified::Session::open_by_id(agent_name, session_id, &sessions_dir, Some(&peer))
            .await
            .with_context(|| format!("Failed to open session '{session_id}'"))
    }

    /// List sessions with metadata synced from JSONL (source of truth)
    pub async fn list_sessions_synced(
        &self,
        agent_name: &str,
        team: Option<&str>,
    ) -> Result<Vec<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        if !sessions_dir.exists() {
            return Ok(vec![]);
        }

        let mut controller = MetadataController::new(&sessions_dir);
        let entries = controller.list_metadata(true).await?;

        let sessions: Vec<SessionInfo> = entries
            .into_iter()
            .map(|e| e.to_entry().into())
            .collect();

        Ok(sessions)
    }

    /// Get session metadata synced from JSONL
    pub async fn get_session_synced(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
    ) -> Result<Option<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        if !sessions_dir.exists() {
            return Ok(None);
        }

        let mut controller = MetadataController::new(&sessions_dir);
        let metadata = controller.get_metadata(session_id, true).await?;
        Ok(metadata.map(|m| m.to_entry().into()))
    }

    /// Get sessions directory for an agent
    pub async fn get_sessions_dir(&self, agent_name: &str, team: Option<&str>) -> Result<PathBuf> {
        let sessions_dir = self.path_resolver.agent_sessions_dir(agent_name, team);
        Ok(sessions_dir)
    }

    /// Convert `SessionEvent` to `HistoryEvent`
    fn convert_event(&self, event: &SessionEvent, query: &HistoryQuery) -> Option<HistoryEvent> {
        let event_type = event.event_type();

        // Filter based on query params
        if !query.include_tool_calls && (event_type == "tool.call" || event_type == "tool.result") {
            return None;
        }

        if !query.include_thinking && event_type == "thinking" {
            return None;
        }

        Some(match event {
            SessionEvent::SessionCreated(e) => HistoryEvent::Session {
                timestamp: e.envelope.ts.to_rfc3339(),
            },
            SessionEvent::MessageV2(msg) => {
                // Use unified SessionMessage format
                HistoryEvent::Message {
                    role: match msg.role() {
                        crate::types::message::MessageRole::User => "user",
                        crate::types::message::MessageRole::Assistant => "assistant",
                        crate::types::message::MessageRole::System => "system",
                        crate::types::message::MessageRole::Tool => "tool",
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
            },
            SessionEvent::ToolResult(e) => HistoryEvent::ToolResult {
                tool_call_id: e.tool_call_id.clone(),
                output: e.output.clone(),
                error: e.error.clone(),
            },
            SessionEvent::Thinking(e) => HistoryEvent::Thinking {
                content: e.content.clone(),
            },
            SessionEvent::System(e) => HistoryEvent::Message {
                role: "system".to_string(),
                content: e.detail.to_string(),
                timestamp: e.envelope.ts.to_rfc3339(),
            },
            SessionEvent::HookTrigger(e) => HistoryEvent::Custom {
                custom_type: format!("hook:{:?}", e.hook_type),
            },
            SessionEvent::SpawnRequest(_)
            | SessionEvent::SpawnResult(_)
            | SessionEvent::A2aSent(_)
            | SessionEvent::A2aReceived(_)
            | SessionEvent::SessionEnded(_) => {
                // These events don't have simple display representations
                return None;
            }
        })
    }
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

    #[test]
    fn test_session_info_from_entry() {
        let entry = SessionEntry::new(
            "sess_123".to_string(),
            "myagent".to_string(),
            "sess_123.jsonl".to_string(),
        );

        let info: SessionInfo = entry.into();
        assert_eq!(info.id, "sess_123");
        assert_eq!(info.agent_name, "myagent");
    }
}
