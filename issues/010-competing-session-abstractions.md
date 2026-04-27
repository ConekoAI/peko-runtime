# Issue 010: Competing Session Abstractions

**Severity:** HIGH  
**Status:** 🟡 **Open**  
**Labels:** `architecture`, `session`, `overlay`, `refactor`  
**Reported:** 2026-04-27  

---

## Summary

Three overlapping session abstractions coexist with blurred boundaries:
- `Session` (`session/unified.rs`) — JSONL persistence, message conversion, compaction
- `SessionHandle` (`session/manager.rs`) — Operations proxy + metadata
- `SessionContext` (`session/context.rs`) — Routing DTO + active operations

`SessionHandle` directly calls `base.add_user()`, leaking the abstraction. `SessionContext` accumulates responsibilities over time (DTO, router factory, operations facade). Issue 004 previously addressed the `HybridSession`/`SessionRouter` overlap, but the remaining three types still compete.

---

## The Three Abstractions

### Abstraction 1: `Session`

**Location:** `src/session/unified.rs`  
**Responsibilities:**
- JSONL persistence (load/save history)
- Message conversion (`event_to_chat_message`, `normalized_entry_to_chat_message`)
- Context text generation (`entries_to_context_text`)
- Compaction trigger
- Context cache
- Model change recording
- Session listing (static method)

**Problems:** 6+ responsibilities in one struct. Message conversion functions (~200 lines) should live in a separate module.

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

---

### Abstraction 2: `SessionHandle`

**Location:** `src/session/manager.rs`  
**Responsibilities:**
- Operations facade for an active session
- `add_user()`, `add_assistant()`, `add_tool_result()`, `load_history()`, `record_usage()`
- Overlay state access (`get_channel_state`, `set_channel_state`, `get_spawn_status`)

**Problems:** It directly acquires locks on the underlying `Session` and calls methods on it. The boundary between "handle" and "session" is thin and leaky.

```rust
pub struct SessionHandle {
    pub session_id: String,
    pub agent_name: String,
    base: Arc<RwLock<Session>>,
    overlay: Option<OverlayRef>,
}

impl SessionHandle {
    pub async fn add_user(&self, content: impl Into<String>) -> Result<()> {
        self.base.write().await.add_user(content).await
    }
    // ... more passthrough methods
}
```

---

### Abstraction 3: `SessionContext`

**Location:** `src/session/context.rs`  
**Responsibilities:**
- Routing metadata DTO (channel_type, is_subagent, is_isolated)
- Router factory (`for_channel`, `for_spawn` constructors)
- Operations facade (duplicates `SessionHandle` methods)
- Overlay state access (duplicates `SessionHandle` methods)

**Problems:** After Issue 004, `SessionContext` was supposed to be a pure DTO. But it still contains `add_user_message()`, `add_assistant_message()`, `add_tool_result()`, and `record_usage()` — all of which forward to `HybridSession` (now merged into `SessionHandle`).

```rust
pub struct SessionContext {
    pub hybrid: HybridSession,  // Now merged — should be SessionHandle
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
}

impl SessionContext {
    // These should NOT be here — use SessionHandle instead
    pub async fn add_user_message(&self, content: impl Into<String>) -> Result<()> {
        self.hybrid.base.write().await.add_user(content).await
    }
    // ... more passthrough methods
}
```

---

## Evidence of Competition

### `SessionContext` Duplicates `SessionHandle`

| Method | `SessionContext` | `SessionHandle` |
|--------|------------------|-----------------|
| `add_user_message` / `add_user` | ✅ | ✅ |
| `add_assistant_message` / `add_assistant` | ✅ | ✅ |
| `add_tool_result` | ✅ | ✅ |
| `load_history` | ✅ | ✅ |
| `record_usage` | ✅ | ✅ |
| `get_channel_state` | ✅ | ✅ |
| `set_channel_state` | ✅ | ✅ |

### Call Sites Use Both Interchangeably

```rust
// src/agent/agent.rs
let session_context = self.get_session_context(...).await?;
// Some code uses session_context.add_user_message()
// Other code uses session_handle.add_user()
```

### `SessionManager` Returns Both

```rust
// src/session/manager.rs
pub struct ResolvedSession {
    pub context: SessionContext,   // DTO + operations
    pub handle: SessionHandle,     // Operations
    pub is_new: bool,
    pub session_id: String,
}
```

Callers receive both but typically only need one. The existence of both encourages inconsistent usage.

---

## Impact

1. **Cognitive overhead:** To add a message to a session, a developer must choose between `SessionContext` and `SessionHandle`. Both work, but neither is clearly canonical.
2. **Lock contention:** `SessionContext::add_user_message()` acquires `write().await` on the base session. If the same session is accessed through `SessionHandle`, deadlocks or contention are possible.
3. **Refactoring friction:** Changing session operations requires updating both `SessionContext` and `SessionHandle`.
4. **Violation of Issue 004 resolution:** Issue 004 explicitly stated `SessionContext` should be a "pure DTO with no active operations." This was not fully achieved.

---

## Root Cause

- Issue 004 merged `HybridSession` into `SessionHandle` and eliminated `SessionRouter`, but did not fully strip `SessionContext` of its operations.
- `SessionContext` was originally designed as a "unified interface" and accumulated responsibilities over time.
- Call sites were partially migrated in Issue 004, but some still use `SessionContext` for operations.
- `ResolvedSession` returns both types to support gradual migration, but this perpetuates the duality.

---

## Proposed Resolution

**Option A: Strip `SessionContext` to pure DTO, make `SessionHandle` the sole operations facade (Recommended)**

1. **Remove ALL operations methods from `SessionContext`:**
   - `add_user_message`
   - `add_assistant_message`
   - `add_tool_result`
   - `load_history`
   - `record_usage`
   - `get_channel_state`
   - `set_channel_state`
   - `get_spawn_status`
   - `update_spawn_status`

2. **Move routing metadata to `SessionContext` as pure fields:**
   ```rust
   pub struct SessionContext {
       pub session_id: String,
       pub agent_name: String,
       pub session_key: String,
       pub channel_type: Option<ChannelType>,
       pub is_subagent: bool,
       pub is_isolated: bool,
   }
   ```

3. **Update all call sites** to use `ResolvedSession.handle` for operations and `ResolvedSession.context` for metadata only.

4. **Delete `ResolvedSession` if unnecessary** — callers can receive `(SessionContext, SessionHandle)` directly.

**Option B: Merge `SessionHandle` into `SessionContext`**

Make `SessionContext` the single abstraction for both metadata and operations. This is simpler but violates SRP — `SessionContext` would again become a god object.

**Option C: Introduce `SessionOperations` trait**

Define a trait for session operations and implement it for both `SessionContext` and `SessionHandle`. This adds abstraction without reducing complexity.

---

## Acceptance Criteria

- [ ] `SessionContext` is a pure DTO with no active operations.
- [ ] `SessionHandle` is the **sole** operations facade for sessions.
- [ ] No call site uses `SessionContext` for `add_user_message`, `load_history`, etc.
- [ ] `ResolvedSession` clearly separates `context` (read-only metadata) from `handle` (operations).
- [ ] All existing tests pass.

---

## Related

- Issue 004: Session Management — Overlapping Types (partially resolved)
- `src/session/unified.rs`
- `src/session/manager.rs`
- `src/session/context.rs`
- `src/agent/agent.rs`
- `src/agent/subagent_executor.rs`
- `src/channels/cli.rs`
- `src/tools/agent_spawn.rs`
