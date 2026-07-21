//! `peko_tools_builtin::session` — Session introspection tool surface +
//! `SessionRuntime` port.
//!
//! Phase 10d extracts the unified `SessionTool` plus the six session
//! DTOs (`SessionInfo`, `HistoryMessage`, `ToolCallInfo`, `ToolResultInfo`,
//! `UsageStats`, `SessionStatusResult`) out of root. Per the Phase 10
//! plan rule ("Built-ins must not import daemon state"), the tools here
//! do NOT call `crate::session::SessionManager` directly. They speak to
//! a runtime port trait ([`SessionRuntime`]) that the daemon/agent
//! side implements.
//!
//! ## DTOs
//!
//! `SessionInfo`, `HistoryMessage`, `ToolCallInfo`, `ToolResultInfo`,
//! `UsageStats`, and `SessionStatusResult` are serialization-friendly
//! types shared between the tool side and the daemon/agent side.
//! peko-tools-builtin is the canonical home; the root re-exports these
//! from peko-tools-builtin via `pub use peko_tools_builtin::session::{...};`
//! — single source of truth going forward. A compile-time
//! JSON-roundtrip test pins the two sides' shapes together.
//!
//! ## Port
//!
//! [`SessionRuntime`] is the four-method surface the `SessionTool`
//! needs: list_sessions / get_history / get_status / current_session_key.
//! Production wiring uses the `SessionManagerRuntime` adapter in
//! `src/session/session_runtime_impl.rs`; tests construct a
//! [`SessionCache`] (in this module, an in-memory implementation).
//!
//! ## What stays in root
//!
//! `SessionIntrospector` (the production `SessionManagerRuntime` adapter)
//! and `crate::session::SessionManager` stay in root — the manager
//! depends on root-internal modules (`crate::session::lock::*`,
//! `crate::common::registry::SimpleRegistry`, etc.) that can't move.

pub mod cache;
pub mod tool;

pub use cache::SessionCache;
pub use tool::SessionTool;

// ─── DTOs (canonical home; root re-exports these) ─────────────────

use serde::{Deserialize, Serialize};

/// Session info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_key: String,
    pub session_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool result info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultInfo {
    pub tool_call_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Token usage stats for a session.
///
/// The two cumulative fields reflect what the LLM has reported
/// across the session's lifetime. The two single-turn fields
/// describe the most recent turn — `last_total_tokens` is what
/// the model told us on its last reply, while `model_context_limit`
/// is the model's maximum context window (or `null` when the
/// session has not been opened against a known model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStats {
    /// Cumulative input tokens across the session (session lifetime).
    pub prompt_tokens: u64,
    /// Cumulative output tokens across the session (session lifetime).
    pub completion_tokens: u64,
    /// `total_tokens` reported by the provider on the most recent
    /// assistant turn. NOT the model's context window size — see
    /// `model_context_limit` for that.
    pub last_total_tokens: u64,
    /// The model's maximum context window size, in tokens. `None`
    /// for legacy sessions and sessions opened without a provider/
    /// model reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_context_limit: Option<usize>,
}

/// Session status result
#[derive(Debug, Clone, Serialize, Deserialize)]
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

// ─── SessionRuntime port trait ────────────────────────────────────

/// Runtime port the `SessionTool` uses to talk to session storage.
///
/// The production wiring implements this with `SessionManagerRuntime`
/// (root's `src/session/session_runtime_impl.rs`) which wraps the
/// shared `Arc<RwLock<SessionManager>>`. Tests and placeholder paths
/// (CLI/test harnesses that don't have a real `SessionManager`) use
/// [`SessionCache`], an in-memory implementation provided in this
/// crate.
#[async_trait::async_trait]
pub trait SessionRuntime: Send + Sync {
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
        peer: Option<&peko_subject::Subject>,
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

/// Type alias for the shared runtime handle threaded through every
/// `SessionTool` constructor.
pub type SharedSessionRuntime = std::sync::Arc<dyn SessionRuntime>;

// ─── JSON-roundtrip pin ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Pin the JSON wire shape against the root-side mirror.
    use super::*;

    #[test]
    fn session_info_roundtrip() {
        let info = SessionInfo {
            session_key: "alice-1".into(),
            session_id: "alice-1".into(),
            kind: "main".into(),
            agent_id: Some("test-agent".into()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".into(),
            last_activity: "2024-01-01T01:00:00Z".into(),
            message_count: 5,
            is_active: true,
            peer_type: Some("user".into()),
            peer_id: Some("alice".into()),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["session_key"], "alice-1");
        assert_eq!(json["peer_type"], "user");
        assert_eq!(json["agent_id"], "test-agent");
        let back: SessionInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, info.session_id);
        assert_eq!(back.peer_type, info.peer_type);
    }

    #[test]
    fn history_message_roundtrip() {
        let msg = HistoryMessage {
            role: "user".into(),
            content: "hello".into(),
            tool_calls: Some(vec![ToolCallInfo {
                id: "tc1".into(),
                name: "Read".into(),
                arguments: serde_json::json!({"path": "/tmp/x"}),
            }]),
            tool_results: None,
            timestamp: "2024-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["tool_calls"][0]["name"], "Read");
        let back: HistoryMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(back.tool_calls.unwrap()[0].name, "Read");
    }

    #[test]
    fn session_status_roundtrip() {
        let status = SessionStatusResult {
            session_id: "s1".into(),
            agent_name: "agent1".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            last_activity: "2024-01-01T01:00:00Z".into(),
            timestamp_utc: String::new(),
            timestamp: String::new(),
            message_count: 10,
            usage: UsageStats {
                prompt_tokens: 100,
                completion_tokens: 50,
                last_total_tokens: 1500,
                model_context_limit: Some(128_000),
            },
            peer_type: None,
            peer_id: None,
            label: None,
            parent_session: None,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["session_id"], "s1");
        assert_eq!(json["usage"]["model_context_limit"], 128_000);
        let back: SessionStatusResult = serde_json::from_value(json).unwrap();
        assert_eq!(back.usage.model_context_limit, Some(128_000));
    }

    #[test]
    fn serialisation_skips_none_optional_fields() {
        let info = SessionInfo {
            session_key: "k".into(),
            session_id: "k".into(),
            kind: "main".into(),
            agent_id: None,
            label: None,
            created_at: String::new(),
            last_activity: String::new(),
            message_count: 0,
            is_active: true,
            peer_type: None,
            peer_id: None,
        };
        let json = serde_json::to_value(&info).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("agent_id"));
        assert!(!obj.contains_key("label"));
        assert!(!obj.contains_key("peer_type"));
        assert!(!obj.contains_key("peer_id"));
    }
}
