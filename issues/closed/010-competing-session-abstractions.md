# Issue 010: Competing Session Abstractions

**Severity:** HIGH  
**Status:** 🟢 **Closed** (2026-04-28)  
**Labels:** `architecture`, `session`, `overlay`, `refactor`  
**Reported:** 2026-04-27  
**Closed:** 2026-04-28  
**Commit:** TBD  

---

## Summary

Three overlapping session abstractions coexisted with blurred boundaries:
- `Session` (`session/unified.rs`) — JSONL persistence, message conversion, compaction
- `SessionHandle` (`session/manager.rs`) — Operations proxy + metadata
- `SessionContext` (`session/context.rs`) — Routing DTO + active operations

`SessionHandle` directly called `base.add_user()`, leaking the abstraction. `SessionContext` accumulated responsibilities over time (DTO, router factory, operations facade). Issue 004 previously addressed the `HybridSession`/`SessionRouter` overlap, but the remaining three types still competed.

---

## Post-Resolution Architecture

### Three Abstractions with Crystal-Clear Boundaries

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                           SESSION ABSTRACTION STACK                          │
├─────────────────────────────────────────────────────────────────────────────┤
│  Layer 3 (External API)                                                      │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ ResolvedSession { context: SessionContext, handle: SessionHandle }  │   │
│  │  • context → read-only metadata DTO                                 │   │
│  │  • handle  → sole operations facade                                 │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────────────────────────┤
│  Layer 2 (Operations)                                                        │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ SessionHandle                                                       │   │
│  │  • add_user(), add_assistant(), add_tool_result()                   │   │
│  │  • load_history(), record_usage(), sync_metadata()                  │   │
│  │  • get_channel_state(), set_channel_state()                         │   │
│  │  • get_spawn_status(), update_spawn_status()                        │   │
│  │  • Delegates persistence to Session (internal)                      │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────────────────────────┤
│  Layer 1 (Persistence)                                                       │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ Session (internal implementation detail)                            │   │
│  │  • JSONL read/write (SessionStorage)                                │   │
│  │  • Message formatting / event construction                          │   │
│  │  • Context building with compaction awareness                       │   │
│  │  • Derived context cache management                                 │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────────────────────────┤
│  Layer 0 (Pure Functions)                                                    │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ message_conversion (new module)                                     │   │
│  │  • event_to_llm_message()                                           │   │
│  │  • normalized_entry_to_llm_message()                                │   │
│  │  • entries_to_context_text()                                        │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Responsibility Matrix

| Concern | Owner | Via |
|---------|-------|-----|
| Session lifecycle (create, open, cache) | `SessionManager` | Direct methods |
| Session resolution (route, auto-resume) | `SessionManager` | Returns `ResolvedSession` |
| Add messages, load history | `SessionHandle` | `handle.add_*()`, `handle.load_history()` |
| Token usage recording | `SessionHandle` | `handle.record_usage()` |
| Overlay state (channel, spawn) | `SessionHandle` | `handle.get/set_channel_state()` |
| Routing metadata (read-only) | `SessionContext` | `resolved.context.*` fields |
| Message format conversion | `message_conversion` | Pure functions, no state |
| JSONL persistence | `Session` (internal) | Called only by `SessionHandle` |

---

## Changes Made

### 1. Extracted `message_conversion` Module (SRP, DRY)

**File:** `src/session/message_conversion.rs` (new)

Moved three pure conversion functions out of `Session` (which had 6+ responsibilities):

- `event_to_llm_message()` — `SessionEvent` → `LlmMessage`
- `normalized_entry_to_llm_message()` — `NormalizedEntry` → `LlmMessage`
- `entries_to_context_text()` — `[NormalizedEntry]` → context text

These are **stateless, deterministic, and testable in isolation**. They belong in their own module because:
- They have no dependencies on `Session` state
- They are used by both `Session::build_context()` and external test utilities
- Extracting them reduces `Session` from 6+ responsibilities to its core persistence role

**Before:** `Session` owned message conversion, context building, caching, compaction, model changes, and static listing.  
**After:** `Session` owns persistence + context building only. Conversion is delegated to `message_conversion`.

### 2. Refactored `Agent` API to Expose `ResolvedSession` Directly

**Files:** `src/agent/agent.rs`, `src/channels/cli.rs`

Eliminated the double-lookup anti-pattern where callers:
1. Called `agent.get_session_context()` to get a `SessionContext`
2. Then called `manager.open_session(ctx.session_id)` to get a `SessionHandle`

**Before:**
```rust
// cli.rs — TWO lookups, potential race condition
let ctx = agent.get_default_session_context().await?;
let handle = manager.open_session(&ctx.session_id).await?;
```

**After:**
```rust
// cli.rs — ONE atomic resolution
let resolved = agent.resolve_default_session().await?;
let handle = resolved.handle;  // handle + context returned together
```

Renamed methods for clarity:
- `get_session_context()` → `resolve_session()` (returns `ResolvedSession`)
- `get_default_session_context()` → `resolve_default_session()` (returns `ResolvedSession`)
- `spawn_session()` unchanged (already returned `ResolvedSession` via `SessionManager`)

### 3. Enhanced `ResolvedSession` Documentation

**File:** `src/session/manager.rs`

Added comprehensive rustdoc explaining the separation of concerns:
- `context` is explicitly documented as "read-only metadata DTO — no operations"
- `handle` is explicitly documented as "operations facade — the SOLE authority for session mutations"
- Included usage example showing correct access patterns

### 4. Verified `SessionContext` is Pure DTO

**File:** `src/session/context.rs`

Confirmed `SessionContext` contains **only** fields and a constructor:
- `session_id`, `agent_name`, `session_key`, `full_session_key`
- `peer`, `channel_type`, `is_subagent`, `is_isolated`

No operation methods remain. No `HybridSession` field. The type is a true DTO.

### 5. Verified `SessionHandle` is Sole Operations Facade

**File:** `src/session/manager.rs`

Confirmed `SessionHandle` owns **all** session operations:
- `add_user()`, `add_assistant()`, `add_tool_result()`
- `load_history()`, `get_context_text()`
- `record_usage()`, `sync_metadata()`, `get_metadata()`
- `get_channel_state()`, `set_channel_state()`
- `get_spawn_status()`, `update_spawn_status()`

No other type exposes these capabilities.

---

## Files Modified

| File | Change |
|------|--------|
| `src/session/message_conversion.rs` | **New** — extracted pure conversion functions |
| `src/session/mod.rs` | Added `pub mod message_conversion;` |
| `src/session/unified.rs` | Removed conversion functions; import from `message_conversion` |
| `src/session/manager.rs` | Enhanced `ResolvedSession` documentation |
| `src/agent/agent.rs` | Renamed `get_session_context` → `resolve_session`; return `ResolvedSession` |
| `src/channels/cli.rs` | Use `resolve_default_session()`; eliminate double lookup |
| `src/agent/announcement_service.rs` | Removed unused `SessionContext` import |

---

## Acceptance Criteria

- [x] `SessionContext` is a pure DTO with no active operations.
- [x] `SessionHandle` is the **sole** operations facade for sessions.
- [x] No call site uses `SessionContext` for `add_user_message`, `load_history`, etc.
- [x] `ResolvedSession` clearly separates `context` (read-only metadata) from `handle` (operations).
- [x] All existing tests pass.
- [x] Message conversion extracted to dedicated module (SRP, DRY).
- [x] Double-lookup pattern eliminated from CLI channel.

---

## Future-Proofing

This resolution is designed to prevent regression:

1. **Compiler-enforced separation:** `SessionContext` has no `&mut self` methods and no `Arc<RwLock<Session>>` field. It is physically impossible to add session operations to it without changing its struct definition.

2. **Single return type:** All session resolution methods return `ResolvedSession`. There is no API that returns `SessionContext` alone, preventing the temptation to use it for operations.

3. **Pure function module:** `message_conversion` is a module of free functions with no state. It cannot accumulate responsibilities because it has no `self` to mutate.

4. **Clear naming:** `resolve_*` implies "get me everything I need" (both metadata + handle), while `context` implies "metadata only".

---

## Related

- Issue 004: Session Management — Overlapping Types (partially resolved, now fully resolved)
- `src/session/message_conversion.rs`
- `src/session/unified.rs`
- `src/session/manager.rs`
- `src/session/context.rs`
- `src/agent/agent.rs`
- `src/channels/cli.rs`
