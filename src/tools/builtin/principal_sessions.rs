//! Principal-wide session introspection tool
//!
//! Provides `principal_sessions` for the supervisor agent to inspect sessions
//! across the whole Principal namespace (not just the supervisor's own session).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::principal::memory::{PrincipalMemory, SessionArtifact};
use crate::tools::core::traits::Tool;

/// Actions supported by `principal_sessions`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionsAction {
    List,
    Status,
    History,
}

/// Lightweight serializable view of a `SessionArtifact`.
#[derive(Debug, Clone, serde::Serialize)]
struct SessionView {
    session_id: String,
    peer: String,
    title: Option<String>,
    updated_at: String,
    summary: Option<String>,
}

impl From<&SessionArtifact> for SessionView {
    fn from(a: &SessionArtifact) -> Self {
        Self {
            session_id: a.session_id.clone(),
            peer: a.peer.to_string(),
            title: a.title.clone(),
            updated_at: a.updated_at.to_rfc3339(),
            summary: a.summary.clone(),
        }
    }
}

/// Tool for inspecting Principal-wide sessions.
pub struct PrincipalSessionsTool {
    memory: Arc<dyn PrincipalMemory>,
}

impl PrincipalSessionsTool {
    /// Create a new tool backed by the given Principal memory.
    #[must_use]
    pub fn new(memory: Arc<dyn PrincipalMemory>) -> Self {
        Self { memory }
    }

    async fn list_sessions(
        &self,
        peer: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<serde_json::Value> {
        let mut sessions = self.memory.list_sessions().await?;
        if let Some(peer_key) = peer {
            sessions.retain(|s| s.peer.to_string() == peer_key);
        }
        sessions.truncate(limit);

        let views: Vec<SessionView> = sessions.iter().map(SessionView::from).collect();
        Ok(json!({ "total": views.len(), "sessions": views }))
    }

    async fn status(
        &self,
        peer: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let artifact = if let Some(peer_key) = peer {
            let peer_subject: crate::auth::Subject = peer_key
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid peer '{peer_key}': {e}"))?;
            self.memory
                .find_latest_session_for_peer(&peer_subject)
                .await?
        } else if let Some(id) = session_id {
            self.memory
                .list_sessions()
                .await?
                .into_iter()
                .find(|s| s.session_id == id)
        } else {
            None
        };

        match artifact {
            Some(a) => Ok(json!({
                "found": true,
                "session": SessionView::from(&a),
            })),
            None => Ok(json!({
                "found": false,
                "session": serde_json::Value::Null,
            })),
        }
    }

    async fn history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> anyhow::Result<serde_json::Value> {
        let path = self.memory.sessions_dir().join(format!("{session_id}.jsonl"));
        if !path.exists() {
            return Ok(json!({ "session_id": session_id, "total_messages": 0, "messages": [] }));
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let mut messages: Vec<serde_json::Value> = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                messages.push(value);
            }
        }
        messages.truncate(limit);

        Ok(json!({
            "session_id": session_id,
            "total_messages": messages.len(),
            "messages": messages,
        }))
    }
}

#[async_trait]
impl Tool for PrincipalSessionsTool {
    fn name(&self) -> &'static str {
        "principal_sessions"
    }

    fn description(&self) -> String {
        r"Inspect Principal-wide sessions: list sessions, check status, or read session history.

Parameters:
- action: 'list', 'status', or 'history' (required)
- peer: Optional filter by peer string (e.g., 'user:alice')
- session_id: Required for 'history'; optional for 'status'
- limit: Maximum results (default: 50)

Returns structured session metadata or raw JSONL messages."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "status", "history"],
                    "description": "What to do"
                },
                "peer": {
                    "type": "string",
                    "description": "Optional peer filter, e.g. 'user:alice'"
                },
                "session_id": {
                    "type": "string",
                    "description": "Required for 'history'; optional for 'status'"
                },
                "limit": {
                    "type": "integer",
                    "default": 50,
                    "description": "Maximum number of results"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let action: SessionsAction = serde_json::from_value(
            params
                .get("action")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?,
        )
        .map_err(|e| anyhow::anyhow!("Invalid action: {e}"))?;

        let peer = params.get("peer").and_then(|v| v.as_str());
        let session_id = params.get("session_id").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

        match action {
            SessionsAction::List => self.list_sessions(peer, limit).await,
            SessionsAction::Status => self.status(peer, session_id).await,
            SessionsAction::History => {
                let id = session_id.ok_or_else(|| {
                    anyhow::anyhow!("'history' action requires a 'session_id' parameter")
                })?;
                self.history(id, limit).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Subject;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::Mutex;

    struct MockMemory {
        sessions: Mutex<Vec<SessionArtifact>>,
    }

    #[async_trait]
    impl PrincipalMemory for MockMemory {
        async fn record_session(&self, artifact: SessionArtifact) -> Result<(), crate::principal::memory::MemoryError> {
            self.sessions.lock().unwrap().push(artifact);
            Ok(())
        }

        async fn find_latest_session_for_peer(
            &self,
            peer: &Subject,
        ) -> Result<Option<SessionArtifact>, crate::principal::memory::MemoryError> {
            let peer_key = peer.to_string();
            Ok(self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.peer.to_string() == peer_key)
                .cloned()
                .max_by(|a, b| a.updated_at.cmp(&b.updated_at)))
        }

        async fn list_sessions(&self) -> Result<Vec<SessionArtifact>, crate::principal::memory::MemoryError> {
            Ok(self.sessions.lock().unwrap().clone())
        }

        async fn store(&self, _artifact: crate::principal::memory::Artifact) -> Result<(), crate::principal::memory::MemoryError> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _k: usize,
        ) -> Result<Vec<crate::principal::memory::Artifact>, crate::principal::memory::MemoryError> {
            Ok(Vec::new())
        }

        async fn compact(
            &self,
        ) -> Result<crate::principal::memory::CompactSummary, crate::principal::memory::MemoryError> {
            Ok(crate::principal::memory::CompactSummary::default())
        }

        fn sessions_dir(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }

        fn supervisor_session_path(&self) -> std::path::PathBuf {
            std::env::temp_dir().join("supervisor.jsonl")
        }
    }

    fn make_artifact(session_id: &str, peer: &Subject) -> SessionArtifact {
        SessionArtifact {
            session_id: session_id.to_string(),
            peer: peer.clone(),
            title: Some("test".to_string()),
            updated_at: Utc::now(),
            summary: Some("summary".to_string()),
        }
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let peer = Subject::User("alice".to_string());
        let memory = Arc::new(MockMemory {
            sessions: Mutex::new(vec![make_artifact("s1", &peer)]),
        });
        let tool = PrincipalSessionsTool::new(memory);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["sessions"][0]["session_id"], "s1");
    }

    #[tokio::test]
    async fn test_status_by_peer() {
        let peer = Subject::User("alice".to_string());
        let memory = Arc::new(MockMemory {
            sessions: Mutex::new(vec![make_artifact("s1", &peer)]),
        });
        let tool = PrincipalSessionsTool::new(memory);

        let result = tool
            .execute(json!({"action": "status", "peer": "user:alice"}))
            .await
            .unwrap();
        assert!(result["found"].as_bool().unwrap());
        assert_eq!(result["session"]["session_id"], "s1");
    }

    #[tokio::test]
    async fn test_status_not_found() {
        let memory = Arc::new(MockMemory {
            sessions: Mutex::new(Vec::new()),
        });
        let tool = PrincipalSessionsTool::new(memory);

        let result = tool
            .execute(json!({"action": "status", "peer": "user:bob"}))
            .await
            .unwrap();
        assert!(!result["found"].as_bool().unwrap());
    }
}
