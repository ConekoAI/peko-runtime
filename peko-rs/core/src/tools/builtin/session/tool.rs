//! Unified `session` tool — single introspection entry point that
//! dispatches by `action` over 12 operations
//! (`status` / `list` / `history` / `search` / `branch` / `rename` /
//! `archive` / `unarchive` / `delete` / `compact` / `new` / `resume`).
//!
//! Replaces the legacy `session_status`, `sessions_list`, `sessions_history`
//! tools (Issue 013, expanded by PR #351 with the 9 lifecycle operations).
//! Speaks to the [`SessionRuntime`] port.

use async_trait::async_trait;
use peko_tools_core::traits::Tool;
use serde::Deserialize;
use serde_json::json;

use super::{HistoryMessage, SessionInfo, SessionStatusResult, SharedSessionRuntime};

/// Unified session introspection tool.
pub struct SessionTool {
    runtime: SharedSessionRuntime,
}

impl SessionTool {
    /// Build a tool bound to the given session runtime.
    #[must_use]
    pub fn new(runtime: SharedSessionRuntime) -> Self {
        Self { runtime }
    }

    async fn get_status_action(
        &self,
        session_key: Option<&str>,
    ) -> anyhow::Result<SessionStatusResult> {
        let session_id = session_key
            .map(String::from)
            .unwrap_or_else(|| self.runtime.current_session_key());
        self.runtime.get_status(&session_id).await
    }

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

    /// Extract a required `session_key` param with an actionable error.
    fn require_session_key<'a>(
        params: &'a serde_json::Value,
        action: &str,
    ) -> anyhow::Result<&'a str> {
        params
            .get("session_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("'{action}' requires the 'session_key' parameter"))
    }
}

/// Actions supported by the `session` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionAction {
    Status,
    List,
    History,
    Search,
    Branch,
    Rename,
    Archive,
    Unarchive,
    Delete,
    Compact,
    New,
    Resume,
}

#[async_trait]
impl Tool for SessionTool {
    fn name(&self) -> &'static str {
        "session"
    }

    fn description(&self) -> String {
        r"Single tool with **12 operations** for inspecting and managing your persisted sessions. The `action` parameter is REQUIRED and MUST be one of:

  delete | compact | new | resume | archive | unarchive | search | branch | rename | status | list | history

Per-action semantics (the action you choose determines which other params apply):
- delete: remove a session (session_key required; recursive:true also deletes its descendants, children first)
- compact: schedule summarization (session_key required) — fires at the next iteration for the current session, at its next run for others
- new: start a fresh chapter for the current conversation (optional title) — the old chapter is kept under '<live-id>#<timestamp>'; takes effect on the NEXT incoming message, not this turn
- resume: swap a chapter/session back into the live slot (target required); takes effect on the NEXT incoming message
- archive / unarchive: hide/show a session in list (session_key required); archived sessions refuse resume/compact
- search: case-insensitive text search across session transcripts (query required; optional peer filter)
- branch: copy a session into a new stored branch (session_key required; optional label)
- rename: retitle a session (session_key + title required)
- status: one session's metadata + token usage (session_key optional, defaults to current)
- list: query sessions (filters: kinds, peer, agent_id, active_minutes; archived hidden unless include_archived:true)
- history: messages of a session (session_key, include_tools)

Kinds (set by the engine, observed via `list`): 'user' (your live session), 'chapter' (rotated conversations), 'spawned' (subagent sessions), 'branch' (copies via `branch`), 'cron' (scheduled-run sessions).

To RUN work in a session, use the Agent tool instead — spawned sessions appear here as kind 'spawned' and can be re-attached with Agent's resume_session param. You cannot modify the session you are currently running in (use 'new' or 'compact' for that)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "history", "search", "branch", "rename", "archive", "unarchive", "delete", "compact", "new", "resume"],
                    "description": "What to do: status/list/history read; search finds text; branch/rename/archive/unarchive/delete/compact manage a session; new/resume rotate the current conversation's chapter"
                },
                "session_key": {
                    "type": "string",
                    "description": "Target session. Required for history/branch/rename/archive/unarchive/delete/compact. Optional for status (defaults to current session)"
                },
                "query": {
                    "type": "string",
                    "description": "Required for 'search': case-insensitive substring to find in transcripts"
                },
                "label": {
                    "type": "string",
                    "description": "Optional for 'branch': label/title for the new branch"
                },
                "title": {
                    "type": "string",
                    "description": "Required for 'rename'; optional for 'new' (labels the archived chapter)"
                },
                "target": {
                    "type": "string",
                    "description": "Required for 'resume': chapter or session id to swap into the live slot"
                },
                "recursive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional for 'delete': also delete the session's descendants (children first)"
                },
                "include_archived": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional for 'list': include archived sessions (hidden by default)"
                },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional filter for 'list': e.g., ['main', 'spawned', 'cron']"
                },
                "peer": {
                    "type": "string",
                    "description": "Optional filter for 'list' and 'search': cross-peer lookup, e.g. 'user:alice' or 'public'. When omitted, results span all peers."
                },
                "agent_id": {
                    "type": "string",
                    "description": "Optional filter for 'list': single agent name"
                },
                "limit": {
                    "type": "integer",
                    "default": 50,
                    "description": "Max results for 'list', 'history', or 'search'"
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

                // A missing/unknown session is a real error — never
                // fabricate a zeroed status for it.
                let mut status = self.get_status_action(session_key).await?;

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
                        s.parse::<peko_subject::Subject>()
                            .map_err(|e| anyhow::anyhow!("Invalid peer '{s}': {e}"))?,
                    ),
                    None => None,
                };
                let agent_id = params.get("agent_id").and_then(|v| v.as_str());
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let active_minutes = params.get("active_minutes").and_then(|v| v.as_i64());
                let include_archived = params
                    .get("include_archived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let kinds_ref = kinds.as_deref();
                let sessions = self
                    .runtime
                    .list_sessions(
                        kinds_ref,
                        peer.as_ref(),
                        agent_id,
                        limit,
                        active_minutes,
                        include_archived,
                    )
                    .await?;
                Ok(Self::build_list_response(sessions))
            }
            SessionAction::History => {
                let session_key = params
                    .get("session_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.runtime.current_session_key())
                    .to_string();
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
                let include_tools = params
                    .get("include_tools")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let messages = self
                    .runtime
                    .get_history(&session_key, limit, include_tools)
                    .await?;
                Ok(Self::build_history_response(&session_key, messages))
            }
            SessionAction::Search => {
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'search' requires the 'query' parameter"))?;
                let peer_str = params.get("peer").and_then(|v| v.as_str());
                let peer = match peer_str {
                    Some(s) => Some(
                        s.parse::<peko_subject::Subject>()
                            .map_err(|e| anyhow::anyhow!("Invalid peer '{s}': {e}"))?,
                    ),
                    None => None,
                };
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

                let hits = self
                    .runtime
                    .search_sessions(query, peer.as_ref(), limit)
                    .await?;
                Ok(json!({
                    "total": hits.len(),
                    "hits": hits,
                }))
            }
            SessionAction::Branch => {
                let session_key = Self::require_session_key(&params, "branch")?;
                let label = params
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let outcome = self.runtime.branch_session(session_key, label).await?;
                Ok(serde_json::to_value(outcome)?)
            }
            SessionAction::Rename => {
                let session_key = Self::require_session_key(&params, "rename")?;
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'rename' requires the 'title' parameter"))?
                    .to_string();

                self.runtime.rename_session(session_key, title).await?;
                Ok(json!({ "renamed": session_key }))
            }
            SessionAction::Archive | SessionAction::Unarchive => {
                let archived = action == SessionAction::Archive;
                let verb = if archived { "archive" } else { "unarchive" };
                let session_key = Self::require_session_key(&params, verb)?;

                self.runtime.set_archived(session_key, archived).await?;
                Ok(json!({
                    "session_key": session_key,
                    "archived": archived,
                }))
            }
            SessionAction::Delete => {
                let session_key = Self::require_session_key(&params, "delete")?;
                let recursive = params
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let outcome = self.runtime.delete_session(session_key, recursive).await?;
                Ok(serde_json::to_value(outcome)?)
            }
            SessionAction::Compact => {
                let session_key = Self::require_session_key(&params, "compact")?;

                let outcome = self.runtime.request_compaction(session_key).await?;
                Ok(serde_json::to_value(outcome)?)
            }
            SessionAction::New => {
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let outcome = self.runtime.new_chapter(title).await?;
                Ok(serde_json::to_value(outcome)?)
            }
            SessionAction::Resume => {
                let target = params
                    .get("target")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'resume' requires the 'target' parameter"))?;

                let outcome = self.runtime.resume_chapter(target).await?;
                Ok(serde_json::to_value(outcome)?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::session::{
        HistoryMessage, SessionCache, SessionInfo, SessionStatusResult, UsageStats,
    };
    use serde_json::json;
    use std::sync::Arc;

    /// F5 (2026-08-11 field test, Addendum 3): the model was anchoring on
    /// the legacy 3 actions (`status` / `list` / `history`) and refusing
    /// to call any of the 9 PR #351 lifecycle operations. The schema
    /// enum listed all 12, but the model wasn't trusting it. Pin the
    /// description here so any future edit that drops one of the 12
    /// action names fails the test — defense-in-depth against the
    /// "register without surfacing in description" omission pattern.
    #[test]
    fn description_names_all_12_actions() {
        let cache = SessionCache::new("test");
        let tool = SessionTool::new(Arc::new(cache).as_shared());
        let desc = tool.description();

        // The 12 actions, in the order they appear in `SessionAction`.
        // If `SessionAction` ever grows, bump this list in lockstep.
        let expected_actions = [
            "status",
            "list",
            "history",
            "search",
            "branch",
            "rename",
            "archive",
            "unarchive",
            "delete",
            "compact",
            "new",
            "resume",
        ];
        assert_eq!(
            expected_actions.len(),
            12,
            "test bug: expected_actions must have 12 entries"
        );

        for action in expected_actions {
            assert!(
                desc.contains(action),
                "session description must name the `{action}` action (F5: model anchored on \
                 legacy 3 and refused lifecycle ops; description must surface every action)"
            );
        }

        // Lead-with-count: the description must advertise the action
        // count up front so the model sees all 12 before any per-action
        // bullet (defeats primacy bias on the legacy 3).
        assert!(
            desc.contains("12 operations")
                || desc.contains("12 actions")
                || desc.contains("12 op"),
            "session description must lead with the action count (F5: defeats primacy bias)"
        );
    }

    fn create_test_cache() -> Arc<SessionCache> {
        let cache = SessionCache::new("main");

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
            archived: false,
            run_active: false,
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
                last_total_tokens: 1500,
                model_context_limit: Some(128_000),
            },
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
            label: Some("Test Session".to_string()),
            parent_session: Some("main".to_string()),
        };

        cache.add_session("test-session".to_string(), session, history, status);
        Arc::new(cache)
    }

    #[tokio::test]
    async fn test_session_list() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "list", "limit": 10}))
            .await
            .unwrap();

        assert_eq!(result["total"], 1);
        assert_eq!(result["sessions"][0]["session_key"], "test-session");
    }

    #[tokio::test]
    async fn test_session_history() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

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
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "status", "session_key": "test-session"}))
            .await
            .unwrap();

        assert_eq!(result["session_id"], "abc123");
        assert_eq!(result["usage"]["last_total_tokens"], 1500);
        assert_eq!(result["usage"]["model_context_limit"], 128_000);
        assert_eq!(result["peer_type"], "user");
        assert_eq!(result["peer_id"], "alice");
    }

    #[tokio::test]
    async fn test_session_status_defaults_to_current() {
        let cache = SessionCache::new("current-session");

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
                last_total_tokens: 800,
                model_context_limit: None,
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
            archived: false,
            run_active: false,
        };

        cache.add_session("current-session".to_string(), session, vec![], status);

        let tool = SessionTool::new(Arc::new(cache).as_shared());

        let result = tool.execute(json!({"action": "status"})).await.unwrap();

        assert_eq!(result["session_id"], "current123");
    }

    #[tokio::test]
    async fn test_session_list_empty() {
        let cache = Arc::new(SessionCache::new("main"));
        let tool = SessionTool::new(cache.as_shared());

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 0);
    }

    #[tokio::test]
    async fn test_session_history_not_found() {
        let cache = Arc::new(SessionCache::new("main"));
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "history", "session_key": "missing"}))
            .await
            .unwrap();

        assert_eq!(result["total_messages"], 0);
    }

    #[tokio::test]
    async fn test_session_status_not_found_errors() {
        let cache = Arc::new(SessionCache::new("main"));
        let tool = SessionTool::new(cache.as_shared());

        // The old fabricated zeroed status is gone: an unknown session
        // must surface the runtime's real error.
        let err = tool
            .execute(json!({"action": "status", "session_key": "missing"}))
            .await
            .expect_err("status on an unknown session must error, not fabricate");
        assert!(err.to_string().contains("missing"), "{err}");
    }

    /// Helper: build a registry pre-loaded with three sessions spanning
    /// two peers (`user:alice`, `user:bob`) and two agents
    /// (`test-agent`, `other-agent`).
    fn cross_peer_cache() -> Arc<SessionCache> {
        let cache = SessionCache::new("main");

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
            archived: false,
            run_active: false,
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
            archived: false,
            run_active: false,
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
            archived: false,
            run_active: false,
        };

        cache.add_session(
            "alice-1".to_string(),
            alice_main,
            vec![],
            dummy_status("alice-1"),
        );
        cache.add_session(
            "alice-2".to_string(),
            alice_other,
            vec![],
            dummy_status("alice-2"),
        );
        cache.add_session("bob-1".to_string(), bob_main, vec![], dummy_status("bob-1"));
        Arc::new(cache)
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
                last_total_tokens: 0,
                model_context_limit: None,
            },
            peer_type: None,
            peer_id: None,
            label: None,
            parent_session: None,
        }
    }

    #[tokio::test]
    async fn test_session_list_peer_filter_returns_only_that_peer() {
        let tool = SessionTool::new(cross_peer_cache().as_shared());
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
        let tool = SessionTool::new(cross_peer_cache().as_shared());
        let result = tool
            .execute(json!({"action": "list", "peer": "user:nobody"}))
            .await
            .unwrap();

        assert_eq!(result["total"], 0);
        assert!(result["sessions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_session_list_agent_id_filter() {
        let tool = SessionTool::new(cross_peer_cache().as_shared());
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
        let tool = SessionTool::new(cross_peer_cache().as_shared());
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
        let tool = SessionTool::new(cross_peer_cache().as_shared());
        let err = tool
            .execute(json!({"action": "list", "peer": "not-a-valid-peer"}))
            .await
            .expect_err("invalid peer must surface an error");
        assert!(err.to_string().contains("Invalid peer"));
    }

    #[tokio::test]
    async fn test_session_info_surfaces_peer_fields() {
        let tool = SessionTool::new(cross_peer_cache().as_shared());
        let result = tool
            .execute(json!({"action": "list", "peer": "user:alice"}))
            .await
            .unwrap();

        for s in result["sessions"].as_array().unwrap() {
            assert_eq!(s["peer_type"], "user");
            assert_eq!(s["peer_id"], "alice");
        }
    }

    // ====================================================================================
    // Phase 3 Tests: lifecycle actions (search/branch/rename/archive/delete/compact/new/resume)
    // ====================================================================================

    #[tokio::test]
    async fn test_session_search_happy_path_and_missing_query() {
        let cache = SessionCache::new("main");
        let session = SessionInfo {
            session_key: "s1".to_string(),
            session_id: "s1".to_string(),
            kind: "main".to_string(),
            agent_id: Some("main".to_string()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 1,
            is_active: true,
            peer_type: None,
            peer_id: None,
            archived: false,
            run_active: false,
        };
        let history = vec![HistoryMessage {
            role: "user".to_string(),
            content: "deploy the frambulator on friday".to_string(),
            tool_calls: None,
            tool_results: None,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        }];
        cache.add_session("s1".to_string(), session, history, dummy_status("s1"));
        let tool = SessionTool::new(Arc::new(cache).as_shared());

        let result = tool
            .execute(json!({"action": "search", "query": "FRAMBULATOR"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["hits"][0]["session_id"], "s1");
        assert_eq!(result["hits"][0]["role"], "user");
        assert!(result["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("frambulator"));

        let err = tool
            .execute(json!({"action": "search"}))
            .await
            .expect_err("search without query must error");
        assert!(err.to_string().contains("query"), "{err}");
    }

    #[tokio::test]
    async fn test_session_branch_and_missing_session_key() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(
                json!({"action": "branch", "session_key": "test-session", "label": "experiment"}),
            )
            .await
            .unwrap();
        assert_eq!(result["parent_session_id"], "test-session");
        let new_id = result["new_session_id"].as_str().unwrap().to_string();

        // The branch is listable and carries the branch kind + label.
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        let branch = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["session_key"] == new_id)
            .expect("branch listed");
        assert_eq!(branch["kind"], "branch");
        assert_eq!(branch["label"], "experiment");

        let err = tool
            .execute(json!({"action": "branch", "label": "x"}))
            .await
            .expect_err("branch without session_key must error");
        assert!(err.to_string().contains("session_key"), "{err}");
    }

    #[tokio::test]
    async fn test_session_rename_and_missing_title() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        tool.execute(
            json!({"action": "rename", "session_key": "test-session", "title": "Renamed"}),
        )
        .await
        .unwrap();

        let result = tool
            .execute(json!({"action": "status", "session_key": "test-session"}))
            .await
            .unwrap();
        assert_eq!(result["label"], "Renamed");

        let err = tool
            .execute(json!({"action": "rename", "session_key": "test-session"}))
            .await
            .expect_err("rename without title must error");
        assert!(err.to_string().contains("title"), "{err}");
    }

    #[tokio::test]
    async fn test_session_archive_unarchive_and_list_visibility() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        // Archive: hidden from the default list.
        let result = tool
            .execute(json!({"action": "archive", "session_key": "test-session"}))
            .await
            .unwrap();
        assert_eq!(result["archived"], true);
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(list["total"], 0, "archived hidden by default");

        // include_archived: true brings it back, flagged.
        let list = tool
            .execute(json!({"action": "list", "include_archived": true}))
            .await
            .unwrap();
        assert_eq!(list["total"], 1);
        assert_eq!(list["sessions"][0]["archived"], true);

        // Unarchive: visible again.
        let result = tool
            .execute(json!({"action": "unarchive", "session_key": "test-session"}))
            .await
            .unwrap();
        assert_eq!(result["archived"], false);
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(list["total"], 1);

        let err = tool
            .execute(json!({"action": "archive"}))
            .await
            .expect_err("archive without session_key must error");
        assert!(err.to_string().contains("session_key"), "{err}");
    }

    #[tokio::test]
    async fn test_session_delete_happy_and_missing_key() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "delete", "session_key": "test-session"}))
            .await
            .unwrap();
        assert_eq!(result["deleted"].as_array().unwrap().len(), 1);
        assert_eq!(result["deleted"][0], "test-session");

        // Gone from the list.
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(list["total"], 0);

        let err = tool
            .execute(json!({"action": "delete"}))
            .await
            .expect_err("delete without session_key must error");
        assert!(err.to_string().contains("session_key"), "{err}");
    }

    #[tokio::test]
    async fn test_session_compact_and_missing_key() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "compact", "session_key": "test-session"}))
            .await
            .unwrap();
        assert_eq!(result["session_id"], "test-session");
        assert!(result["message"].as_str().unwrap().contains("Compaction"));

        let err = tool
            .execute(json!({"action": "compact"}))
            .await
            .expect_err("compact without session_key must error");
        assert!(err.to_string().contains("session_key"), "{err}");
    }

    #[tokio::test]
    async fn test_session_new_chapter() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "new", "title": "morning"}))
            .await
            .unwrap();
        assert_eq!(result["live_session_id"], "main");
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("next incoming message"));
    }

    #[tokio::test]
    async fn test_session_resume_and_missing_target() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "resume", "target": "test-session"}))
            .await
            .unwrap();
        assert_eq!(result["live_session_id"], "main");
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("next incoming message"));

        let err = tool
            .execute(json!({"action": "resume"}))
            .await
            .expect_err("resume without target must error");
        assert!(err.to_string().contains("target"), "{err}");

        // Unknown target surfaces the runtime error.
        let err = tool
            .execute(json!({"action": "resume", "target": "nope"}))
            .await
            .expect_err("resume of an unknown target must error");
        assert!(err.to_string().contains("nope"), "{err}");
    }
}
