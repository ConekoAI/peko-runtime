# Issue 004: Session Management — Overlapping Types

**Severity:** HIGH  
**Status:** Resolved  
**Labels:** `architecture`, `session`, `overlay`, `refactor`  
**Reported:** 2026-04-21  
**Assigned:** Kimi Code CLI  
**Resolved:** 2026-04-22  

---

## Summary

The session module is powerful but contains too many overlapping types that blur responsibility boundaries. `SessionManager` handles creation, `SessionRouter` handles routing, `SessionContext` is both a DTO and an active operations handle, and `HybridSession` is a thin wrapper around `Session` + overlay. The result is a developer-unfriendly API where simple operations require navigating 4+ types.

---

## Types Involved

| Type | Module | Responsibility | Lines |
|------|--------|---------------|-------|
| `Session` | `session::unified` | Core session: storage, history, messages, metadata | ~300 |
| `HybridSession` | `session::manager` | Base `Session` + `OverlayRef` (channel or spawn) | ~100 |
| `SessionContext` | `session::context` | DTO + routing + message operations + overlay state | ~424 |
| `SessionManager` | `session::manager` | Lifecycle: create, open, cache, branch, delete | ~600+ |
| `SessionRouter` | `session::context` | Routes messages to appropriate session | ~60 |
| `SessionService` | `common::services` | High-level: list, history, branching, deletion | ~200+ |
| `SessionHandle` | `session::manager` | Opaque handle for session operations | ~80 |

---

## Evidence

### `Session` — The core type

**`src/session/unified.rs` (lines 98–108):**
```rust
pub struct Session {
    pub id: String,
    pub agent_name: String,
    pub session_key: String,
    pub peer: Peer,
    storage: SessionStorage,
    // ... history, metadata, etc.
}
```

### `HybridSession` — Base + overlay

**`src/session/manager.rs` (lines 116–197):**
```rust
pub struct HybridSession {
    pub base: Arc<RwLock<Session>>,
    pub overlay: OverlayRef,  // Channel | Spawn | None
}

impl HybridSession {
    pub fn new(base: Arc<RwLock<Session>>, overlay: OverlayRef) -> Self { ... }
    pub fn base_only(base: Arc<RwLock<Session>>) -> Self { ... }
    pub async fn base_session_key(&self) -> String { ... }
    pub async fn full_session_key(&self) -> String { ... }
    pub async fn peer(&self) -> Peer { ... }
    pub async fn channel_type(&self) -> Option<ChannelType> { ... }
    pub async fn is_isolated_spawn(&self) -> bool { ... }
}
```

`HybridSession` is essentially a tuple with convenience methods. Most of its methods just `read().await` on the base `Session`.

### `SessionContext` — DTO + operations + routing

**`src/session/context.rs` (lines 17–219):**
```rust
pub struct SessionContext {
    pub hybrid: HybridSession,
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
}

impl SessionContext {
    // DTO constructors
    pub async fn new(hybrid: HybridSession) -> Self { ... }
    pub async fn for_channel(manager: &Arc<RwLock<SessionManager>>, agent: &str, peer: &Peer, channel_type: ChannelType, channel_id: &str) -> Result<Self> { ... }
    pub async fn for_spawn(manager: &Arc<RwLock<SessionManager>>, agent: &str, peer: &Peer, task: &str, isolated: bool, parent_session_key: &str, timeout_seconds: Option<u64>) -> Result<Self> { ... }

    // Passthrough to HybridSession
    pub async fn base_session_key(&self) -> String { self.hybrid.base_session_key().await }
    pub async fn full_session_key(&self) -> String { self.hybrid.full_session_key().await }
    pub async fn peer(&self) -> Peer { self.hybrid.peer().await }
    pub async fn agent_name(&self) -> String { self.hybrid.base.read().await.agent_name.clone() }
    pub async fn is_isolated(&self) -> bool { self.hybrid.is_isolated_spawn().await }

    // Active operations (delegates to Session via HybridSession.base)
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> { self.hybrid.base.read().await.load_history().await }
    pub async fn add_user_message(&self, content: impl Into<String>) -> Result<()> { self.hybrid.base.write().await.add_user(content).await }
    pub async fn add_assistant_message(&self, content: impl Into<String>, tool_calls: Option<Vec<ToolCall>>, usage: Option<TokenUsage>) -> Result<()> { self.hybrid.base.write().await.add_assistant(content, tool_calls, usage).await }
    pub async fn add_tool_result(&self, tool_call_id: impl Into<String>, tool_name: impl Into<String>, result: impl Into<String>) -> Result<()> { self.hybrid.base.write().await.add_tool_result(tool_call_id, tool_name, result).await }
    pub async fn record_usage(&self, context_window: usize, input_tokens: usize, output_tokens: usize) -> Result<()> { self.hybrid.base.write().await.record_usage(...); Ok(()) }

    // Overlay-specific state
    pub async fn get_channel_state(&self, key: &str) -> Option<Value> { ... }
    pub async fn set_channel_state(&self, key: impl Into<String>, value: Value) -> bool { ... }
    pub async fn get_spawn_status(&self) -> Option<SpawnStatus> { ... }
    pub async fn update_spawn_status<F>(&self, f: F) -> bool { ... }
}
```

`SessionContext` is doing three jobs:
1. **DTO:** Holds `HybridSession` + `channel_type` + `is_subagent`
2. **Router factory:** `for_channel`, `for_spawn` construct sessions via `SessionManager`
3. **Operations facade:** `add_user_message`, `add_assistant_message`, `add_tool_result`, `record_usage`

### `SessionRouter` — Thin wrapper around `SessionContext::for_channel`

**`src/session/context.rs` (lines 224–289):**
```rust
pub struct SessionRouter {
    manager: Arc<RwLock<SessionManager>>,
    default_agent: String,
}

impl SessionRouter {
    pub async fn route(&self, peer: &Peer, channel_type: ChannelType, channel_id: &str, agent: Option<&str>) -> Result<SessionContext> {
        SessionContext::for_channel(&self.manager, agent_name, peer, channel_type, channel_id).await
    }
    pub async fn route_to_agent(&self, agent: &str, peer: &Peer, channel_type: ChannelType, channel_id: &str) -> Result<SessionContext> { ... }
    pub async fn spawn(&self, agent: &str, peer: &Peer, task: &str, isolated: bool, parent_session_key: &str, timeout_seconds: Option<u64>) -> Result<SessionContext> { ... }
}
```

`SessionRouter` just delegates to `SessionContext` constructors. It adds no unique behavior.

### `SessionService` — High-level operations

**`src/common/services/session_service.rs` (lines 1–100+):**
```rust
pub struct SessionService {
    path_resolver: PathResolver,
}

// Provides: list_sessions, get_session_history, branch_session, delete_session, etc.
```

This is a separate service layer that operates on sessions but is not part of the `session` module.

---

## Impact

1. **Cognitive overhead:** To add a message to a session, a developer must understand `SessionContext` → `HybridSession` → `Arc<RwLock<Session>>` → `Session::add_user()`.
2. **Lock contention:** `SessionContext::add_user_message()` acquires `write().await` on the base session. If the same session is accessed through a different `SessionContext`, deadlocks or contention are possible.
3. **Redundant passthrough methods:** `SessionContext` has ~15 methods that just forward to `HybridSession` or `Session`.
4. **Router/Context confusion:** `SessionRouter` creates `SessionContext`, but `SessionContext` also has `for_channel` and `for_spawn` constructors. Which is the entry point?
5. **Testing difficulty:** Constructing a `SessionContext` for tests requires a `SessionManager`, `HybridSession`, and `Arc<RwLock<Session>>`.

---

## Root Cause

- The overlay architecture (base session + channel/spawn overlays) was introduced to support cross-channel context sharing and subagent isolation.
- `HybridSession` was created to represent the base+overlay combination.
- `SessionContext` was added as a "unified interface" but accumulated responsibilities over time.
- `SessionRouter` was added for message routing but overlaps with `SessionContext::for_channel`.
- `SessionService` was added in `common::services` to provide CLI/API operations, separate from the core session module.

---

## Selected Resolution: Modified Option A

After reviewing all source files and call sites, the selected approach merges `HybridSession` into `SessionHandle`, eliminates `SessionRouter`, and reduces `SessionContext` to a pure DTO. `SessionHandle` already exists as a well-designed operations facade — it just needs overlay awareness.

### New Architecture

```
┌─────────────────────────────────────┐
│         SessionManager              │
│  (lifecycle, routing, caching)      │
├─────────────────────────────────────┤
│  resolve_session() → ResolvedSession│
│  create_session()  → SessionHandle  │
│  open_session()    → SessionHandle  │
│  route()           → ResolvedSession│
│  spawn_session()   → ResolvedSession│
└─────────────────────────────────────┘
              │
              ▼
┌─────────────────────────────────────┐
│         ResolvedSession             │
│  ├─ context: SessionContext (DTO)   │
│  └─ handle: SessionHandle (ops)     │
└─────────────────────────────────────┘
              │
    ┌─────────┴─────────┐
    ▼                   ▼
┌─────────┐      ┌─────────────┐
│Session  │      │ SessionHandle│
│Context  │      │ (operations) │
│(DTO)    │      │              │
│         │      │ add_user()   │
│channel_ │      │ add_assistant│
│type     │      │ load_history │
│is_subagent     │ record_usage │
│is_isolated     │ get/set state│
└─────────┘      └─────────────┘
                          │
                          ▼
                   ┌─────────────┐
                   │   Session   │
                   │ (internal)  │
                   │ + overlay   │
                   └─────────────┘
```

### Public API Surface (3 Types)

| Type | Module | Responsibility |
|------|--------|---------------|
| `SessionManager` | `session::manager` | Lifecycle, routing, resolution |
| `SessionHandle` | `session::manager` | Operations on an active session |
| `SessionContext` | `session::context` | Pure DTO with routing metadata |

`SessionService` (renamed `SessionQueryService`) lives in `session::service` for read-only queries.

### Detailed Changes

#### 1. Merge `HybridSession` into `Session` and `SessionHandle`

**`src/session/unified.rs`:**
- Add `overlay: Option<OverlayRef>` field to `Session`
- Move `HybridSession`'s methods (`full_session_key`, `is_isolated_spawn`, `channel_type`) to `Session`
- Delete `HybridSession`

**`src/session/manager.rs`:**
- Update `SessionHandle` to hold `overlay: Option<OverlayRef>` directly
- Add overlay-aware methods to `SessionHandle`:
  - `full_session_key()`
  - `is_isolated()`
  - `channel_type()`
  - `get_channel_state()` / `set_channel_state()`
  - `get_spawn_status()` / `update_spawn_status()`

#### 2. Reduce `SessionContext` to a Pure DTO

**`src/session/context.rs`:**
```rust
/// Lightweight context for agent execution — pure DTO, no operations
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: String,
    pub agent_name: String,
    pub session_key: String,
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
    pub is_isolated: bool,
}
```

- Remove ALL message operations (`add_user_message`, `add_assistant_message`, etc.)
- Remove `for_channel` and `for_spawn` constructors
- `SessionContext` is constructed by `SessionManager` only, from resolved session metadata

#### 3. Delete `SessionRouter`, Merge into `SessionManager`

**`src/session/context.rs`:**
- Delete `SessionRouter` entirely

**`src/session/manager.rs`:**
- Add routing methods directly to `SessionManager`:
  - `route()`
  - `route_to_agent()`
  - `spawn_session()`
- Update `ResolvedSession` to contain both `context` (DTO) and `handle` (ops):
  ```rust
  pub struct ResolvedSession {
      pub context: SessionContext,
      pub handle: SessionHandle,
      pub is_new: bool,
      pub session_id: String,
  }
  ```

#### 4. Update `Agent` and Call Sites

**`src/agent/agent.rs`:**
- Replace `session_router: SessionRouter` with direct `SessionManager` methods
- `get_session_context()` returns `ResolvedSession`; callers use `.handle` for ops, `.context` for metadata

**`src/channels/cli.rs`, `src/tools/agent_spawn.rs`, etc.:**
- Use `SessionHandle` for `record_usage()` instead of `SessionContext`
- Use `SessionContext` as DTO only (for routing info)

#### 5. Evaluate `SessionService`

**`src/common/services/session_service.rs`:**
- Move to `src/session/service.rs` and rename to `SessionQueryService`
- Boundary: `SessionManager` = active session lifecycle + routing; `SessionQueryService` = read-only queries for CLI/API

### Files to Modify

| File | Changes |
|------|---------|
| `src/session/unified.rs` | Add `overlay` field; merge `HybridSession` methods |
| `src/session/manager.rs` | Merge `SessionRouter`; expand `SessionHandle`; update `ResolvedSession`; delete `HybridSession` |
| `src/session/context.rs` | Strip to pure DTO; delete `SessionRouter` |
| `src/session/mod.rs` | Update re-exports |
| `src/agent/agent.rs` | Replace `SessionRouter` usage with `SessionManager` |
| `src/agent/subagent_executor.rs` | Use `ResolvedSession.handle` for ops, `.context` for metadata |
| `src/channels/cli.rs` | Use `SessionHandle` for `record_usage` instead of `SessionContext` |
| `src/tools/agent_spawn.rs` | Same as above |
| `src/agent/announcement_service.rs` | Use `SessionContext` as DTO only |
| `src/common/services/session_service.rs` | Move to `src/session/service.rs` (optional, separate PR) |

### Migration Strategy

1. **Phase 1**: Add `overlay` to `Session`, add methods to `SessionHandle`, create new `SessionContext` DTO ✅
2. **Phase 2**: Update `SessionManager` routing methods to return `ResolvedSession` with both `context` + `handle` ✅
3. **Phase 3**: Update all call sites in `agent/`, `channels/`, `tools/` to use `.handle` for operations ✅
4. **Phase 4**: Delete `HybridSession`, delete old `SessionContext` methods, delete `SessionRouter` ✅
5. **Phase 5**: Move `SessionService` to `session::service` (optional — deferred to future PR)

---

## Acceptance Criteria

- [x] There are at most 3 public session types (`SessionManager`, `SessionHandle`, `SessionContext`).
- [x] `SessionContext` is a pure DTO with no active operations.
- [x] `SessionRouter` is deleted; routing lives in `SessionManager`.
- [x] `HybridSession` is deleted; overlay awareness lives in `Session`/`SessionHandle`.
- [x] Adding a message to a session requires at most one lock acquisition and one method call.
- [x] `SessionService` in `common::services` is evaluated for merge into `session` module or kept with a clear boundary.

---

## Related

- `src/session/unified.rs`
- `src/session/manager.rs`
- `src/session/context.rs`
- `src/common/services/session_service.rs`
- `docs/session_overlay_architecture.md`
- `docs/SESSION_MERGER_PLAN.md`
