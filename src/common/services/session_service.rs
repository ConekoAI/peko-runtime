//! Session Service
//!
//! Provides unified session management for both CLI and HTTP API.
//! Handles session listing, history retrieval, branching, and deletion.

use crate::common::paths::PathResolver;
use crate::session::events::SessionEvent;
use crate::session::index::{SessionEntry, SessionIndex};
use crate::session::sync::SyncSessionStorage;
use crate::session::SessionManager;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::{debug, info};

/// Session information
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub turn_count: u32,
    pub message_count: usize,
    pub total_tokens: usize,
    pub parent_session_id: Option<String>,
    pub title: Option<String>,
    pub ended: bool,
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
            total_tokens: entry.total_tokens,
            parent_session_id: entry.parent_session_id,
            title: entry.title,
            ended: entry.ended,
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
#[derive(Debug, Clone)]
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

        let mut index = SessionIndex::open(sessions_dir);
        let entries = index
            .list_all()
            .await
            .with_context(|| format!("Failed to list sessions for agent '{}'", agent_name))?;

        // Filter to only sessions for this agent and convert
        let sessions: Vec<SessionInfo> = entries
            .into_iter()
            .filter(|e| e.agent_name == agent_name)
            .map(|e| e.into())
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

        let mut index = SessionIndex::open(sessions_dir);
        let entry = index
            .get(session_id)
            .await
            .with_context(|| format!("Failed to get session '{}'", session_id))?;

        Ok(entry.map(|e| e.into()))
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
            anyhow::bail!("Session '{}' not found", session_id);
        }

        // Load events
        let events = storage
            .load_events(session_id)
            .await
            .with_context(|| format!("Failed to load events for session '{}'", session_id))?;

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
        let mut manager = SessionManager::for_cli(agent_name, team);

        // Verify parent exists
        let _parent_metadata = manager
            .get_session_metadata(parent_session_id)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Parent session '{}' not found for agent '{}'",
                    parent_session_id,
                    agent_name
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
            anyhow::bail!(
                "Session '{}' not found for agent '{}'",
                session_id,
                agent_name
            );
        }

        // Delete the session file
        storage
            .delete_session(session_id)
            .await
            .with_context(|| format!("Failed to delete session '{}'", session_id))?;

        // CRITICAL: Remove from SessionIndex so it doesn't appear in listings
        let mut index = SessionIndex::open(&sessions_dir);
        index.remove(session_id).await?;
        index.save().await?;

        // Also remove from active preference if set
        let active_pref_path = sessions_dir.join(".active.json");
        if active_pref_path.exists() {
            if let Ok(content) = tokio::fs::read_to_string(&active_pref_path).await {
                if let Ok(pref) = serde_json::from_str::<serde_json::Value>(&content) {
                    if pref.get("session_id").and_then(|v| v.as_str()) == Some(session_id) {
                        let _ = tokio::fs::remove_file(&active_pref_path).await;
                    }
                }
            }
        }

        info!(
            "Deleted session '{}' for agent '{}'",
            session_id, agent_name
        );

        Ok(true)
    }

    /// Set active session preference
    pub async fn set_active_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        session_id: &str,
    ) -> Result<()> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        // Ensure directory exists
        tokio::fs::create_dir_all(&sessions_dir).await?;

        // Verify session exists
        let mut index = SessionIndex::open(sessions_dir.clone());
        if index.get(session_id).await?.is_none() {
            anyhow::bail!(
                "Session '{}' not found for agent '{}'",
                session_id,
                agent_name
            );
        }

        // Save preference
        let pref_path = sessions_dir.join(".active.json");
        let pref = serde_json::json!({
            "session_id": session_id,
            "set_at": chrono::Utc::now().to_rfc3339(),
            "set_by": "cli",
        });

        let temp_path = pref_path.with_extension("tmp");
        tokio::fs::write(&temp_path, serde_json::to_string_pretty(&pref)?).await?;
        tokio::fs::rename(&temp_path, &pref_path).await?;

        info!(
            "Set active session preference for '{}' to '{}'",
            agent_name, session_id
        );

        Ok(())
    }

    /// Get active session preference
    pub async fn get_active_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
    ) -> Result<Option<String>> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;
        let pref_path = sessions_dir.join(".active.json");

        if !pref_path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&pref_path).await?;
        let pref: serde_json::Value = serde_json::from_str(&content)?;

        Ok(pref
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from))
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

    /// Get sessions directory for an agent
    async fn get_sessions_dir(&self, agent_name: &str, team: Option<&str>) -> Result<PathBuf> {
        // Use path resolver directly
        let sessions_dir = self.path_resolver.agent_sessions_dir(agent_name, team);

        Ok(sessions_dir)
    }

    /// Convert SessionEvent to HistoryEvent
    fn convert_event(&self, event: &SessionEvent, query: &HistoryQuery) -> Option<HistoryEvent> {
        let event_type = event.event_type();

        // Filter based on query params
        if !query.include_tool_calls {
            if event_type == "tool.call" || event_type == "tool.result" {
                return None;
            }
        }

        if !query.include_thinking && event_type == "thinking" {
            return None;
        }

        Some(match event {
            SessionEvent::SessionCreated(e) => HistoryEvent::Session {
                timestamp: e.envelope.ts.to_rfc3339(),
            },
            SessionEvent::UserMessage(e) => HistoryEvent::Message {
                role: "user".to_string(),
                content: e.content.clone(),
                timestamp: e.envelope.ts.to_rfc3339(),
            },
            SessionEvent::AssistantMessage(e) => HistoryEvent::Message {
                role: "assistant".to_string(),
                content: e.content.clone(),
                timestamp: e.envelope.ts.to_rfc3339(),
            },
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
