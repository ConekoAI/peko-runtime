//! Unified `session` tool — single storage entry point that dispatches
//! by `action` over 7 operations (`status` / `list` / `history` /
//! `find` / `copy` / `move` / `remove`).
//!
//! Replaces the legacy `session_status`, `sessions_list`,
//! `sessions_history` tools (Issue 013). The verbs match the bash
//! family where they overlap: `find` ≈ grep across transcripts, `copy`
//! = `cp` (duplicate a session to a destination slug path), `move` =
//! `mv` (reparent or rename in place — both via `target: <parent>/<slug>`,
//! with `title` as an optional display-only rename), `remove` = `rm`
//! (delete the session, optionally recursive).
//!
//! Sprint 7 Commit F (2026-08-21): trimmed from 10 → 7 actions.
//! `archive` / `unarchive` removed (sessions are monotonically visible
//! until `remove`; the `include_archived` filter stays for legacy
//! records). `rename` removed — its semantics folded into `move`
//! (slug without reparent = rename in place via `target`). `branch`
//! renamed to `copy`, `search` to `find`, `delete` to `remove`.
//!
//! Sprint 7 Commit G (2026-08-22): the LLM-facing target parameter
//! is renamed from `session_key` to `path`, matching the Agent tool
//! (which took the same rename in sprint 7 Commits 1-4). Both tools
//! now describe their target the same way: a slug path (`/a/b/c`),
//! raw session ids refused at the runtime layer via
//! `peko_session::path::resolve_reference`. The session tool's
//! runtime already routed through `resolve_reference`; the
//! tool-layer `validate_path` is the new early-fail point that
//! mirrors `AgentArgs::validate_action_args`.
//!
//! The caller-relative branch (`b/c` without a leading `/`) is gone
//! in sprint 7: it was promised in the tool surface but never wired
//! into `resolve_reference`. Only `/`-prefixed slug paths and the
//! caller's own id pass the runtime layer.
//!
//! Sprint 7 Commit H (2026-08-22): `copy` and `move` now share a
//! single `target` field shaped like bash `cp src dst` / `mv src dst`
//! — `<parent>/<new_slug>`, last segment = new slug, before-last =
//! new parent. Replaces the prior sibling-only `copy` shape and the
//! prior `new_parent` + `title` + `slug` trio on `move`. `title`
//! stays as a display-only rename knob on `move` (independent of
//! `target`).
//!
//! `new` / `resume` / `compact` stay off this tool — they drive the
//! LLM and live on the Agent tool instead. Speaks to the
//! [`SessionRuntime`] port.

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
            "path": session_key,
            "total_messages": messages.len(),
            "messages": messages,
        })
    }

    /// Extract a required `path` param with an actionable error, and
    /// validate the slug shape at the tool boundary (mirrors
    /// `AgentArgs::validate_action_args`). Raw session ids and
    /// malformed paths are rejected here so the model gets a clean
    /// structured error before the runtime touches state.
    fn require_path<'a>(
        params: &'a serde_json::Value,
        action: &str,
    ) -> anyhow::Result<&'a str> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "action \"{action}\" requires 'path' — an absolute slug path \
                     ('/a/b/c') naming the target session (use the `path` field from \
                     `session list`)"
                )
            })?;
        if let Err(e) = peko_session::path::validate_path(path) {
            return Err(anyhow::anyhow!(
                "action \"{action}\" 'path' is not a valid slug path: {e}"
            ));
        }
        Ok(path)
    }

    /// Extract and parse the bash-style `target` param for `copy` and
    /// `move`. The target is a slug path: the last segment is the new
    /// slug (under the parent = everything before), mirroring bash
    /// `cp src dst` / `mv src dst`. Returns `(parent_str, slug)` —
    /// `parent_str` may be empty when the target is a single-segment
    /// slug whose new parent is the caller's tree root; the caller
    /// passes `/` to the runtime in that case.
    ///
    /// `target` is the unified destination field introduced in Sprint
    /// 7 Commit H (2026-08-22); it replaces the prior
    /// `new_parent`+`title`+`slug` combination on `move` and the
    /// prior sibling-only placement on copy.
    fn parse_target<'a>(
        params: &'a serde_json::Value,
        action: &str,
    ) -> anyhow::Result<(&'a str, &'a str)> {
        let target = params
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "action \"{action}\" requires 'target' — an absolute slug path \
                     naming the destination ('/parent/new_slug'); the last segment is \
                     the new slug, the rest is the destination parent. Mirrors bash \
                     `cp src dst` / `mv src dst`."
                )
            })?;
        peko_session::path::validate_path(target).map_err(|e| {
            anyhow::anyhow!("action \"{action}\" 'target' is not a valid slug path: {e}")
        })?;
        let (parent_str, slug) = target.rsplit_once('/').ok_or_else(|| {
            anyhow::anyhow!(
                "action \"{action}\" 'target' must include a parent path and a slug segment \
                 (got '{target}')"
            )
        })?;
        if slug.is_empty() {
            return Err(anyhow::anyhow!(
                "action \"{action}\" 'target' slug segment is empty (got '{target}')"
            ));
        }
        Ok((parent_str, slug))
    }
}

/// Actions supported by the `session` tool.
///
/// Sprint 7 Commit F (2026-08-21): 7 actions, bash-aligned.
/// `new` / `resume` / `compact` stay off this tool — they drive the
/// LLM and live on the Agent tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionAction {
    Status,
    List,
    History,
    Find,
    Copy,
    Move,
    Remove,
}

#[async_trait]
impl Tool for SessionTool {
    fn name(&self) -> &'static str {
        "session"
    }

    fn description(&self) -> String {
        r"Single tool with **7 operations** for inspecting and managing your persisted sessions (pure storage reads/writes — no LLM involvement). The `action` parameter is REQUIRED and MUST be one of:

  status | list | history | find | copy | move | remove

Per-action semantics (the action you choose determines which other params apply):
- status: one session's metadata + token usage (path optional, defaults to current)
- list: query sessions (filters: peer, agent_id, active_minutes; archived hidden unless include_archived:true)
- history: messages of a session (path optional, defaults to current; include_tools)
- find: case-insensitive text search across session transcripts (query required; optional peer filter)
- copy: duplicate a session to a destination path (path + target required; optional label). `target` is the full destination slug path — last segment = new slug, before-last = new parent (mirrors bash `cp src dst`). The copy is a fresh session JSON file with its own UUID; the source is unchanged. The copy is NOT running; attach a run to it via the Agent tool's resume action.
- move: reparent a session to a destination path (path + target required; optional title). `target` is the full destination slug path — last segment = the new slug at the new parent (mirrors bash `mv src dst`). To rename in place, set `target` to `<current_parent>/<new_slug>`. Subtree moves with the session. `title` (optional) is the new display label.
- remove: delete a session (path required; recursive:true also deletes its descendants, children first)

The `path` parameter is an absolute slug path (`/a/b/c`, anchored at the root of YOUR session tree — each segment is a slug). Use the `path` field returned by `list`. Raw session ids and caller-relative slugs are REFUSED at the runtime layer via `resolve_reference` (match the Agent tool's behavior). Required: `copy` / `move` / `remove`. Optional: `status` / `history` (defaults to current session).

The `target` parameter (used by `copy` / `move`) is the destination slug path. Shape: `<parent>/<new_slug>` where `<parent>` is an absolute slug path (`/a/b/c`) and `<new_slug>` is the per-parent-unique segment (1-64 chars, no `/`, no leading/trailing whitespace). Same addressing as `path`; same refusal of raw session ids and caller-relative slugs. Mirrors bash `cp src dst` / `mv src dst`. Required: `copy` / `move`.

Refusals: the principal's trunk session (`root:self`) is continuous and managed by the engine — remove/move on it are refused (moving UNDER the trunk is allowed). You cannot remove or move the session you are currently running in. Sessions with an active run refuse remove/move. A move whose destination is the session itself or one of its descendants is refused (would create a cycle). A caller in a spawned session manages only its own subtree — both the moved session and the destination must be inside it. Sessions are monotonically visible until `remove` (Sprint 7 Commit F: archive/unarchive retired; if you want it gone, remove it).

To RUN work in a session, use the Agent tool instead — its three actions (new / resume / compact) drive the LLM. Session ids are stable: the engine pages oversized transcripts and compacts full context windows automatically. To find subagent sessions, look for entries with `parent_session_id` set (visible on status)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "history", "find", "copy", "move", "remove"],
                    "description": "What to do: status/list/history read; find searches text; copy/move/remove manage a session's storage. To run work in a session, use the Agent tool (new/resume/compact)."
                },
                "path": {
                    "type": "string",
                    "description": "Source session: an absolute slug path ('/a/b/c', anchored at the root of your session tree). Use the `path` field returned by `list`. Raw session ids and caller-relative slugs are REFUSED at the runtime layer. Required: `copy` / `move` / `remove`. Optional: `status` / `history` (defaults to current session). Match the Agent tool's `path` parameter."
                },
                "target": {
                    "type": "string",
                    "description": "Required for `copy` / `move`: destination slug path `<parent>/<new_slug>`. `<parent>` is an absolute slug path ('/a/b/c'); `<new_slug>` is the per-parent-unique segment (1-64 chars, no '/', no leading/trailing whitespace). For `copy`: where to place the new copy. For `move`: the new address — to rename in place, set `target` to `<current_parent>/<new_slug>`. Mirrors bash `cp src dst` / `mv src dst`. Raw session ids and caller-relative slugs are refused at the runtime layer."
                },
                "query": {
                    "type": "string",
                    "description": "Required for 'find': case-insensitive substring to find in transcripts"
                },
                "title": {
                    "type": "string",
                    "description": "Optional for 'move': new display title (free-form label; does not affect addressing)."
                },
                "label": {
                    "type": "string",
                    "description": "Optional for 'copy': label/title for the new copy"
                },
                "recursive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional for 'remove': also delete the session's descendants (children first)"
                },
                "include_archived": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional for 'list': include archived sessions (hidden by default). Sprint 7 Commit F retired archive/unarchive actions; this filter remains for legacy records that already carry the flag."
                },
                "peer": {
                    "type": "string",
                    "description": "Optional filter for 'list' and 'find': cross-peer lookup, e.g. 'user:alice' or 'public'. When omitted, results span all peers."
                },
                "agent_id": {
                    "type": "string",
                    "description": "Optional filter for 'list': single agent name"
                },
                "limit": {
                    "type": "integer",
                    "default": 100,
                    "description": "Max results for 'list', 'history', or 'find'"
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
                let path = params.get("path").and_then(|v| v.as_str());
                let timezone = params.get("timezone").and_then(|v| v.as_str());

                // A missing/unknown session is a real error — never
                // fabricate a zeroed status for it.
                let mut status = self.get_status_action(path).await?;

                // Add current timestamps
                let now_utc = chrono::Utc::now();
                status.timestamp_utc = now_utc.to_rfc3339();
                status.timestamp = if let Some(tz_str) = timezone {
                    match tz_str.parse::<chrono_tz::Tz>() {
                        Ok(tz) => now_utc
                            .with_timezone(&tz)
                            .format("%Y-%m-%d %H:%M:%S %Z")
                            .to_string(),
                        Err(_) => {
                            return Err(anyhow::anyhow!(
                                "timezone '{tz_str}' is not a valid IANA tz (e.g. 'America/New_York', 'UTC')"
                            ));
                        }
                    }
                } else {
                    chrono::Local::now()
                        .format("%Y-%m-%d %H:%M:%S %Z")
                        .to_string()
                };

                Ok(Self::build_status_response(&status))
            }
            SessionAction::List => {
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

                let sessions = self
                    .runtime
                    .list_sessions(
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
                    .get("path")
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
            SessionAction::Find => {
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'find' requires the 'query' parameter"))?;
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
            SessionAction::Copy => {
                let session_key = Self::require_path(&params, "copy")?;
                let (parent_str, slug) = Self::parse_target(&params, "copy")?;
                let target_parent =
                    if parent_str.is_empty() { "/".to_string() } else { parent_str.to_string() };
                let label = params
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let outcome = self
                    .runtime
                    .copy_session(session_key, target_parent, slug.to_string(), label)
                    .await?;
                Ok(serde_json::to_value(outcome)?)
            }
            SessionAction::Move => {
                // `move` subsumes both reparent (mv to a new dir) and
                // in-place rename (mv to a new name in the same dir).
                // The unified `target` field (Commit H, 2026-08-22)
                // handles both cases — to rename in place, set `target`
                // to `<current_parent>/<new_slug>`. `title` (optional)
                // is a separate display-label change and is the only
                // way to change the label without re-targeting.
                let session_key = Self::require_path(&params, "move")?;
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if params.get("target").is_none() && title.is_none() {
                    return Err(anyhow::anyhow!(
                        "'move' requires at least one of 'target' or 'title'"
                    ));
                }

                if params.get("target").is_some() {
                    let (parent_str, slug) = Self::parse_target(&params, "move")?;
                    let new_parent = if parent_str.is_empty() {
                        "/".to_string()
                    } else {
                        parent_str.to_string()
                    };
                    self.runtime
                        .move_session(session_key, new_parent, Some(slug.to_string()))
                        .await?;
                }
                if let Some(t) = title {
                    self.runtime
                        .rename_session(session_key, Some(t), None)
                        .await?;
                }
                Ok(json!({
                    "path": session_key,
                    "modified": true,
                }))
            }
            SessionAction::Remove => {
                let session_key = Self::require_path(&params, "remove")?;
                let recursive = params
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let outcome = self.runtime.delete_session(session_key, recursive).await?;
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

    /// F5 (2026-08-11 field test, Addendum 3) + round 7 (2026-08-13):
    /// the model was anchoring on the legacy 3 actions (`status` /
    /// `list` / `history`) and refused to call the lifecycle
    /// operations. The history: WS4 demoted all 6 lifecycle
    /// operations; round 7 restored the storage-only three
    /// (`branch` / `archive` / `unarchive`), bringing the surface to
    /// 9 actions; `move` (reparent) brought it to 10. Sprint 7
    /// Commit F (2026-08-21) trimmed to 7 bash-aligned verbs
    /// (`status` / `list` / `history` / `find` / `copy` / `move` /
    /// `remove`) — `rename` folded into `move` (title/slug without
    /// new_parent), `archive` / `unarchive` dropped (sessions are
    /// monotonically visible until `remove`), `search` → `find`,
    /// `branch` → `copy`, `delete` → `remove`. Pin the description
    /// here so any future edit that drops one of the 7 action names
    /// fails the test — defense-in-depth against the "register
    /// without surfacing in description" omission pattern (F5).
    #[test]
    fn description_names_all_7_actions() {
        let cache = SessionCache::new("test");
        let tool = SessionTool::new(Arc::new(cache).as_shared());
        let desc = tool.description();

        // The 7 actions, in the order they appear in `SessionAction`.
        // If `SessionAction` ever grows, bump this list in lockstep.
        let expected_actions = [
            "status", "list", "history", "find", "copy", "move", "remove",
        ];
        assert_eq!(
            expected_actions.len(),
            7,
            "test bug: expected_actions must have 7 entries"
        );

        for action in expected_actions {
            assert!(
                desc.contains(action),
                "session description must name the `{action}` action (F5: model anchored on \
                 legacy 3 and refused lifecycle ops; description must surface every action)"
            );
        }

        // Lead-with-count: the description must advertise the action
        // count up front so the model sees all 7 before any
        // per-action bullet (defeats primacy bias on the legacy 3).
        assert!(
            desc.contains("7 operations") || desc.contains("7 actions") || desc.contains("7 op"),
            "session description must lead with the action count (F5: defeats primacy bias)"
        );

        // The retired verbs MUST NOT appear as standalone per-action
        // bullets (the format is `<verb>: ...` — the same shape the
        // model anchors on). We only flag the dangerous prefix
        // pattern, not the prose, because the description legitimately
        // mentions the old verbs in sentences like "rename it in
        // place" or "delete a session" to disambiguate. A naive
        // substring check on those words would falsely fail.
        for retired in ["rename", "delete", "branch", "archive", "unarchive"] {
            let bullet_prefix = format!("- {retired}:");
            assert!(
                !desc.contains(&bullet_prefix),
                "session description must not list `{retired}` as a per-action bullet \
                 (Sprint 7 Commit F retired it; the new 7 actions are status/list/history/find/copy/move/remove)"
            );
        }
    }

    fn create_test_cache() -> Arc<SessionCache> {
        let cache = SessionCache::new("main");

        let session = SessionInfo {
            session_key: "test-session".to_string(),
            session_id: "abc123".to_string(),
            agent_id: Some("test-agent".to_string()),
            label: Some("Test Session".to_string()),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 10,
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
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
            .execute(json!({"action": "history", "path": "test-session", "limit": 10}))
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
            .execute(json!({"action": "status", "path": "test-session"}))
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
            agent_id: Some("main".to_string()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 5,
            peer_type: None,
            peer_id: None,
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
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
            .execute(json!({"action": "history", "path": "missing"}))
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
            .execute(json!({"action": "status", "path": "missing"}))
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
            agent_id: Some("test-agent".to_string()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 5,
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
        };
        let alice_other = SessionInfo {
            session_key: "alice-2".to_string(),
            session_id: "alice-2".to_string(),
            agent_id: Some("other-agent".to_string()),
            label: None,
            created_at: "2024-01-02T00:00:00Z".to_string(),
            last_activity: "2024-01-02T01:00:00Z".to_string(),
            message_count: 3,
            peer_type: Some("user".to_string()),
            peer_id: Some("alice".to_string()),
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
        };
        let bob_main = SessionInfo {
            session_key: "bob-1".to_string(),
            session_id: "bob-1".to_string(),
            agent_id: Some("test-agent".to_string()),
            label: None,
            created_at: "2024-01-03T00:00:00Z".to_string(),
            last_activity: "2024-01-03T01:00:00Z".to_string(),
            message_count: 7,
            peer_type: Some("user".to_string()),
            peer_id: Some("bob".to_string()),
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
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
    async fn test_session_list_drops_kinds_param_silently() {
        // The kinds filter was removed from the session tool (round-6 F1).
        // The model still has the underlying data on each result: it
        // derives "spawned" by `parent_session_id is not None` (visible
        // on status results).
        // An old payload that includes `kinds` must surface no error
        // and just be ignored — the unfiltered list is returned.
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
        assert_eq!(result["total"], 2);
        assert!(ids.contains(&"alice-1"));
        assert!(ids.contains(&"alice-2"));
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
    // Tests: mutation actions (find/copy/move/remove). `new` /
    // `resume` / `compact` stay refused on this tool — they drive
    // the LLM and live on the Agent tool.
    // ====================================================================================

    #[tokio::test]
    async fn test_session_find_happy_path_and_missing_query() {
        let cache = SessionCache::new("main");
        let session = SessionInfo {
            session_key: "s1".to_string(),
            session_id: "s1".to_string(),
            agent_id: Some("main".to_string()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 1,
            peer_type: None,
            peer_id: None,
            archived: false,
            run_active: false,
            slug: None,
            path: String::new(),
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
            .execute(json!({"action": "find", "query": "FRAMBULATOR"}))
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
            .execute(json!({"action": "find"}))
            .await
            .expect_err("find without query must error");
        assert!(err.to_string().contains("query"), "{err}");
    }

    #[tokio::test]
    async fn test_session_remove_happy_and_missing_key() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({"action": "remove", "path": "test-session"}))
            .await
            .unwrap();
        assert_eq!(result["deleted"].as_array().unwrap().len(), 1);
        assert_eq!(result["deleted"][0], "test-session");

        // Gone from the list.
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(list["total"], 0);

        let err = tool
            .execute(json!({"action": "remove"}))
            .await
            .expect_err("remove without path must error");
        assert!(err.to_string().contains("path"), "{err}");
    }

    /// The LLM-driving actions (`new` / `resume` / `compact`) and the
    /// Sprint 7 Commit F-retired actions (`search` / `rename` /
    /// `delete` / `branch` / `archive` / `unarchive`) must all be
    /// rejected by the schema validation on this tool — they live on
    /// the Agent tool or were retired, respectively.
    #[tokio::test]
    async fn test_demoted_actions_rejected_with_clear_validation_error() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        for demoted in [
            // Agent-tool-only (drive the LLM).
            "new", "resume", "compact",
            // Sprint 7 Commit F retired (bash-aligned verb replaced them).
            "search", "rename", "delete", "branch", "archive", "unarchive",
        ] {
            let exec_err = tool
                .execute(json!({"action": demoted}))
                .await
                .expect_err(&format!(
                    "demoted action `{demoted}` must error, not silently route"
                ));
            assert!(
                exec_err.to_string().contains("Invalid action"),
                "demoted `{demoted}` should fail with Invalid action, got: {exec_err}"
            );
        }
    }

    #[tokio::test]
    async fn test_session_copy_creates_stored_copy_not_running() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({
                "action": "copy",
                "path": "test-session",
                "target": "test-session/fork",
                "label": "fork",
            }))
            .await
            .unwrap();
        let new_id = result["new_session_id"].as_str().unwrap();
        assert_eq!(result["parent_session_id"], "test-session");
        assert_ne!(new_id, "test-session");

        // The copy is stored (listed) but NOT running, and carries the
        // copy label.
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        let copy = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["session_key"] == new_id)
            .expect("copy must appear in list");
        assert_eq!(copy["run_active"], false);
        assert_eq!(copy["label"], "fork");

        // History is copied over.
        let history = tool
            .execute(json!({"action": "history", "path": new_id}))
            .await
            .unwrap();
        assert_eq!(history["total_messages"], 2);

        // Copy without path must error.
        let err = tool
            .execute(json!({"action": "copy", "target": "x/y"}))
            .await
            .expect_err("copy without path must error");
        assert!(err.to_string().contains("path"), "{err}");

        // Copy without target must error.
        let err = tool
            .execute(json!({"action": "copy", "path": "test-session"}))
            .await
            .expect_err("copy without target must error");
        assert!(err.to_string().contains("target"), "{err}");
    }

    #[tokio::test]
    async fn test_session_status_invalid_timezone_errors() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let err = tool
            .execute(json!({
                "action": "status",
                "path": "test-session",
                "timezone": "Not/AZone"
            }))
            .await
            .expect_err("invalid timezone must error, not fall back to local");
        assert!(err.to_string().contains("not a valid IANA tz"), "{err}");

        // A valid IANA tz still works.
        tool.execute(json!({
            "action": "status",
            "path": "test-session",
            "timezone": "UTC"
        }))
        .await
        .unwrap();
    }

    /// The `limit` schema default must match the history handler's
    /// fallback (100) — a schema/handler drift here silently changes
    /// what the model gets when it omits `limit`.
    #[test]
    fn limit_schema_default_matches_history_handler() {
        let cache = SessionCache::new("test");
        let tool = SessionTool::new(Arc::new(cache).as_shared());
        let schema = tool.parameters();
        assert_eq!(schema["properties"]["limit"]["default"], 100);
    }

    #[tokio::test]
    async fn test_session_move_reparent_happy_and_unknown_session() {
        let cache = create_test_cache();
        cache.add_session(
            "parent-s".to_string(),
            SessionInfo {
                session_key: "parent-s".to_string(),
                session_id: "parent-s".to_string(),
                agent_id: Some("test-agent".to_string()),
                label: None,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                last_activity: "2024-01-01T01:00:00Z".to_string(),
                message_count: 0,
                peer_type: None,
                peer_id: None,
                archived: false,
                run_active: false,
                slug: None,
                path: String::new(),
            },
            vec![],
            dummy_status("parent-s"),
        );
        let tool = SessionTool::new(cache.as_shared());

        let result = tool
            .execute(json!({
                "action": "move",
                "path": "test-session",
                "target": "parent-s/task-b",
            }))
            .await
            .unwrap();
        assert_eq!(result["modified"], true);

        // The reparent is visible on status.
        let status = tool
            .execute(json!({"action": "status", "path": "test-session"}))
            .await
            .unwrap();
        assert_eq!(status["parent_session"], "parent-s");

        // path is always required.
        let err = tool
            .execute(json!({"action": "move", "target": "parent-s/x"}))
            .await
            .expect_err("move without path must error");
        assert!(err.to_string().contains("path"), "{err}");

        // Unknown endpoints error.
        let err = tool
            .execute(json!({
                "action": "move",
                "path": "missing",
                "target": "parent-s/x",
            }))
            .await
            .expect_err("move of an unknown session must error");
        assert!(err.to_string().contains("missing"), "{err}");
    }

    /// Sprint 7 Commit F: `move` with `target` set to
    /// `<current_parent>/<new_slug>` is the bash `mv` rename-in-place
    /// case. Pre-merge this was the `rename` action; Commit F subsumed
    /// it, Commit H unified the addressing under `target`.
    #[tokio::test]
    async fn test_session_move_rename_in_place_with_slug_title_both() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        // Slug only — rename in place at the root (test-session has
        // no recorded parent in the cache).
        tool.execute(json!({"action": "move", "path": "test-session", "target": "/task-b"}))
            .await
            .unwrap();
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(list["sessions"][0]["slug"], "task-b");
        // Title untouched by a slug-only rename.
        assert_eq!(list["sessions"][0]["label"], "Test Session");

        // Title only (slug survives) — target must be present even
        // when only the display label changes.
        tool.execute(
            json!({"action": "move", "path": "test-session", "target": "/task-b", "title": "New Title"}),
        )
        .await
        .unwrap();
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(list["sessions"][0]["label"], "New Title");
        assert_eq!(list["sessions"][0]["slug"], "task-b");

        // Both at once — different slug, new title.
        tool.execute(
            json!({"action": "move", "path": "test-session", "target": "/s2", "title": "T"}),
        )
        .await
        .unwrap();
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(list["sessions"][0]["label"], "T");
        assert_eq!(list["sessions"][0]["slug"], "s2");
    }

    /// `move` with NOTHING (no target, no title) is a no-op — refuse
    /// it explicitly so the model can't accidentally call move with
    /// all default params and get a false success.
    #[tokio::test]
    async fn test_session_move_all_none_params_errors() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let err = tool
            .execute(json!({"action": "move", "path": "test-session"}))
            .await
            .expect_err("move with no target/title must error");
        assert!(err.to_string().contains("target"), "{err}");
        assert!(err.to_string().contains("title"), "{err}");
    }

    /// Sprint 7 Commit G: tool-layer `validate_path` mirrors
    /// `AgentArgs::validate_action_args`. Raw session ids (with the
    /// reserved `:` that legacy ids carry) and malformed slug paths
    /// are rejected with a structured error at the tool boundary,
    /// before the runtime touches state.
    #[tokio::test]
    async fn test_session_path_validation_at_tool_boundary() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        // Legacy raw session id shape (contains `:` which slugs forbid).
        let err = tool
            .execute(json!({
                "action": "copy",
                "path": "root:user:alice",
            }))
            .await
            .expect_err("raw session id shape must error at the boundary");
        assert!(err.to_string().contains("path"), "{err}");
        assert!(err.to_string().contains("slug"), "{err}");

        // Empty path.
        let err = tool
            .execute(json!({"action": "copy", "path": ""}))
            .await
            .expect_err("empty path must error");
        assert!(err.to_string().contains("path"), "{err}");

        // Bare `/` (no segments) is refused (validate_path says supply
        // at least one segment).
        let err = tool
            .execute(json!({"action": "copy", "path": "/"}))
            .await
            .expect_err("bare '/' path must error");
        assert!(err.to_string().contains("path"), "{err}");

        // Empty segment (`/a//b`).
        let err = tool
            .execute(json!({"action": "copy", "path": "/a//b"}))
            .await
            .expect_err("empty segment path must error");
        assert!(err.to_string().contains("segment"), "{err}");

        // Leading/trailing whitespace in a segment.
        let err = tool
            .execute(json!({"action": "copy", "path": "/ a/b"}))
            .await
            .expect_err("whitespace in segment must error");
        assert!(err.to_string().contains("whitespace"), "{err}");

        // Valid slug path passes through to the runtime (success on a
        // known slug is not required here — we just need the
        // validator to let it through, which we check by ensuring
        // the error message does NOT mention path validation).
        let res = tool
            .execute(json!({"action": "copy", "path": "test-session"}))
            .await;
        // Either success (slug exists in cache) or a runtime-level
        // error — but never a "path is not a valid slug path" error.
        if let Err(e) = res {
            assert!(
                !e.to_string().contains("is not a valid slug path"),
                "valid slug path should pass tool-layer validation, got: {e}"
            );
        }
    }

    /// `target` is also a slug path — same shape rules as `path`.
    /// Validate at the tool boundary.
    #[tokio::test]
    async fn test_session_move_target_validates_at_tool_boundary() {
        let cache = create_test_cache();
        let tool = SessionTool::new(cache.as_shared());

        let err = tool
            .execute(json!({
                "action": "move",
                "path": "test-session",
                "target": "root:user:bob/x",
            }))
            .await
            .expect_err("raw-id target must error at the boundary");
        assert!(err.to_string().contains("target"), "{err}");
        assert!(err.to_string().contains("slug"), "{err}");
    }
}
