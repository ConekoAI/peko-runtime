# Issue 013: Consolidate `session_status` / `sessions_list` / `sessions_history` into Single Unified `session` Tool; Remove Legacy `sessions_send`

**Severity:** MEDIUM  
**Status:** 🟢 **Closed — Implemented**  
**Labels:** `architecture`, `session`, `tool-design`, `refactor`, `ux`  
**Reported:** 2026-04-30  
**Related:** Issue 012 (Unified `task` tool — closed), Issue 011 (Generalize async task status tools — closed)

---

## Summary

Issue 012 successfully consolidated `task_status` / `task_list` into a single `task` tool with an `action` parameter. This issue applies the exact same pattern to session management:

1. **Merge** `session_status`, `sessions_list`, and `sessions_history` into a single **`session`** tool with `action: status | list | history`.
2. **Remove** the legacy `sessions_send` tool — superseded by `a2a_send` (ADR-023) and no longer relevant.
3. **Implement** real `list` and `history` functionality (currently stubs/TODO) in the unified tool.

This is the natural completion of the "one registry → one tool" design principle established by Issue 012.

---

## The Problem: Tool Sprawl + Stub Code + Legacy Debt

### Current State

```
┌─────────────────────────────────────────┐
│  SessionManager (unified storage)       │
│  ├─ session:abc-123  [metadata]         │
│  ├─ session:def-456  [metadata]         │
│  └─ session:ghi-789  [metadata]         │
└─────────────────────────────────────────┘
         ▲           ▲           ▲
         │           │           │
   ┌─────┘     ┌────┘      ┌────┘
   │           │           │
┌──┴────────┐ ┌┴─────────┐ ┌┴──────────┐
│session_   │ │sessions_ │ │sessions_  │
│status     │ │list      │ │history    │
│- session_ │ │- kinds   │ │- session_ │
│  key      │ │- limit   │ │  key      │
│- timezone │ │- active_ │ │- limit     │
│           │ │  minutes │ │- include_ │
│           │ │          │ │  tools     │
└───────────┘ └──────────┘ └───────────┘
   Same SessionManager. Same data source. Different wrappers.
   list = stub (SessionCache). history = TODO (returns []).
```

Additionally, `sessions_send` is a legacy human-to-agent messaging tool that overlaps with `a2a_send` (which handles both agent-to-agent and, via StatelessAgentService, human-to-agent). It should be removed.

### Why This Matters

1. **Token overhead:** Four tool names + descriptions in LLM context. One tool replaces four.
2. **Stub code is technical debt:** `sessions_list` and `sessions_history` are registered with `SessionCache` (empty in-memory placeholder) in `BuiltinToolRegistrar`. The real `SessionIntrospector::get_history` is a `TODO` returning `vec![]`. Merging now means implementing once, correctly.
3. **No SRP violation to preserve:** `status`/`list`/`history` are query variants over the same `SessionRegistry` trait and `SessionManager` data source. SRP applies to the *registry* (data ownership) and the *tool* (LLM interface), not to every SQL-like operation.
4. **Future-proof:** `rename`, `archive`, `fork`, `clear_history` — all fit into `action` without new tool registrations.
5. **Clean mental model:** One registry → one tool. The `task` tool already proved this works.

---

## Design Principles

- **SRP:** `SessionManager` owns session data. The `session` tool owns the LLM-facing query interface. Internal helper methods (`lookup_status`, `list_sessions`, `get_history`) are implementation details, not separate tools.
- **DRY:** `SessionRegistry` trait, `SessionInfo`/`SessionStatusResult`/`HistoryMessage` projection, and filtering logic remain in one place — reused across all actions.
- **Future-proof:** New actions require only a new enum variant + a handler method. No new tool structs, no new registration blocks, no new config flags.
- **Zero tech debt:** Delete `SessionStatusTool`, `SessionsListTool`, `SessionsHistoryTool`, `SessionsSendTool` entirely. No aliases, no deprecation shims. We are at dev stage.
- **Implement real functionality:** The unified tool must provide working `list` (via `SessionManager::list_all_sessions`) and `history` (via `SessionStorage::load_events` → `event_to_llm_message` → `HistoryMessage` projection).

---

## Proposed Resolution

### 1. Unified `session` Tool Interface

```json
{
  "name": "session",
  "description": "Manage and introspect sessions: check status, list sessions, or view conversation history.",
  "parameters": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["status", "list", "history"],
        "description": "What to do: 'status' (get one session), 'list' (query sessions), 'history' (get messages)"
      },
      "session_key": {
        "type": "string",
        "description": "Required for 'status' and 'history'. Session ID or key. Defaults to current session for 'status' when omitted."
      },
      "kinds": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional filter for 'list': e.g., ['main', 'spawned', 'cron']"
      },
      "limit": {
        "type": "integer",
        "default": 50,
        "description": "Max results for 'list' (sessions) or 'history' (messages)"
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
  }
}
```

### 2. Response Shapes (per action)

| Action | Shape |
|--------|-------|
| `status` | `SessionStatusResult` JSON — same fields as current `session_status` output |
| `list` | `{ total, sessions: [SessionInfo…] }` — same as current `sessions_list` output |
| `history` | `{ session_key, total_messages, messages: [HistoryMessage…] }` — same as current `sessions_history` output |

### 3. Real Implementation of `list` and `history`

#### `list` action

The `SessionRegistry::list_sessions` method currently delegates to `SessionIntrospector::list_sessions`, which calls `SessionManager::list_all_sessions(false)`. This already works — it returns real `SessionMetadata` from the `MetadataController`. The `SessionTool` will:

1. Call `registry.list_sessions(kinds, limit, active_minutes).await`
2. Map `SessionMetadata` → `SessionInfo`
3. Apply `active_minutes` filter (if provided) by comparing `updated_at` against current time
4. Truncate to `limit`
5. Return `{ total, sessions }`

#### `history` action

The `SessionRegistry::get_history` method is currently a TODO stub. Implement it properly:

1. **Data layer:** Add `SessionManager::load_session_history(session_id: &str, limit: usize) -> Result<Vec<LlmMessage>>` that:
   - Opens the session via `open_session()` or uses `SessionStorage::load_events()` directly
   - Returns `Vec<LlmMessage>` via `Session::load_history()`
2. **Registry layer:** `SessionIntrospector::get_history` calls `session_manager.read().await.load_session_history(session_key, limit).await`
3. **Tool layer:** `SessionTool` maps `LlmMessage` → `HistoryMessage`, filtering out tool calls/results when `include_tools: false`

**Mapping `LlmMessage` → `HistoryMessage`:**

```rust
fn llm_message_to_history(msg: &LlmMessage, include_tools: bool) -> Option<HistoryMessage> {
    let role = msg.role.as_str().to_string();
    let content = msg.content.iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::Thinking { text, .. } if include_tools => Some(format!("[thinking] {text}")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let (tool_calls, tool_results) = if include_tools {
        let calls: Vec<_> = msg.content.iter().filter_map(|block| match block {
            ContentBlock::ToolCall { id, name, arguments } => Some(ToolCallInfo { id: id.clone(), name: name.clone(), arguments: arguments.clone() }),
            _ => None,
        }).collect();
        let results: Vec<_> = msg.content.iter().filter_map(|block| match block {
            ContentBlock::ToolResult { tool_call_id, name, content, is_error } => Some(ToolResultInfo {
                tool_call_id: tool_call_id.clone(),
                success: !is_error,
                result: Some(serde_json::json!({ "name": name, "content": content.iter().filter_map(|c| match c { ContentBlock::Text { text } => Some(text.clone()), _ => None }).collect::<Vec<_>>() })),
                error: if *is_error { Some("Tool execution failed".to_string()) } else { None },
            }),
            _ => None,
        }).collect();
        (if calls.is_empty() { None } else { Some(calls) }, if results.is_empty() { None } else { Some(results) })
    } else {
        (None, None)
    };

    Some(HistoryMessage {
        role,
        content,
        tool_calls,
        tool_results,
        timestamp: msg.timestamp.to_rfc3339(),
    })
}
```

### 4. `SessionTool` Architecture

```rust
//! Unified Session Management Tool
//!
//! Provides `session` — a single tool for introspecting ANY session.
//! Replaces `session_status`, `sessions_list`, `sessions_history` (Issue 013).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::tools::session_introspection::{
    HistoryMessage, SessionInfo, SessionRegistry, SessionStatusResult, UsageStats,
};
use crate::tools::Tool;

// ------------------------------------------------------------------------------
// SessionAction — serde-driven, extensible
// ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionAction {
    Status,
    List,
    History,
}

// ------------------------------------------------------------------------------
// SessionTool — unified interface
// ------------------------------------------------------------------------------

pub struct SessionTool {
    registry: Box<dyn SessionRegistry>,
}

impl SessionTool {
    #[must_use]
    pub fn new(registry: Box<dyn SessionRegistry>) -> Self {
        Self { registry }
    }

    // Internal helpers — DRY across all actions
    async fn get_status(&self, session_key: Option<&str>) -> anyhow::Result<SessionStatusResult> {
        let session_id = session_key
            .map(String::from)
            .unwrap_or_else(|| self.registry.current_session_key());
        self.registry.get_status(&session_id).await
    }

    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        limit: usize,
        active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        self.registry.list_sessions(kinds, limit, active_minutes).await
    }

    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        self.registry.get_history(session_key, limit, include_tools).await
    }

    // Response builders — pure functions, keep execute() readable
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
- limit: Optional — max results (default: 50)
- active_minutes: Optional for 'list' — only sessions active in last N minutes
- include_tools: Optional for 'history' — include tool calls/results (default: true)
- timezone: Optional for 'status' — timezone for timestamp formatting

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
                let mut status = self.get_status(session_key).await?;

                // Add current timestamps
                let now_utc = chrono::Utc::now();
                status.timestamp_utc = now_utc.to_rfc3339();
                status.timestamp = if let Some(tz_str) = timezone {
                    match tz_str.parse::<chrono_tz::Tz>() {
                        Ok(tz) => now_utc.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S %Z").to_string(),
                        Err(_) => chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z").to_string(),
                    }
                } else {
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z").to_string()
                };

                Ok(Self::build_status_response(&status))
            }
            SessionAction::List => {
                let kinds: Option<Vec<String>> = params
                    .get("kinds")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let active_minutes = params.get("active_minutes").and_then(|v| v.as_i64());

                let kinds_ref = kinds.as_deref();
                let sessions = self.list_sessions(kinds_ref, limit, active_minutes).await?;
                Ok(Self::build_list_response(sessions))
            }
            SessionAction::History => {
                let session_key = params
                    .get("session_key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'history' action requires 'session_key'"))?;
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
                let include_tools = params.get("include_tools").and_then(|v| v.as_bool()).unwrap_or(true);

                let messages = self.get_history(session_key, limit, include_tools).await?;
                Ok(Self::build_history_response(session_key, messages))
            }
        }
    }
}
```

### 5. `SessionIntrospector` Enhancements

#### Implement `get_history` properly

```rust
async fn get_history(
    &self,
    session_key: &str,
    limit: usize,
    include_tools: bool,
) -> anyhow::Result<Vec<HistoryMessage>> {
    let manager = self.session_manager.read().await;

    // Try to open the session to get a handle, then load history
    let messages = if let Ok(Some(handle)) = manager.open_session(session_key).await {
        let llm_messages = handle.load_history().await?;
        llm_messages
            .iter()
            .filter_map(|m| llm_message_to_history(m, include_tools))
            .take(limit)
            .collect()
    } else {
        // Fallback: try loading directly from storage
        let storage = SessionStorage::new(manager.directory().path());
        let events = storage.load_events(session_key).await?;
        events.iter()
            .filter_map(event_to_history_message)
            .take(limit)
            .collect()
    };

    Ok(messages)
}
```

#### Implement `list_sessions` with `active_minutes` filtering

```rust
async fn list_sessions(
    &self,
    kinds: Option<&[String]>,
    limit: usize,
    active_minutes: Option<i64>,
) -> anyhow::Result<Vec<SessionInfo>> {
    let mut manager = self.session_manager.write().await;
    let metadatas = manager.list_all_sessions(false).await?;

    let now = chrono::Utc::now().timestamp_millis() as u64;
    let cutoff_ms = active_minutes.map(|m| now - (m as u64 * 60 * 1000));

    let sessions: Vec<SessionInfo> = metadatas
        .into_iter()
        .filter(|m| {
            // Filter by kinds
            let kind_match = kinds.map_or(true, |k| k.contains(&m.trigger));
            // Filter by active_minutes
            let active_match = cutoff_ms.map_or(true, |cutoff| m.updated_at >= cutoff);
            kind_match && active_match
        })
        .take(limit)
        .map(|m| SessionInfo {
            session_key: m.session_id.clone(),
            session_id: m.session_id,
            kind: m.trigger,
            agent_id: Some(m.agent_name),
            label: m.title,
            created_at: chrono::DateTime::from_timestamp_millis(m.created_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            last_activity: chrono::DateTime::from_timestamp_millis(m.updated_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            message_count: m.message_count,
            is_active: true,
        })
        .collect();

    Ok(sessions)
}
```

### 6. Remove `sessions_send`

Delete:
- `src/tools/sessions_send.rs` (entire file)
- All references in `src/tools/mod.rs`
- All references in `src/tools/builtin_registry.rs`
- All references in `src/agent/agent.rs`
- Update `src/tools/a2a_send.rs` description to remove reference to `sessions_send`

---

## Implementation Log

| Date | Milestone |
|------|-----------|
| 2026-04-30 | Issue filed with detailed design |
| 2026-04-30 | Implementation complete — all files updated, `cargo test --lib`: 904 passed, 0 failed, 19 ignored |

## Implementation Plan

### Phase 1: Data Layer — Implement Real `history` and Enhanced `list`

| File | Action | Description |
|------|--------|-------------|
| `src/session/manager.rs` | **Add** | Add `load_session_history(session_id, limit) -> Result<Vec<LlmMessage>>` convenience method |
| `src/tools/session_introspection.rs` | **Modify** | Implement `SessionIntrospector::get_history` using `SessionManager::load_session_history` or `SessionStorage::load_events` fallback |
| `src/tools/session_introspection.rs` | **Modify** | Enhance `SessionIntrospector::list_sessions` with `active_minutes` filtering |

### Phase 2: Tool Layer — Create Unified `SessionTool`

| File | Action | Description |
|------|--------|-------------|
| `src/tools/session_introspection.rs` | **Rewrite** | Delete `SessionStatusTool`, `SessionsListTool`, `SessionsHistoryTool`. Introduce single `SessionTool` with `SessionAction` enum and response builders |
| `src/tools/session_introspection.rs` | **Add** | Add `llm_message_to_history` helper for `LlmMessage` → `HistoryMessage` mapping |

### Phase 3: Registration Layer — Update All Call Sites

| File | Action | Description |
|------|--------|-------------|
| `src/tools/mod.rs` | **Modify** | Replace re-exports: `SessionTool` only. Remove `SessionsSendTool` re-export |
| `src/tools/builtin_registry.rs` | **Modify** | Replace three session tool registrations with one `SessionTool::new(Box::new(SessionCache::new("main")))`. Remove `sessions_send` from `all_tool_names()` and `is_agent_specific_builtin()` |
| `src/agent/agent.rs` | **Modify** | Replace `SessionStatusTool::new(...)` with `SessionTool::new(Box::new(session_registry))`. Remove `SessionsSendTool` registration |
| `src/types/agent.rs` | **Modify** | Update `ExtensionConfig::default()` whitelist: `"session"` replaces `"session_status"`, `"sessions_list"`, `"sessions_history"`, `"sessions_send"` |
| `src/tools/a2a_send.rs` | **Modify** | Update description to remove `sessions_send` reference |

### Phase 4: Cleanup — Delete Legacy Code

| File | Action | Description |
|------|--------|-------------|
| `src/tools/sessions_send.rs` | **Delete** | Entire file — legacy tool |
| `src/tools/session_introspection.rs` | **Modify** | Delete `SessionStatusTool`, `SessionsListTool`, `SessionsHistoryTool` structs and their `Tool` impls (already covered in Phase 2) |
| `src/tools/session_introspection.rs` | **Modify** | Delete `SessionsListArgs`, `SessionsHistoryArgs`, `SessionStatusArgs` if no longer needed (or keep for `SessionTool` internal use) |

### Phase 5: Tests — Update and Add

| File | Action | Description |
|------|--------|-------------|
| `src/tools/session_introspection.rs` | **Rewrite tests** | Update existing tests to use `SessionTool`. Add `test_session_history_real` that creates a session, adds messages, then queries history |
| `src/daemon/state.rs` | **Modify** | Update `test_appstate_has_registered_tools` to assert `"session"` tool registered |
| `e2e_tests/extensions/tools/built-in/session_status/` | **Rename + modify** | Rename to `session/`, update tests to use `session` tool with `action` parameter |

---

## File Changes Summary

| File | Action | Description |
|------|--------|-------------|
| `src/session/manager.rs` | **Add** | `load_session_history` convenience method |
| `src/tools/session_introspection.rs` | **Rewrite** | Unified `SessionTool`, real `get_history`, enhanced `list_sessions`, response builders |
| `src/tools/sessions_send.rs` | **Delete** | Legacy tool — superseded by `a2a_send` |
| `src/tools/mod.rs` | **Modify** | Re-exports: `SessionTool` only |
| `src/tools/builtin_registry.rs` | **Modify** | Register single `session` tool; remove `sessions_send`; update `all_tool_names()` and `is_agent_specific_builtin()` |
| `src/agent/agent.rs` | **Modify** | Register `SessionTool` instead of `SessionStatusTool` + `SessionsSendTool` |
| `src/types/agent.rs` | **Modify** | Update whitelist: `"session"` replaces four old names |
| `src/tools/a2a_send.rs` | **Modify** | Remove `sessions_send` reference from description |
| `src/daemon/state.rs` | **Modify** | Update test assertions |
| `e2e_tests/extensions/tools/built-in/session_status/` | **Rename + modify** | Use unified `session` tool |

---

## Acceptance Criteria

- [ ] `SessionStatusTool`, `SessionsListTool`, `SessionsHistoryTool`, `SessionsSendTool` structs **completely removed** from codebase
- [ ] Single `SessionTool` registered as `"session"` handles `status`, `list`, and `history` actions
- [ ] `session` tool `list` action returns **real sessions** from `SessionManager::list_all_sessions` (not stub `SessionCache`)
- [ ] `session` tool `history` action returns **real conversation history** from session JSONL storage
- [ ] `session` tool `status` action defaults to current session when `session_key` omitted (agent-specific registration)
- [ ] `sessions_send` tool **completely removed** from all registration paths, imports, and references
- [ ] `a2a_send` description no longer references `sessions_send`
- [ ] `BuiltinToolRegistrar` registers exactly one session tool (not three)
- [ ] `BuiltinToolRegistrar::all_tool_names()` updated
- [ ] `BuiltinToolRegistrar::is_agent_specific_builtin()` updated
- [ ] `ExtensionConfig::default()` whitelist updated
- [ ] `Agent::init_builtins_async()` registers `SessionTool` + removes `SessionsSendTool`
- [ ] E2E tests updated and passing
- [ ] All existing unit tests pass (905 passed, 0 failed, 19 ignored baseline)

---

## Why Not Keep `session_status` + `sessions_list` + `sessions_history`?

| Argument | Counter |
|----------|---------|
| "LLMs handle discrete tools better" | Only for *unrelated* domains. `status`/`list`/`history` are query variants over the same `SessionRegistry`. An `action` enum is *easier* for an LLM than three names to remember. |
| "Parameter schema gets messy" | Not if structured cleanly. `action` drives which other params are relevant. This is standard REST/API design. |
| "Three tools is already clean enough" | It leaves no home for `rename`, `archive`, `fork` without more tools. The trajectory is toward N tools for N operations. One tool scales better. |

---

## Related

- Issue 012: Consolidate `task_status` / `task_list` into Single Unified `task` Tool (closed)
- Issue 011: Generalize async task status tools (closed)
- `src/tools/session_introspection.rs`
- `src/session/manager.rs`
- `src/tools/builtin_registry.rs`
- `src/agent/agent.rs`
