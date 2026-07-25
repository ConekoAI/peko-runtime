//! Unified `session` tool — single introspection entry point that
//! dispatches by `action` (`status` / `list` / `history`).
//!
//! Replaces the legacy `session_status`, `sessions_list`, `sessions_history`
//! tools (Issue 013). Speaks to the [`SessionRuntime`] port.

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
}

/// Actions supported by the `session` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionAction {
    Status,
    List,
    History,
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
                let mut status = match self.get_status_action(session_key).await {
                    Ok(s) => s,
                    Err(_) => {
                        let session_id = session_key
                            .unwrap_or(&self.runtime.current_session_key())
                            .to_string();
                        SessionStatusResult {
                            session_id,
                            agent_name: "unknown".to_string(),
                            created_at: chrono::Utc::now().to_rfc3339(),
                            last_activity: chrono::Utc::now().to_rfc3339(),
                            timestamp_utc: String::new(),
                            timestamp: String::new(),
                            message_count: 0,
                            usage: super::UsageStats {
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
                        s.parse::<peko_subject::Subject>()
                            .map_err(|e| anyhow::anyhow!("Invalid peer '{s}': {e}"))?,
                    ),
                    None => None,
                };
                let agent_id = params.get("agent_id").and_then(|v| v.as_str());
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let active_minutes = params.get("active_minutes").and_then(|v| v.as_i64());

                let kinds_ref = kinds.as_deref();
                let sessions = self
                    .runtime
                    .list_sessions(kinds_ref, peer.as_ref(), agent_id, limit, active_minutes)
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        HistoryMessage, SessionCache, SessionInfo, SessionStatusResult, UsageStats,
    };
    use serde_json::json;
    use std::sync::Arc;

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
        let mut cache = SessionCache::new("current-session");

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
    async fn test_session_status_not_found_returns_minimal() {
        let cache = Arc::new(SessionCache::new("main"));
        let tool = SessionTool::new(cache.as_shared());

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
}
