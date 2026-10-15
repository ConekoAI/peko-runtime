//! Unified `Session` tool — single storage entry point that dispatches
//! by `action` over 10 operations (`status` / `list` / `history` /
//! `find` / `copy` / `move` / `remove` / `list_pages` / `read_page` /
//! `search_pages`).
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
//! until `remove`; the `archived` flag and the `include_archived`
//! filter were removed entirely on 2026-09-26). `rename` removed — its semantics folded into `move`
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
//! [`SessionRuntime`](super::SessionRuntime) port.

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
    pub(crate) fn tool_description() -> String {
        r"Inspect and manage your principal's stored sessions with 10 actions. Pure storage: no LLM runs here; to run work in a session use the Agent tool.

## Addressing
`path` and `target` are absolute slug paths such as `sess:/user-bob/writer-1` — use the `path` field from `list`. `sess:/` is your trunk (readable, and usable as a list/find scope, but never moved or removed). Raw session ids and relative slugs are refused. Actions that read one session default to your current session when `path` is omitted.

## Actions
- status: metadata and token usage of one session (`timezone` formats timestamps).
- list: sessions, optionally limited to the subtree at `path` and filtered by `peer`, `agent_name`, or `active_minutes`.
- history: a session's messages (`include_tools`, `limit`).
- find: case-insensitive text search across transcripts (`query`; optional `peer` and subtree `path`).
- copy: duplicate `path` to `target` (`<parent path>/<new slug>`, like `cp`). The copy is not running; to work on its context use Agent action `branch` with `source` set to the copy.
- move: reparent and/or rename with `target` (like `mv`; children move along), change the display `title`, or set `page_limit`, the number of closed compaction pages kept (0 = unlimited; older pages are deleted immediately and permanently).
- remove: delete a session; `recursive: true` also deletes its descendants.
- list_pages: the compaction pages of a session. Each compaction archives the transcript before it as a numbered page; numbers never shift when old pages are pruned.
- read_page: one page as transcript text (`page` from list_pages; `offset` / `limit` window the lines). A cut response says which `offset` continues it.
- search_pages: case-insensitive search across every page including the live one (`query`, `max_results`); follow a hit with read_page.

## Refusals
move and remove refuse your trunk, the session you are running in, and any session with an active run; a move into the session's own subtree is refused. There is no archive: to set a session aside, move it into `sess:/trash` and remove it later. New principals start with two ordinary sessions, `sess:/tmp` for throwaway work and `sess:/trash` for pending removals; nothing recreates them once removed."
            .to_string()
    }

    pub(crate) fn tool_parameters() -> serde_json::Value {
        let schema = json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "history", "find", "copy", "move", "remove", "list_pages", "read_page", "search_pages"],
                    "description": "status/list/history read sessions; find searches transcripts; copy/move/remove manage storage; list_pages/read_page/search_pages read compaction pages"
                },
                "path": {
                    "type": "string",
                    "description": "Absolute slug path ('sess:/a/b', from list). Required for copy/move/remove; defaults to your session for reads; a subtree scope for list/find"
                },
                "target": {
                    "type": "string",
                    "description": "Destination '<parent path>/<new slug>' (slug: 1-64 chars, no '/'); for move, '<current parent>/<new slug>' renames in place"
                },
                "query": {
                    "type": "string",
                    "description": "Case-insensitive text to search for"
                },
                "page": {
                    "type": "integer",
                    "maximum": usize::MAX,
                    "minimum": 1,
                    "description": "Page number from list_pages"
                },
                "offset": {
                    "type": "integer",
                    "maximum": usize::MAX,
                    "minimum": 0,
                    "default": 0,
                    "description": "Line offset to start from (a cut response names the next one)"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": usize::MAX,
                    "default": 20,
                    "description": "Most hits to return"
                },
                "title": {
                    "type": "string",
                    "description": "Display label (does not affect the path)"
                },
                "page_limit": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 10000,
                    "description": "Closed compaction pages to keep (0 = unlimited); older pages are deleted immediately and permanently"
                },
                "recursive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Also delete the session's descendants"
                },
                "peer": {
                    "type": "string",
                    "description": "Only sessions with this peer, e.g. 'user:alice' or 'public'"
                },
                "agent_name": {
                    "type": "string",
                    "description": "Only sessions of this agent (the agent_name field of list entries)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": usize::MAX,
                    "description": "Most results (list/find 50, history 100, read_page 200 lines by default)"
                },
                "active_minutes": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": u64::MAX / 60_000,
                    "description": "Only sessions active in the last N minutes"
                },
                "include_tools": {
                    "type": "boolean",
                    "default": true,
                    "description": "Include tool calls and results"
                },
                "timezone": {
                    "type": "string",
                    "description": "IANA timezone for timestamps, e.g. 'America/New_York'"
                }
            },
        });
        let variants = [
            ("status", &["path", "timezone"][..], &[][..]),
            (
                "list",
                &["path", "peer", "agent_name", "active_minutes", "limit"][..],
                &[][..],
            ),
            ("history", &["path", "limit", "include_tools"][..], &[][..]),
            (
                "find",
                &["query", "path", "peer", "limit"][..],
                &["query"][..],
            ),
            (
                "copy",
                &["path", "target", "title"][..],
                &["path", "target"][..],
            ),
            (
                "move",
                &["path", "target", "title", "page_limit"][..],
                &["path"][..],
            ),
            ("remove", &["path", "recursive"][..], &["path"][..]),
            ("list_pages", &["path"][..], &[][..]),
            (
                "read_page",
                &["page", "path", "offset", "limit"][..],
                &["page"][..],
            ),
            (
                "search_pages",
                &["query", "path", "max_results"][..],
                &["query"][..],
            ),
        ];
        let branches: Vec<_> = variants.into_iter().map(|(action, allowed, required)| {
            let properties: serde_json::Map<String, serde_json::Value> = allowed.iter()
                .map(|field| ((*field).to_string(), schema["properties"][*field].clone())).collect();
            let mut branch = json!({"type": "object", "properties": properties, "required": required});
            let limit_default = match action {
                "list" | "find" => Some(50),
                "history" => Some(100),
                "read_page" => Some(200),
                _ => None,
            };
            if let Some(default) = limit_default {
                branch["properties"]["limit"]["default"] = json!(default);
            }
            if action == "move" {
                branch["anyOf"] = json!([{"required": ["target"]}, {"required": ["title"]}, {"required": ["page_limit"]}]);
            }
            (action, branch)
        }).collect();
        let mut result = peko_tools_core::schema::action_schema(&branches);
        // No global limit default: defaults belong to the selected action.
        result["properties"]["limit"] = schema["properties"]["limit"].clone();
        result
    }

    fn action_schema() -> &'static crate::tools::action_schema::ActionSchema {
        static SCHEMA: std::sync::OnceLock<crate::tools::action_schema::ActionSchema> =
            std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| {
            crate::tools::action_schema::ActionSchema::new("Session", Self::tool_parameters())
        })
    }

    pub(crate) fn validate_params(params: &serde_json::Value) -> anyhow::Result<()> {
        Self::action_schema().validate(params)
    }

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

    fn build_history_response(path: &str, messages: Vec<HistoryMessage>) -> serde_json::Value {
        json!({
            "path": path,
            "total_messages": messages.len(),
            "messages": messages,
        })
    }

    /// Extract a required `path` param with an actionable error, and
    /// validate the slug shape at the tool boundary (mirrors
    /// `AgentArgs::validate_action_args`). Raw session ids and
    /// malformed paths are rejected here so the model gets a clean
    /// structured error before the runtime touches state.
    fn require_path<'a>(params: &'a serde_json::Value, action: &str) -> anyhow::Result<&'a str> {
        let path = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "action \"{action}\" requires 'path' — an absolute slug path \
                     ('sess:/a/b/c') naming the target session (use the `path` field from \
                     `Session` with action `list`)"
            )
        })?;
        if let Err(e) = peko_session::path::validate_path(path) {
            return Err(anyhow::anyhow!(
                "action \"{action}\" 'path' is not a valid slug path: {e}"
            ));
        }
        Ok(path)
    }

    /// Optional subtree scope for `list` / `find`: the `path` param
    /// names the subtree ROOT (a session reference, `sess:/a/b` — same
    /// addressing as every other action). Shape-validated here
    /// (early-fail, mirrors `require_path`); resolved to a canonical
    /// session id at the runtime layer. Absent / empty ⇒ `None` (the
    /// whole store). `sess:/` (or `/`) scopes reads to the caller's
    /// tree root; mutation paths still require a nonempty slug segment.
    fn optional_subtree(params: &serde_json::Value) -> anyhow::Result<Option<String>> {
        match params.get("path").and_then(|v| v.as_str()) {
            None | Some("") => Ok(None),
            Some(path) => {
                if peko_session::path::strip_scheme_prefix(path) != "/" {
                    peko_session::path::validate_path(path).map_err(|e| {
                        anyhow::anyhow!("'path' subtree scope is not a valid slug path: {e}")
                    })?;
                }
                Ok(Some(path.to_string()))
            }
        }
    }

    /// Extract and parse the bash-style `target` param for `copy` and
    /// `move`. The target is a slug path: the last segment is the new
    /// slug (under the parent = everything before), mirroring bash
    /// `cp src dst` / `mv src dst`. Returns `(parent_str, slug)` —
    /// `parent_str` may be empty when the target is a single-segment
    /// slug whose new parent is the caller's tree root; the caller
    /// passes `/` to the runtime in that case. The `sess:` scheme
    /// prefix is stripped before splitting so `sess:/slug` yields an
    /// empty `parent_str` exactly like the legacy bare form.
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
                     naming the destination ('sess:/parent/new_slug'); the last segment is \
                     the new slug, the rest is the destination parent. Mirrors bash \
                     `cp src dst` / `mv src dst`."
                )
            })?;
        peko_session::path::validate_path(target).map_err(|e| {
            anyhow::anyhow!("action \"{action}\" 'target' is not a valid slug path: {e}")
        })?;
        let target = peko_session::path::strip_scheme_prefix(target);
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

/// Actions supported by the `Session` tool.
///
/// Sprint 7 Commit F (2026-08-21): 7 actions, bash-aligned.
/// `new` / `resume` / `compact` stay off this tool — they drive the
/// LLM and live on the Agent tool.
///
/// ADR-051 (2026-09-05): +3 read actions for the compaction-page
/// archive (`list_pages` / `read_page` / `search_pages`).
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
    ListPages,
    ReadPage,
    SearchPages,
}

#[async_trait]
impl Tool for SessionTool {
    fn name(&self) -> &'static str {
        "Session"
    }

    fn description(&self) -> String {
        Self::tool_description()
    }

    fn parameters(&self) -> serde_json::Value {
        Self::tool_parameters()
    }

    async fn execute(&self, mut params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Self::action_schema().normalize(&mut params)?;
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
                        s.parse::<peko_subject::ActorRef>()
                            .map_err(|e| anyhow::anyhow!("Invalid peer '{s}': {e}"))?,
                    ),
                    None => None,
                };
                let agent_id = params.get("agent_name").and_then(|v| v.as_str());
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let active_minutes = params.get("active_minutes").and_then(|v| v.as_i64());
                let subtree = Self::optional_subtree(&params)?;

                let sessions = self
                    .runtime
                    .list_sessions(
                        peer.as_ref(),
                        agent_id,
                        limit,
                        active_minutes,
                        subtree.as_deref(),
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
                Ok(Self::build_history_response(&messages.1, messages.0))
            }
            SessionAction::Find => {
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'find' requires the 'query' parameter"))?;
                let peer_str = params.get("peer").and_then(|v| v.as_str());
                let peer = match peer_str {
                    Some(s) => Some(
                        s.parse::<peko_subject::ActorRef>()
                            .map_err(|e| anyhow::anyhow!("Invalid peer '{s}': {e}"))?,
                    ),
                    None => None,
                };
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let subtree = Self::optional_subtree(&params)?;

                let hits = self
                    .runtime
                    .search_sessions(query, peer.as_ref(), limit, subtree.as_deref())
                    .await?;
                Ok(json!({
                    "total": hits.len(),
                    "hits": hits,
                }))
            }
            SessionAction::Copy => {
                let session_key = Self::require_path(&params, "copy")?;
                let (parent_str, slug) = Self::parse_target(&params, "copy")?;
                let target_parent = if parent_str.is_empty() {
                    "/".to_string()
                } else {
                    parent_str.to_string()
                };
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let outcome = self
                    .runtime
                    .copy_session(session_key, target_parent, slug.to_string(), title)
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
                let mut session_key = Self::require_path(&params, "move")?.to_string();
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if params.get("target").is_none()
                    && title.is_none()
                    && params.get("page_limit").is_none()
                {
                    return Err(anyhow::anyhow!(
                        "'move' requires at least one of 'target', 'title', or 'page_limit'"
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
                        .move_session(&session_key, new_parent, Some(slug.to_string()))
                        .await?;
                    session_key = params["target"]
                        .as_str()
                        .expect("validated target")
                        .to_string();
                }
                if let Some(t) = title {
                    self.runtime
                        .rename_session(&session_key, Some(t), None)
                        .await?;
                }

                // Retention (page_limit): 0 = unlimited; any other
                // value caps the closed compaction pages — pruning is
                // immediate and IRREVERSIBLE, so the pruned count is
                // echoed.
                let mut response = json!({
                    "path": session_key,
                    "modified": true,
                });
                if let Some(raw) = params.get("page_limit").and_then(|v| v.as_u64()) {
                    let limit = u32::try_from(raw)
                        .ok()
                        .filter(|l| *l <= 10_000)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "'page_limit' must be between 0 and 10000 (0 = unlimited)"
                            )
                        })?;
                    let pruned = self
                        .runtime
                        .set_page_limit(&session_key, if limit == 0 { None } else { Some(limit) })
                        .await?;
                    response["page_limit"] = if limit == 0 {
                        serde_json::json!(null)
                    } else {
                        serde_json::json!(limit)
                    };
                    response["pages_pruned"] = serde_json::json!(pruned);
                }
                Ok(response)
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
            SessionAction::ListPages => {
                let session_key = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.runtime.current_session_key())
                    .to_string();

                let catalog = self.runtime.list_pages(&session_key).await?;
                Ok(json!({
                    "path": session_key,
                    "total": catalog.pages.len(),
                    "first_available_page": catalog.first_available_page,
                    "page_limit": catalog.page_limit,
                    "pages": catalog.pages,
                }))
            }
            SessionAction::ReadPage => {
                let session_key = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.runtime.current_session_key())
                    .to_string();
                let page = params.get("page").and_then(|v| v.as_u64()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "'read_page' requires the 'page' parameter — a 1-based page \
                         number from 'list_pages'"
                    )
                })? as usize;
                let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

                let content = self
                    .runtime
                    .read_page(&session_key, page, offset, limit)
                    .await?;
                Ok(json!({
                    "path": session_key,
                    "page": page,
                    "offset": offset,
                    "content": content,
                }))
            }
            SessionAction::SearchPages => {
                let session_key = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.runtime.current_session_key())
                    .to_string();
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("'search_pages' requires the 'query' parameter")
                    })?;
                let max_results = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20) as usize;

                let hits = self
                    .runtime
                    .search_pages(&session_key, query, max_results)
                    .await?;
                Ok(json!({
                    "path": session_key,
                    "total": hits.len(),
                    "hits": hits,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! The Session tool over the production `SessionManagerRuntime` and a
    //! real tempdir session store (`session_runtime_impl::test_support`).
    //! Tree used by most tests (`tree_harness`):
    //! `root:user:alice` (`sess:/`) ── `spawn1` (`sess:/a`) ── `child1` (`sess:/a/b`)
    //!                                └── `spawn2` (`sess:/c`)
    use super::*;
    use crate::session::session_runtime_impl::test_support::{sid, tree_harness, Harness};
    use peko_subject::ActorRef;
    use serde_json::{json, Value};

    async fn tool_over(h: &Harness) -> SessionTool {
        SessionTool::new(h.runtime.clone())
    }

    fn ids(list: &Value, key: &str, field: &str) -> Vec<String> {
        list[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s[field].as_str().unwrap().to_string())
            .collect()
    }

    /// F5 (2026-08-11 field test) through ADR-051: the model anchored on
    /// a few legacy actions and refused the rest, so the description must
    /// name all 10, lead with the count, and not bullet retired verbs.
    #[tokio::test]
    async fn description_names_all_10_actions() {
        let desc = tool_over(&Harness::new().await).await.description();
        for action in [
            "status",
            "list",
            "history",
            "find",
            "copy",
            "move",
            "remove",
            "list_pages",
            "read_page",
            "search_pages",
        ] {
            assert!(desc.contains(action), "description must name `{action}`");
        }
        assert!(
            desc.contains("10 operations") || desc.contains("10 actions") || desc.contains("10 op"),
            "description must lead with the action count"
        );
        for retired in ["rename", "delete", "branch", "archive", "unarchive"] {
            assert!(
                !desc.contains(&format!("- {retired}:")),
                "`{retired}` must not be a per-action bullet"
            );
        }
    }

    /// A single schema default would misrepresent the per-action handler
    /// defaults; they're advertised per variant and in the description.
    #[tokio::test]
    async fn limit_schema_describes_per_action_defaults() {
        let schema = tool_over(&Harness::new().await).await.parameters();
        assert!(schema["properties"]["limit"].get("default").is_none());
        let description = schema["properties"]["limit"]["description"]
            .as_str()
            .unwrap();
        for default in ["50", "100", "200"] {
            assert!(description.contains(default));
        }
        for (action, expected) in [
            ("list", 50),
            ("find", 50),
            ("history", 100),
            ("read_page", 200),
        ] {
            let variant = schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|branch| branch["properties"]["action"]["const"] == action)
                .unwrap();
            assert_eq!(variant["properties"]["limit"]["default"], expected);
        }
    }

    #[tokio::test]
    async fn list_shows_every_session_with_its_path() {
        let h = tree_harness("root:user:alice").await;
        let list = tool_over(&h)
            .await
            .execute(json!({"action": "list"}))
            .await
            .unwrap();
        assert_eq!(list["total"], 4, "{list}");
        let paths = ids(&list, "sessions", "path");
        for path in ["sess:/", "sess:/a", "sess:/a/b", "sess:/c"] {
            assert!(paths.contains(&path.to_string()), "{path}: {paths:?}");
        }
        assert!(ids(&list, "sessions", "session_id").contains(&sid("spawn1")));

        let empty = Harness::new().await;
        let list = tool_over(&empty)
            .await
            .execute(json!({"action": "list"}))
            .await
            .unwrap();
        assert_eq!(list["total"], 0);
    }

    #[tokio::test]
    async fn list_and_find_accept_a_subtree_path_scope() {
        let h = tree_harness("root:user:alice").await;
        for id in ["root:user:alice", "spawn1", "child1", "spawn2"] {
            h.add_user_message(id, &format!("needle in {id}")).await;
        }
        let tool = tool_over(&h).await;

        let list = tool
            .execute(json!({"action": "list", "path": "sess:/a"}))
            .await
            .unwrap();
        let mut paths = ids(&list, "sessions", "path");
        paths.sort();
        assert_eq!(paths, ["sess:/a", "sess:/a/b"]);

        let found = tool
            .execute(json!({"action": "find", "query": "needle", "path": "sess:/a"}))
            .await
            .unwrap();
        let mut hit_paths = ids(&found, "hits", "path");
        hit_paths.sort();
        assert_eq!(hit_paths, ["sess:/a", "sess:/a/b"], "{found}");

        for params in [
            json!({"action": "list", "path": "/p//c1"}),
            json!({"action": "find", "query": "x", "path": "a//b"}),
        ] {
            let err = tool.execute(params).await.unwrap_err();
            assert!(err.to_string().contains("not a valid slug path"), "{err}");
        }
    }

    #[tokio::test]
    async fn history_returns_the_sessions_messages() {
        let h = tree_harness("root:user:alice").await;
        h.add_user_message("spawn1", "Hello").await;
        let tool = tool_over(&h).await;
        let history = tool
            .execute(json!({"action": "history", "path": "sess:/a", "limit": 10}))
            .await
            .unwrap();
        assert_eq!(history["total_messages"], 1, "{history}");
        assert_eq!(history["messages"][0]["role"], "user");
        assert_eq!(history["messages"][0]["content"], "Hello");
    }

    /// Unknown sessions are errors on every read, never a fabricated empty
    /// or zeroed record.
    #[tokio::test]
    async fn unknown_sessions_are_errors() {
        let h = tree_harness("root:user:alice").await;
        let tool = tool_over(&h).await;
        for action in ["status", "history"] {
            let err = tool
                .execute(json!({"action": action, "path": "sess:/missing"}))
                .await
                .expect_err(action);
            assert!(err.to_string().contains("missing"), "{action}: {err}");
        }
    }

    #[tokio::test]
    async fn status_reports_the_session_and_defaults_to_the_current_one() {
        let h = tree_harness("spawn1").await;
        h.add_user_message("spawn1", "hi").await;
        let tool = tool_over(&h).await;

        let status = tool
            .execute(json!({"action": "status", "path": "sess:/c"}))
            .await
            .unwrap();
        assert_eq!(status["session_id"], sid("spawn2"));
        assert_eq!(status["peer_type"], "user");
        assert_eq!(status["peer_id"], "alice");

        // (Counters such as `message_count` come from the session index,
        // whose per-controller cache can trail another writer by ~30s.)
        let current = tool.execute(json!({"action": "status"})).await.unwrap();
        assert_eq!(current["session_id"], sid("spawn1"));
        assert_eq!(current["path"], "sess:/a");
    }

    #[tokio::test]
    async fn status_validates_the_timezone() {
        let h = tree_harness("root:user:alice").await;
        let tool = tool_over(&h).await;
        let err = tool
            .execute(json!({"action": "status", "path": "sess:/a", "timezone": "Not/AZone"}))
            .await
            .expect_err("invalid timezone must error, not fall back to local");
        assert!(err.to_string().contains("not a valid IANA tz"), "{err}");
        tool.execute(json!({"action": "status", "path": "sess:/a", "timezone": "UTC"}))
            .await
            .unwrap();
    }

    /// Sessions from two peers and two agents.
    async fn cross_peer_harness() -> Harness {
        let h = Harness::new().await;
        let alice = || ActorRef::User("alice".into());
        h.create_as("alice-1", None, alice(), "test-agent").await;
        h.create_as("alice-2", None, alice(), "other-agent").await;
        h.create_as("bob-1", None, ActorRef::User("bob".into()), "test-agent")
            .await;
        h.set_current("alice-1").await;
        h
    }

    #[tokio::test]
    async fn list_filters_by_peer_and_agent() {
        let h = cross_peer_harness().await;
        let tool = tool_over(&h).await;

        let alice = tool
            .execute(json!({"action": "list", "peer": "user:alice"}))
            .await
            .unwrap();
        let mut got = ids(&alice, "sessions", "session_id");
        got.sort();
        let mut want = vec![sid("alice-1"), sid("alice-2")];
        want.sort();
        assert_eq!(got, want, "{alice}");
        for s in alice["sessions"].as_array().unwrap() {
            assert_eq!(s["peer_type"], "user");
            assert_eq!(s["peer_id"], "alice");
        }

        let nobody = tool
            .execute(json!({"action": "list", "peer": "user:nobody"}))
            .await
            .unwrap();
        assert_eq!(nobody["total"], 0);

        let agent = tool
            .execute(json!({"action": "list", "agent_name": "test-agent"}))
            .await
            .unwrap();
        let mut got = ids(&agent, "sessions", "session_id");
        got.sort();
        let mut want = vec![sid("alice-1"), sid("bob-1")];
        want.sort();
        assert_eq!(got, want, "{agent}");
    }

    #[tokio::test]
    async fn list_rejects_removed_params_and_invalid_peers() {
        let h = cross_peer_harness().await;
        let tool = tool_over(&h).await;
        let err = tool
            .execute(json!({"action":"list", "peer":"user:alice", "kinds":["spawned"]}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("kinds"), "{err}");
        let err = tool
            .execute(json!({"action": "list", "peer": "not-a-valid-peer"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Invalid peer"), "{err}");
    }

    #[tokio::test]
    async fn find_matches_message_text_case_insensitively() {
        let h = tree_harness("root:user:alice").await;
        h.add_user_message("spawn2", "deploy the frambulator on friday")
            .await;
        let tool = tool_over(&h).await;
        let found = tool
            .execute(json!({"action": "find", "query": "FRAMBULATOR"}))
            .await
            .unwrap();
        assert_eq!(found["total"], 1, "{found}");
        assert_eq!(found["hits"][0]["session_id"], sid("spawn2"));
        assert_eq!(found["hits"][0]["role"], "user");
        assert!(found["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("frambulator"));
        let err = tool.execute(json!({"action": "find"})).await.unwrap_err();
        assert!(err.to_string().contains("query"), "{err}");
    }

    #[tokio::test]
    async fn remove_deletes_the_session() {
        let h = tree_harness("root:user:alice").await;
        let tool = tool_over(&h).await;
        let removed = tool
            .execute(json!({"action": "remove", "path": "sess:/c"}))
            .await
            .unwrap();
        assert_eq!(removed["deleted"], json!([sid("spawn2")]), "{removed}");
        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(!ids(&list, "sessions", "session_id").contains(&sid("spawn2")));
        let err = tool.execute(json!({"action": "remove"})).await.unwrap_err();
        assert!(err.to_string().contains("path"), "{err}");
    }

    /// LLM-driving actions live on the Agent tool and Sprint 7 retired
    /// the old verbs; both fail validation here.
    #[tokio::test]
    async fn demoted_actions_are_rejected() {
        let h = Harness::new().await;
        let tool = tool_over(&h).await;
        for demoted in [
            "new",
            "resume",
            "compact",
            "search",
            "rename",
            "delete",
            "branch",
            "archive",
            "unarchive",
        ] {
            let err = tool.execute(json!({"action": demoted})).await.unwrap_err();
            assert!(
                err.to_string().contains("Invalid action"),
                "{demoted}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn copy_stores_a_titled_copy_with_the_history_and_does_not_run_it() {
        let h = tree_harness("root:user:alice").await;
        h.add_user_message("spawn1", "Hello").await;
        h.add_user_message("spawn1", "Again").await;
        let tool = tool_over(&h).await;

        let copied = tool
            .execute(json!({
                "action": "copy", "path": "sess:/a", "target": "sess:/a/fork", "title": "fork"
            }))
            .await
            .unwrap();
        assert_eq!(copied["new_path"], "sess:/a/fork", "{copied}");
        let new_id = copied["new_session_id"].as_str().unwrap().to_string();
        assert_ne!(new_id, sid("spawn1"));

        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        let copy = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["session_id"] == new_id.as_str())
            .expect("copy is listed");
        assert_eq!(copy["run_active"], false);
        assert_eq!(copy["title"], "fork");
        let history = tool
            .execute(json!({"action": "history", "path": "sess:/a/fork"}))
            .await
            .unwrap();
        assert_eq!(history["total_messages"], 2, "{history}");
    }

    #[tokio::test]
    async fn move_reparents_renames_and_retitles() {
        let h = tree_harness("root:user:alice").await;
        let tool = tool_over(&h).await;

        let moved = tool
            .execute(json!({"action": "move", "path": "sess:/c", "target": "sess:/a/task-b"}))
            .await
            .unwrap();
        assert_eq!(moved["modified"], true, "{moved}");
        let status = tool
            .execute(json!({"action": "status", "path": "sess:/a/task-b"}))
            .await
            .unwrap();
        assert_eq!(status["parent_session"], sid("spawn1"), "{status}");

        // Rename in place, then retitle without changing the slug.
        tool.execute(json!({"action": "move", "path": "sess:/a/task-b", "target": "sess:/a/s2"}))
            .await
            .unwrap();
        tool.execute(json!({
            "action": "move", "path": "sess:/a/s2", "target": "sess:/a/s2", "title": "New Title"
        }))
        .await
        .unwrap();
        let status = tool
            .execute(json!({"action": "status", "path": "sess:/a/s2"}))
            .await
            .unwrap();
        assert_eq!(status["title"], "New Title", "{status}");

        let err = tool
            .execute(json!({"action": "move", "path": "sess:/missing", "target": "sess:/a/x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
        let err = tool
            .execute(json!({"action": "move", "target": "sess:/a/x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("path"), "{err}");
    }

    #[tokio::test]
    async fn move_page_limit_sets_retention() {
        let h = tree_harness("root:user:alice").await;
        let tool = tool_over(&h).await;
        let result = tool
            .execute(json!({"action": "move", "path": "sess:/a", "title": "T", "page_limit": 5}))
            .await
            .unwrap();
        assert_eq!(result["page_limit"], 5);
        assert_eq!(result["pages_pruned"], 0);
        let result = tool
            .execute(json!({"action": "move", "path": "sess:/a", "page_limit": 0}))
            .await
            .unwrap();
        assert_eq!(result["page_limit"], Value::Null);
        let err = tool
            .execute(json!({"action": "move", "path": "sess:/a", "page_limit": 10001}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("10000"), "{err}");
    }

    /// `move` with no target, title, or retention would be a silent no-op.
    #[tokio::test]
    async fn move_with_nothing_to_change_errors() {
        let h = tree_harness("root:user:alice").await;
        let err = tool_over(&h)
            .await
            .execute(json!({"action": "move", "path": "sess:/a"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("target"), "{err}");
        assert!(err.to_string().contains("title"), "{err}");
    }

    /// Each action's argument errors name exactly what is missing or
    /// malformed.
    #[tokio::test]
    async fn malformed_arguments_name_the_problem() {
        let h = tree_harness("root:user:alice").await;
        let tool = tool_over(&h).await;
        for (params, expected) in [
            (
                json!({"action": "copy", "path": "sess:/a"}),
                "Session action 'copy': missing required 'target'",
            ),
            (
                json!({"action": "move", "path": "sess:/a"}),
                "needs at least one of: 'target', 'title', 'page_limit'",
            ),
            (json!({"action": "copy", "target": "sess:/x/y"}), "path"),
            (
                json!({"action": "copy", "path": "sess:/a", "target": "a//b"}),
                "'target' is not a valid slug path",
            ),
            (
                json!({"action": "copy", "path": "sess:/a", "target": "solo"}),
                "must include a parent path and a slug segment",
            ),
            (
                json!({"action": "move", "path": "sess:/a", "target": "root:user:bob/x"}),
                "target",
            ),
            (
                json!({"action": "find"}),
                "Session action 'find': missing required 'query'",
            ),
            (
                json!({"action": "find", "query": "x", "peer": "nobody"}),
                "Invalid peer 'nobody'",
            ),
            (
                json!({"action": "read_page", "path": "sess:/a"}),
                "missing required 'page'",
            ),
            (
                json!({"action": "search_pages", "path": "sess:/a"}),
                "missing required 'query'",
            ),
            (
                json!({"action": "status", "query": "x"}),
                "'query' is not a parameter here. Accepted fields: action, path, timezone",
            ),
        ] {
            let err = tool
                .execute(params.clone())
                .await
                .expect_err(&params.to_string())
                .to_string();
            assert!(err.contains(expected), "{params}: {err}");
        }
    }

    /// Malformed `path`s are refused at the tool boundary, before the
    /// runtime touches state.
    #[tokio::test]
    async fn paths_are_validated_at_the_tool_boundary() {
        let h = tree_harness("root:user:alice").await;
        let tool = tool_over(&h).await;
        for (path, expected) in [
            ("root:user:alice", "slug"),
            ("", "path"),
            ("/", "path"),
            ("/a//b", "segment"),
            ("/ a/b", "whitespace"),
        ] {
            let err = tool
                .execute(json!({"action": "copy", "target": "sess:/destination", "path": path}))
                .await
                .expect_err(path);
            assert!(err.to_string().contains(expected), "{path:?}: {err}");
        }
    }

    /// Two pages: page 1 archived by a compaction, then the live page.
    async fn paged_harness() -> Harness {
        let h = tree_harness("spawn1").await;
        h.add_user_message("spawn1", "archived question about frambulators")
            .await;
        h.add_compaction("spawn1").await;
        h.add_user_message("spawn1", "live question").await;
        h
    }

    #[tokio::test]
    async fn list_pages_numbers_archived_and_live_pages() {
        let h = paged_harness().await;
        let pages = tool_over(&h)
            .await
            .execute(json!({"action": "list_pages", "path": "sess:/a"}))
            .await
            .unwrap();
        assert_eq!(pages["total"], 2, "{pages}");
        assert_eq!(pages["pages"][0]["page_number"], 1);
        assert_eq!(pages["pages"][0]["compaction_number"], 1);
        assert_eq!(
            pages["pages"][0]["title"],
            "archived question about frambulators"
        );
        assert_eq!(pages["pages"][1]["page_number"], 2);
        assert!(pages["pages"][1]["compaction_number"].is_null());
    }

    #[tokio::test]
    async fn read_page_renders_one_page_and_explains_unknown_ones() {
        let h = paged_harness().await;
        let tool = tool_over(&h).await;
        let page = tool
            .execute(json!({"action": "read_page", "path": "sess:/a", "page": 1}))
            .await
            .unwrap();
        let content = page["content"].as_str().unwrap();
        assert!(
            content.contains("user: archived question about frambulators"),
            "{content}"
        );
        assert!(!content.contains("live question"), "page 1 only: {content}");
        let missing = tool
            .execute(json!({"action": "read_page", "path": "sess:/a", "page": 9}))
            .await
            .unwrap();
        assert!(missing["content"]
            .as_str()
            .unwrap()
            .contains("Page 9 does not exist"));
    }

    #[tokio::test]
    async fn search_pages_finds_hits_across_pages_and_caps_them() {
        let h = paged_harness().await;
        let tool = tool_over(&h).await;
        let hits = tool
            .execute(json!({"action": "search_pages", "path": "sess:/a", "query": "QUESTION"}))
            .await
            .unwrap();
        assert_eq!(hits["total"], 2, "{hits}");
        assert_eq!(hits["hits"][0]["page_number"], 1);
        assert_eq!(hits["hits"][1]["page_number"], 2);
        let capped = tool
            .execute(json!({
                "action": "search_pages", "path": "sess:/a", "query": "question", "max_results": 1
            }))
            .await
            .unwrap();
        assert_eq!(capped["total"], 1);
    }

    /// The page actions default `path` to the current session.
    #[tokio::test]
    async fn page_actions_default_to_the_current_session() {
        let h = paged_harness().await;
        let tool = tool_over(&h).await;
        let pages = tool.execute(json!({"action": "list_pages"})).await.unwrap();
        assert_eq!(pages["total"], 2, "{pages}");
        let hits = tool
            .execute(json!({"action": "search_pages", "query": "live"}))
            .await
            .unwrap();
        assert_eq!(hits["total"], 1, "{hits}");
    }
}
