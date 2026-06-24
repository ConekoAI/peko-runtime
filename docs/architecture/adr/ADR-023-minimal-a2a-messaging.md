# ADR-023: Minimal Agent-to-Agent (A2A) Messaging

**Status**: Accepted  
**Date**: 2026-04-28  
**Last Updated**: 2026-04-28  
**Author**: Kimi Code CLI  
**Depends On**: ADR-021 (Daemon as Central Runtime), ADR-019 (Dynamic Tool and Prompt Updates)  
**Replaces / Supersedes**: `docs/A2A-MIGRATION-PLAN.md` (event-bus-centric approach)

---

## Context

Peko has a partially implemented A2A messaging subsystem that has never been fully wired into the runtime:

- `src/team/bus/mod.rs` — `EventBus` trait with direct, broadcast, task, and pub/sub operations.
- `src/team/bus/memory.rs` — `InMemoryBus` implementation with agent inboxes.
- `src/engine/input.rs` — `AgentInput::A2AMessage` enum variant (defined but never consumed by the agentic loop).
- `src/session/events.rs` — `A2aSentEvent` and `A2aReceivedEvent` for JSONL audit (never emitted).
- `src/tools/sessions_send.rs` — Stub tool that returns simulated responses; explicitly disallows agent-to-agent use within teams.
- `docs/A2A-MIGRATION-PLAN.md` — Proposes unifying A2A with the async executor framework via `SessionMessage` variants and queue-based delivery.

**The core problem**: the event-bus architecture assumes long-running agent processes with persistent inboxes. Peko's current architecture is **stateless cold-start** (ADR-021). Agents are spawned per-request, execute, and are dropped. There is no persistent process to own an inbox, no background task to poll the bus, and no code path to inject bus messages into the agentic loop.

Building out the event bus to full functionality would require:
1. Persistent agent processes or a background polling task per agent.
2. Agent registration with the bus on startup and unregistration on shutdown.
3. A new input path in `AgenticLoop` to handle `AgentInput::A2AMessage`.
4. A delivery guarantee mechanism (what happens if the target agent is not running?).
5. Session ownership semantics for ephemeral A2A sessions.

This is a large architectural project that duplicates concerns already solved by the existing execution path.

---

## Decision

We will implement A2A messaging as a **minimal built-in tool** (`a2a_send`) that delegates to the existing `StatelessAgentService` execution path — the same path used by `peko send` and the HTTP API.

**Principle**: Agents talk to other agents the same way users do — by sending a message into their session and letting the target agent execute.

### Why This Works

The existing execution path (`StatelessAgentService::execute_message`) already handles everything A2A needs:

| Requirement | Existing Capability |
|-------------|---------------------|
| Session resolution | `SessionManager::resolve_session()` — finds or creates target session |
| History / context | `load_session_history()` — target agent resumes with full context |
| Blocking execution | `Agent::execute_with_session()` — waits for full agentic loop |
| Async execution | `_async` framework parameter on any tool |
| Timeout | `_timeout` framework parameter |
| Session branching | Already supported by `SessionManager` |
| Audit trail | Session JSONL records all messages via `add_user()` / `add_assistant()` |
| Streaming | `execute_streaming_with_session()` for real-time delivery |
| Caller identification | Built-in tool schema includes caller metadata |

### Tool Design

```rust
/// a2a_send — send a message to another agent and receive its response
///
/// This tool delegates to StatelessAgentService, reusing the exact same
/// execution path as `peko send` and the HTTP API.
pub struct A2aSendTool {
    agent_service: Arc<StatelessAgentService>,
}
```

**Parameters:**
```json
{
  "target_agent": "analyzer",
  "message": "Review this code for bugs",
  "session_id": "optional-session-to-resume",
  "_async": true,
  "_timeout": 120
}
```

**Blocking response:**
```json
{
  "success": true,
  "response": "I found 3 issues...",
  "session_id": "agent:analyzer:session:xyz",
  "iterations": 2,
  "tool_calls": [...]
}
```

**Async response:**
```json
{
  "task_id": "task_abc123",
  "task_file": "/path/to/task_abc123.json",
  "status": "accepted"
}
```

### What the Target Agent Sees

The target agent receives a normal user message via `execute_message()`. It does not need to know it was called by another agent. Its session JSONL records a standard `user.message` → `assistant.message` exchange.

If desired, the `a2a_send` tool can prepend a lightweight system annotation (e.g., `[Message from agent: researcher]`) so the target agent knows who is calling. This is optional polish, not architectural change.

---

## Consequences

### Positive

1. **A2A works immediately** — one tool implementation, not a multi-week architecture project.
2. **Zero duplication** — reuses session management, execution, timeout, async, streaming, and audit infrastructure.
3. **Consistent semantics** — A2A messages behave exactly like user messages; no special cases in the agentic loop.
4. **Testable** — mock `StatelessAgentService` in unit tests; use existing E2E patterns.
5. **Scales with the runtime** — as the execution path improves (better caching, faster cold-start), A2A improves automatically.

### Negative

1. **Synchronous coupling** — the caller waits for the target agent's full execution (unless `_async` is used). This is acceptable for delegation patterns but not for fire-and-forget notifications.
2. **No pub/sub** — agents cannot broadcast to a topic and have multiple subscribers react. This is deferred to the event bus (see Migration Path).
3. **No persistent inbox** — if an agent is not executing, messages cannot be queued for later delivery. The `_async` parameter queues the task, but the target agent only "receives" it when the task is picked up.

### Neutral / Deferred

- The existing `EventBus` trait and `InMemoryBus` implementation are **kept but not wired** into the agentic loop. They remain available for a future where agents are long-running processes.
- `AgentInput::A2AMessage` and `A2aSentEvent` / `A2aReceivedEvent` remain in the codebase but are unused. They may be revived if the event bus is activated later.

---

## Migration Path

### Phase 1: Implement `a2a_send` Tool (Immediate)

1. **Add `A2aSendTool`** in `src/tools/a2a_send.rs`.
2. **Wire into `Agent::init_builtins_async()`** — requires passing `StatelessAgentService` into the tool. Options:
   - **Preferred**: Store `StatelessAgentService` in `ExtensionCore` (consistent with how `global_core()` works).
   - **Alternative**: Pass through `Agent` constructor from `AppState`.
3. **Update `sessions_send`** — remove the cross-team blocking comment and simulated response; make it a real human-to-agent send (optional — can coexist).
4. **Add E2E tests** in `e2e_tests/a2a/`:
   - Blocking A2A send between two agents
   - Async A2A send with task file polling
   - Session resumption across A2A calls
   - Timeout and error handling

### Phase 2: Caller Identification (Short Term)

1. **Add caller metadata** to `MessageRequest` (e.g., `caller_agent: Option<String>`).
2. **Propagate through `A2aSendTool`** so the target agent's system prompt can include `[Message from agent: researcher]`.
3. **Record in session JSONL** as metadata on the user message.

### Phase 3: Event Bus Revival (Long Term — Optional)

When / if Peko moves to long-running agent processes:

1. **Agent registration** — agents register their inbox with the team bus on startup.
2. **Background polling** — a task per agent polls `EventBus` for incoming messages.
3. **Agentic loop integration** — `AgentInput::A2AMessage` is handled as a first-class input source.
4. **`a2a_send` becomes a bus client** — the tool can optionally use the bus for fire-and-forget messages, while still supporting the direct execution path for request/response patterns.

At that point, `a2a_send` and the event bus **coexist**:
- `a2a_send` → request/response delegation (synchronous, reliable, session-aware)
- Event bus → notifications, broadcasts, pub/sub (asynchronous, decoupled)

---

## Related Documents

- `docs/A2A-MIGRATION-PLAN.md` — Superseded by this ADR; kept for historical reference.
- `CAPABILITY_INTERFACE.md` §3.9 — `sessions_send` specification (to be updated to reflect `a2a_send`).
- `API_CONTRACT.md` — A2A event types (`a2a_sent`, `a2a_received`) remain documented but marked as deferred.

---

## Implementation Checklist

- [x] Create `src/tools/a2a_send.rs` with `A2aSendTool`
- [x] Pass `StatelessAgentService` into `Agent` / `ExtensionCore`
- [x] Register `A2aSendTool` in `Agent::init_builtins_async()`
- [x] Update `CAPABILITY_INTERFACE.md` §3.9 to document `a2a_send`
- [x] Add E2E tests: `e2e_tests/a2a/a2a_blocking.ps1`, `a2a_async.ps1`, `a2a_all.ps1`
- [x] Mark `docs/A2A-MIGRATION-PLAN.md` as superseded
- [x] (Optional) Fix `sessions_send` to delegate to `StatelessAgentService`
- [x] Add structured `caller_agent` field to `MessageRequest` and `ExecutionRequest`
- [x] Defensive empty-string filtering in `with_caller_agent_opt()` and service layer
- [x] All 898 unit tests pass
