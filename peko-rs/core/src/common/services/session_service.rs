//! Session Service
//!
//! Provides unified session management for both CLI and HTTP API.
//! Handles session listing, history retrieval, branching, and deletion.

use crate::common::paths::PathResolver;
use anyhow::{Context, Result};
use peko_auth::Subject;
use peko_session::events::SessionEvent;
use peko_session::metadata_controller::MetadataController;
use peko_session::session_info::{
    session_event_to_history, BranchResult, HistoryEvent, HistoryQuery, HistoryResult,
    HistorySummary, SessionDetails, SessionInfo,
};
use peko_session::sync::SyncSessionStorage;
use peko_session::SessionManager;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info};

/// Unified session service
pub struct SessionService {
    path_resolver: Arc<dyn peko_subject::PathResolverLike>,
}

impl SessionService {
    /// Create a new session service
    #[must_use]
    pub fn new(path_resolver: PathResolver) -> Self {
        let path_resolver: Arc<dyn peko_subject::PathResolverLike> =
            Arc::new(peko_session::DefaultPathResolver::with_data_dir(
                path_resolver.data_dir().to_path_buf(),
            ));
        Self { path_resolver }
    }

    /// List sessions for an agent
    pub async fn list_sessions(&self, agent_name: &str) -> Result<Vec<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;

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

    /// List sessions for an agent, including the active session ID for a peer.
    pub async fn list_sessions_with_active(
        &self,
        agent_name: &str,
        peer: &peko_auth::Subject,
    ) -> Result<(Vec<SessionInfo>, Option<String>)> {
        let sessions = self.list_sessions(agent_name).await?;

        let active_session = if !sessions.is_empty() {
            let sessions_dir = self.get_sessions_dir(agent_name).await?;
            let mut controller = MetadataController::new(&sessions_dir);
            let peer_key = peko_session::key::derive_base_session_key(agent_name, peer);
            controller
                .get_active_session_id(&peer_key)
                .await
                .ok()
                .flatten()
        } else {
            None
        };

        Ok((sessions, active_session))
    }

    /// Get session info by ID
    pub async fn get_session(
        &self,
        agent_name: &str,
        session_id: &str,
    ) -> Result<Option<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;

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
        session_id: &str,
        query: HistoryQuery,
    ) -> Result<HistoryResult> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;
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
        // Pre-compute session_started_at from the first SessionCreated event
        // so the Session marker we emit at index 0 carries a meaningful value.
        // (CLI's `peko log` doesn't render the Session marker, but the IPC
        // path does; keep both call sites using the same converter.)
        let session_started_at = events
            .iter()
            .find_map(|e| match e {
                SessionEvent::SessionCreated(c) => Some(c.envelope.ts.to_rfc3339()),
                _ => None,
            })
            .unwrap_or_default();

        let mut history_events: Vec<HistoryEvent> = events
            .iter()
            .filter_map(|event| self.convert_event(event, session_id, &session_started_at, &query))
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
        parent_session_id: &str,
        label: Option<String>,
    ) -> Result<BranchResult> {
        // Use SessionManager for branching
        let mut manager =
            SessionManager::for_cli(self.path_resolver.clone(), agent_name, "default");

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
            new_session_id: new_session_id.to_string(),
            parent_session_id: parent_session_id.to_string(),
            label,
        })
    }

    /// Delete a session
    ///
    /// Removes both the session JSONL file and its metadata from the index.
    pub async fn delete_session(&self, agent_name: &str, session_id: &str) -> Result<bool> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;

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
        session_id: &str,
    ) -> Result<Option<SessionDetails>> {
        let info = match self.get_session(agent_name, session_id).await? {
            Some(info) => info,
            None => return Ok(None),
        };

        // Get history for summary
        let history = self
            .get_history(
                agent_name,
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
    pub async fn session_exists(&self, agent_name: &str, session_id: &str) -> Result<bool> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;

        if !sessions_dir.exists() {
            return Ok(false);
        }

        let storage = SyncSessionStorage::new(sessions_dir);
        Ok(storage.session_exists(session_id).await)
    }

    /// Cross-agent session metadata lookup by id. Used by IPC handlers
    /// that don't know the agent name (e.g. `SessionSteer` which
    /// only carries `session_id` and `content`).
    ///
    /// Walks the `{data_dir}/sessions/` tree, looking in each
    /// `{agent}/personal` directory until it finds one whose metadata
    /// index has the session id. Returns the first match (session ids
    /// are globally unique by UUID).
    pub async fn get_session_metadata(
        &self,
        session_id: &str,
    ) -> Result<peko_session::metadata::SessionMetadata> {
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let sessions_root = self.path_resolver.sessions_root();
        let mut last_err: Option<anyhow::Error> = None;

        let agent_dirs = match std::fs::read_dir(&sessions_root) {
            Ok(rd) => rd,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "could not read sessions root {}: {e}",
                    sessions_root.display()
                ));
            }
        };

        for agent_entry in agent_dirs.flatten() {
            let agent_path = agent_entry.path();
            if !agent_path.is_dir() {
                continue;
            }
            let agent_name = match agent_entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };

            let personal_path = agent_path.join("personal");
            if !personal_path.is_dir() {
                continue;
            }

            let controller = Arc::new(RwLock::new(MetadataController::new(personal_path)));
            let mut guard = controller.write().await;
            match guard.get_metadata(session_id, false).await {
                Ok(Some(m)) => {
                    debug!("get_session_metadata: found {session_id} under agent='{agent_name}'");
                    return Ok(m);
                }
                Ok(None) => continue,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Session '{session_id}' not found")))
    }

    /// Resolve a session ID, falling back to the active session if none provided
    pub async fn resolve_session_id(
        &self,
        agent_name: &str,
        user: &str,
        session_id: Option<String>,
    ) -> Result<String> {
        match session_id {
            Some(id) => Ok(id),
            None => {
                let mut manager =
                    SessionManager::for_cli(self.path_resolver.clone(), agent_name, user);
                let peer = Subject::User(user.to_string());
                match manager.get_active_session_id(&peer).await? {
                    Some(id) => Ok(id),
                    None => Err(anyhow::anyhow!(
                        "No active session for agent '{agent_name}'. \
                         Specify a session ID explicitly, or start a conversation with `peko send`."
                    )),
                }
            }
        }
    }

    /// Open a session by ID (returns the unified Session)
    pub async fn open_session(
        &self,
        agent_name: &str,
        session_id: &str,
        user: &str,
    ) -> Result<peko_session::unified::Session> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;
        let peer = Subject::User(user.to_string());
        peko_session::unified::Session::open_by_id(
            agent_name,
            session_id,
            &sessions_dir,
            Some(&peer),
        )
        .await
        .with_context(|| format!("Failed to open session '{session_id}'"))
    }

    /// List sessions with metadata synced from JSONL (source of truth)
    pub async fn list_sessions_synced(&self, agent_name: &str) -> Result<Vec<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;

        if !sessions_dir.exists() {
            return Ok(vec![]);
        }

        let mut controller = MetadataController::new(&sessions_dir);
        let entries = controller.list_metadata(true).await?;

        let sessions: Vec<SessionInfo> = entries.into_iter().map(Into::into).collect();

        Ok(sessions)
    }

    /// Get session metadata synced from JSONL
    pub async fn get_session_synced(
        &self,
        agent_name: &str,
        session_id: &str,
    ) -> Result<Option<SessionInfo>> {
        let sessions_dir = self.get_sessions_dir(agent_name).await?;

        if !sessions_dir.exists() {
            return Ok(None);
        }

        let mut controller = MetadataController::new(&sessions_dir);
        let metadata = controller.get_metadata(session_id, true).await?;
        Ok(metadata.map(Into::into))
    }

    /// Get sessions directory for an agent
    pub async fn get_sessions_dir(&self, agent_name: &str) -> Result<PathBuf> {
        let sessions_dir = self.path_resolver.agent_sessions_dir(agent_name);
        Ok(sessions_dir)
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
    /// Convert `SessionEvent` to `HistoryEvent`, applying query filters.
    fn convert_event(
        &self,
        event: &SessionEvent,
        session_id: &str,
        session_started_at: &str,
        query: &HistoryQuery,
    ) -> Option<HistoryEvent> {
        let event_type = event.event_type();

        // Filter based on query params
        if !query.include_tool_calls && (event_type == "tool.call" || event_type == "tool.result") {
            return None;
        }

        if !query.include_thinking && event_type == "thinking" {
            return None;
        }

        session_event_to_history(event, session_id, session_started_at)
    }
}

