//! `session` built-in tool surface + the `SessionRuntime` port.
//!
//! The unified `SessionTool` plus the session DTOs (`SessionInfo`,
//! `HistoryMessage`, `ToolCallInfo`, `ToolResultInfo`, `UsageStats`,
//! `SessionStatusResult`, and the outcome DTOs) live here. The tool
//! does NOT call `crate::session::SessionManager` directly — it speaks
//! to the [`SessionRuntime`] port trait that the daemon/agent side
//! implements.
//!
//! ## DTOs
//!
//! The DTOs are serialization-friendly types shared between the tool
//! side and the daemon/agent side; this module is the single source of
//! truth. A compile-time JSON-roundtrip test pins the wire shape.
//!
//! ## Port
//!
//! [`SessionRuntime`] is the full surface the `SessionTool` needs:
//! reads (`list_sessions` / `get_history` / `get_status` /
//! `search_sessions` / `current_session_key`) and storage mutations
//! (`branch_session` / `rename_session` / `set_archived` /
//! `delete_session` / `move_session`). `request_compaction` rides
//! the same trait but is engine-facing only — the model-facing
//! `compact` affordance lives on the Agent tool. Production wiring
//! uses the `SessionManagerRuntime` adapter in
//! `src/session/session_runtime_impl.rs`; tests construct a
//! [`SessionCache`] (in this module, an in-memory implementation).
//!
//! Note on `SessionRuntime` method names vs. tool-action names:
//! the runtime methods are storage verbs (`branch_session`,
//! `delete_session`, `rename_session`, `move_session`,
//! `set_archived`); the model-facing tool actions are bash-aligned
//! verbs (`copy`, `remove`, in-place `move`, no archive/unarchive).
//! The tool layer is the only place that bridges between the two —
//! see `tool.rs::SessionTool::execute` for the dispatch. `set_archived`
//! stays on the trait for legacy record compatibility (records that
//! already carry `archived: true` are still readable via
//! `list_sessions` with `include_archived: true`), but no model-facing
//! action writes it. Sprint 7 Commit F (2026-08-21).

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: String,
    pub last_activity: String,
    pub message_count: usize,
    /// Subject type ("user", "principal", or "public") — present when
    /// the underlying `SessionMetadata` was written with peer info.
    /// Branched sessions may have `None` here (see `branch_session_by_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_type: Option<String>,
    /// Subject ID (e.g. `"alice"` for `user:alice`). `None` when no
    /// peer is recorded for the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// Archived sessions are hidden from `list` unless
    /// `include_archived: true` and refuse resume/compact.
    #[serde(default)]
    pub archived: bool,
    /// Whether a run is currently in flight in this session (the
    /// session — or one of its descendants — cannot be deleted,
    /// archived, or re-attached while true).
    #[serde(default)]
    pub run_active: bool,
    /// Per-parent-unique path segment (see `peko_session::path`).
    /// `None` for sessions that were never given a slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Absolute display path (`/a/b` — slug segments, slugless
    /// ancestors skipped, slugless target falling back to its raw id
    /// as the last segment). Computed view only; ids stay canonical.
    #[serde(default)]
    pub path: String,
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

/// One match from a transcript search (`session` tool `search` action).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSearchHit {
    pub session_id: String,
    /// Role of the matching message ("user" or "assistant").
    pub role: String,
    /// RFC 3339 timestamp of the matching message.
    pub timestamp: String,
    /// ~160 chars of message text centered on the match.
    pub snippet: String,
}

/// Result of the `branch` action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchOutcome {
    pub new_session_id: String,
    pub parent_session_id: String,
}

/// Result of the `delete` action: every session id actually removed
/// (the target plus, with `recursive: true`, its descendants).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteOutcome {
    pub deleted: Vec<String>,
}

/// Result of the `compact` action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactRequestOutcome {
    pub session_id: String,
    /// When the request fires (next iteration for the current
    /// session, next run for others).
    pub message: String,
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
    /// - `peer`: filter to a single peer (`user:alice`, `principal:<did>`, or `public`).
    ///   When `None`, results span all peers (the cross-peer view).
    /// - `agent_id`: filter to a single agent name.
    /// - `limit`: cap on results returned.
    /// - `active_minutes`: only sessions updated within the last N minutes.
    /// - `include_archived`: include archived sessions (hidden by default).
    ///
    /// To find subagent sessions, the caller filters on
    /// `parent_session_id is not None` in its own reasoning.
    /// There is no closed-enum `kinds` filter — those were dropped
    /// because the description-vs-engine drift (r5/r6 field tests)
    /// could not be reconciled.
    async fn list_sessions(
        &self,
        peer: Option<&peko_subject::Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
        include_archived: bool,
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

    /// Case-insensitive substring search over session transcripts.
    ///
    /// - `query`: the needle.
    /// - `peer`: restrict to one peer's sessions (`None` = all peers).
    /// - `limit`: cap on hits returned.
    async fn search_sessions(
        &self,
        query: &str,
        peer: Option<&peko_subject::Subject>,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionSearchHit>>;

    /// Branch a session (copy it under a new id, stored not running).
    /// Returns the new session's id alongside the parent's.
    async fn branch_session(
        &self,
        session_key: &str,
        label: Option<String>,
    ) -> anyhow::Result<BranchOutcome>;

    /// Rename (retitle) a session and/or set its slug (the
    /// per-parent-unique path segment used for `/a/b` addressing).
    /// At least one of `title` / `slug` is supplied (enforced by the
    /// tool layer). A `Some` slug is validated and must be unique
    /// among the session's siblings; a conflict is a structured error
    /// naming the conflicting session id.
    async fn rename_session(
        &self,
        session_key: &str,
        title: Option<String>,
        slug: Option<String>,
    ) -> anyhow::Result<()>;

    /// Move (reparent) a session — with its subtree — under a new
    /// parent. Refused when the move would create a cycle, when the
    /// target is the live trunk session (`root:self`), or when the
    /// target or any descendant has an active run.
    async fn move_session(&self, session_key: &str, new_parent: String) -> anyhow::Result<()>;

    /// Set or clear the archived flag on a session. Archived sessions
    /// are hidden from `list` (unless `include_archived: true`) and
    /// refuse resume/compact.
    async fn set_archived(&self, session_key: &str, archived: bool) -> anyhow::Result<()>;

    /// Delete a session. When the session has descendants (via
    /// `parent_session_id`), the delete refuses unless `recursive` —
    /// which deletes the whole subtree, children first.
    async fn delete_session(
        &self,
        session_key: &str,
        recursive: bool,
    ) -> anyhow::Result<DeleteOutcome>;

    /// Schedule compaction for a session (next iteration for the
    /// current session, next run for others).
    ///
    /// Not exposed on the `session` tool: the orchestrator fires
    /// compaction automatically from the persisted token counter (WS1),
    /// and the model-facing `compact` affordance lives on the Agent
    /// tool — its real caller arrives via the `SubagentExecutor` path
    /// (next phase), sharing the ownership guard helpers in
    /// `crate::session::ownership`. The trait method stays so that
    /// caller can route through the same port.
    #[allow(dead_code)]
    async fn request_compaction(&self, session_key: &str) -> anyhow::Result<CompactRequestOutcome>;
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
            agent_id: Some("test-agent".into()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".into(),
            last_activity: "2024-01-01T01:00:00Z".into(),
            message_count: 5,
            peer_type: Some("user".into()),
            peer_id: Some("alice".into()),
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["session_key"], "alice-1");
        assert_eq!(json["peer_type"], "user");
        assert_eq!(json["agent_id"], "test-agent");
        assert_eq!(json["archived"], false);
        assert_eq!(json["run_active"], false);
        // The old `kind` field is gone — the model now uses
        // `parent_session_id` (not in SessionInfo) to derive role.
        let back: SessionInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, info.session_id);
        assert_eq!(back.peer_type, info.peer_type);
        assert!(!back.archived);
        assert!(!back.run_active);
    }

    #[test]
    fn session_info_legacy_json_without_new_fields_defaults_false() {
        // A `SessionInfo` serialized before `archived` / `run_active`
        // existed must still deserialize, with both flags = false.
        // The pre-refactor wire also had `kind` and `is_active` fields;
        // absent in new payloads but tolerated on read (ignored via
        // the default serde unknown-field behavior).
        let legacy = serde_json::json!({
            "session_key": "k",
            "session_id": "k",
            "kind": "main",
            "created_at": "2024-01-01T00:00:00Z",
            "last_activity": "2024-01-01T01:00:00Z",
            "message_count": 3,
            "is_active": true
        });
        let back: SessionInfo = serde_json::from_value(legacy).unwrap();
        assert!(!back.archived);
        assert!(!back.run_active);
        // slug/path were added later still; legacy payloads default them.
        assert_eq!(back.slug, None);
        assert_eq!(back.path, "");
    }

    #[test]
    fn session_info_slug_and_path_roundtrip() {
        let info = SessionInfo {
            session_key: "s1".into(),
            session_id: "s1".into(),
            agent_id: None,
            label: None,
            created_at: "2024-01-01T00:00:00Z".into(),
            last_activity: "2024-01-01T01:00:00Z".into(),
            message_count: 0,
            peer_type: None,
            peer_id: None,
            archived: false,
            run_active: false,
            slug: Some("task-b".into()),
            path: "/memory/task-b".into(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["slug"], "task-b");
        assert_eq!(json["path"], "/memory/task-b");
        let back: SessionInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back.slug.as_deref(), Some("task-b"));
        assert_eq!(back.path, "/memory/task-b");
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
            agent_id: None,
            label: None,
            created_at: String::new(),
            last_activity: String::new(),
            message_count: 0,
            peer_type: None,
            peer_id: None,
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
        };
        let json = serde_json::to_value(&info).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("agent_id"));
        assert!(!obj.contains_key("label"));
        assert!(!obj.contains_key("peer_type"));
        assert!(!obj.contains_key("peer_id"));
    }

    #[test]
    fn session_search_hit_roundtrip() {
        let hit = SessionSearchHit {
            session_id: "s1".into(),
            role: "user".into(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            snippet: "…the needle is here…".into(),
        };
        let json = serde_json::to_value(&hit).unwrap();
        assert_eq!(json["session_id"], "s1");
        assert_eq!(json["role"], "user");
        let back: SessionSearchHit = serde_json::from_value(json).unwrap();
        assert_eq!(back.snippet, hit.snippet);
    }

    #[test]
    fn outcome_dtos_roundtrip() {
        let branch = BranchOutcome {
            new_session_id: "b1".into(),
            parent_session_id: "p1".into(),
        };
        let json = serde_json::to_value(&branch).unwrap();
        assert_eq!(json["new_session_id"], "b1");
        let back: BranchOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back.parent_session_id, "p1");

        let delete = DeleteOutcome {
            deleted: vec!["a".into(), "b".into()],
        };
        let json = serde_json::to_value(&delete).unwrap();
        assert_eq!(json["deleted"].as_array().unwrap().len(), 2);
        let back: DeleteOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back.deleted, vec!["a".to_string(), "b".to_string()]);

        let compact = CompactRequestOutcome {
            session_id: "s1".into(),
            message: "fires next run".into(),
        };
        let json = serde_json::to_value(&compact).unwrap();
        assert_eq!(json["session_id"], "s1");
        let back: CompactRequestOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back.message, "fires next run");
    }
}
