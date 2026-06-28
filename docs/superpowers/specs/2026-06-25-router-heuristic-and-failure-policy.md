# Router Heuristic and Failure Policy

**Status:** Draft  
**Date:** 2026-06-25  
**Author:** rlsn  
**Related:** [ADR-041: Principal-as-Container](../../architecture/adr/ADR-041-principal-as-container.md), [Router Agent Spec](2026-06-25-router-agent-spec.md), [Rust Types Spec](2026-06-25-principal-rust-types.md).

---

## 1. Framing

The Principal model does not change what Agents do. Agents still run the agentic loop, use tools, generate sessions, and produce responses. What changes is who decides *which* Agent to run, *which* session to continue, and *when* to start fresh.

In the current Peko 0.1.0 model, the user makes those decisions explicitly:

- `peko send alice "..."` continues the active session.
- `peko send alice --session sess_xxx "..."` resumes a specific session.
- `peko session branch sess_xxx` forks a thread.
- `peko session compact sess_xxx` trims old context.

In the Principal model, the **Router Agent** makes those same decisions on the user's behalf. The user just talks to the Principal. The Router Agent is, in effect, an automated user of the underlying agent system.

This document specifies:

1. The default heuristic for session selection.
2. When to continue, spawn, branch, or compact.
3. Failure policies and fallbacks.

---

## 2. Default router heuristic

The `builtin:default` router uses a deterministic decision tree. The `agent:router` strategy uses the same tree as guidance but may override via LLM reasoning.

### 2.1 Inputs

- `principal`: the receiving Principal.
- `peer`: the caller (`Subject::User` or `Subject::Principal`).
- `message`: the incoming message text.
- `candidates`: peer-specific sessions, sorted by `updated_at` descending.
- `config`: `principal.routing` settings.

### 2.2 Decision tree

```text
1. Collect candidate sessions for (principal, peer).
   If none → SPAWN.

2. Take the most recent session (MRS).
   If MRS is empty (created but no assistant response) → CONTINUE MRS.

3. Compute topic similarity between message and:
   - MRS title/summary
   - Last 3 user messages in MRS
   If max similarity >= threshold (default 0.75) → CONTINUE MRS.

4. Check for explicit session references in the message:
   - "continue that", "what about the other one", "go back to..."
   - If resolved to a candidate session → CONTINUE that session.

5. If message looks like a new topic (no similarity, no references,
   contains explicit new-task markers like "new task:", "unrelated:"):
   → SPAWN.

6. Ambiguous case (low similarity, no references, no new-topic markers):
   → CONTINUE MRS (continuity-by-default).
```

### 2.3 Thresholds

| Parameter | Default | Meaning |
|-----------|---------|---------|
| `similarity_threshold` | 0.75 | Minimum embedding cosine similarity to consider a message a continuation of a session. |
| `recent_message_window` | 3 | Number of recent user messages to compare against. |
| `continuity_default_hours` | 24 | If the most recent session is older than this, require stronger similarity to continue. |

If the most recent session is older than `continuity_default_hours`, the threshold is raised to `0.85` to avoid resurrecting stale threads.

### 2.4 Branching

Branching is **optional** and confidence-thresholded. The router branches when:

- Similarity to the most recent session is moderate (0.5–0.75).
- The message introduces a new sub-task that could benefit from the current context but should not pollute it.
- The router is confident this is an exploration, not a continuation.

Default behavior: **do not branch automatically**. The user can still branch manually via `peko principal memory session branch` if they need to. The Router Agent may be configured to enable auto-branching via `[principal.routing] auto_branch = true`.

### 2.5 Compaction

Compaction is **not a router decision**. Auto-compaction runs inside each Agent session based on the existing token-limit rules (`max_session_tokens`, `auto_threshold_percent`). The Router Agent does not trigger compaction.

The Router Agent's own session is also subject to auto-compaction. Because router sessions accumulate reasoning traces, a tighter `max_session_tokens` may be configured by default for router sessions.

---

## 3. Agent selection

Once the router has decided to `continue` or `spawn`, it must select an Agent prompt.

### 3.1 `builtin:default` agent selection

- Use `principal.routing.default_agent` unless the message matches a specialist Agent prompt.
- Matching is done by keyword/embedding similarity against agent prompt descriptions.
- If no specialist matches, use the default.

### 3.2 `agent:router` agent selection

The Router Agent uses `list_agents` and its LLM reasoning to select the best Agent prompt. It may also decide to chain multiple Agents (e.g., research → write → review) by emitting multiple `route` calls or by delegating to a target Agent that itself orchestrates.

---

## 4. Session routing actions

| Action | When it happens | What the user sees |
|--------|----------------|-------------------|
| `continue` | Message is a clear follow-up to an existing session. | Seamless continuation. |
| `spawn` | Message is a new topic, or no candidate sessions exist. | New thread started automatically. |
| `branch` | Message is a tangent/exploration (only if auto-branch enabled). | New thread with inherited context. |
| `respond` | Router decides no Agent invocation is needed. | Direct answer. |
| `defer` | Router decides the task should run later (async). | Acknowledgment + task receipt. |

---

## 5. Router failure policy

The Router Agent sits on the critical path of every message. If it fails, the Principal must still respond. The policy is: **fail soft, fall back, audit everything**.

### 5.1 Invalid route decision

If the Router Agent emits a `route` call that does not match the schema (unknown `target_agent`, invalid `action`, missing required fields):

1. Log the invalid decision at `warn` level.
2. Append a `system` event to the Router session describing the invalid decision.
3. Re-run the `builtin:default` router on the same message.
4. Execute the fallback decision.

### 5.2 Router loop

If the Router Agent emits more than `max_router_iterations` tool calls without emitting `route` (default 5):

1. Abort the Router Agent run.
2. Log a `router_loop_detected` event.
3. Append a `system` event to the Router session.
4. Run the `builtin:default` router as fallback.

### 5.3 Router Agent error

If the Router Agent errors (LLM failure, tool error, timeout):

1. Log the error.
2. Append a `system` event to the Router session.
3. Run the `builtin:default` router as fallback.

If the `builtin:default` router also fails (e.g., Principal has no default agent configured), return a generic error to the caller:

```text
"<principal> is unable to route this message right now. Please check the Principal configuration."
```

### 5.4 Target Agent error

If the routed-to Agent errors:

- If `synthesize=false`: propagate the error directly to the caller.
- If `synthesize=true`: append the error to the Router Agent's context and let it decide how to present the failure.

### 5.5 Memory/recall failure

If memory recall fails (e.g., vector store unavailable):

1. Log the failure.
2. Continue routing with empty recalled context.
3. Do not fail the message.

### 5.6 Session load failure

If the selected session cannot be loaded:

1. Log the failure.
2. Fall back to `spawn` with the default Agent.

---

## 6. Audit and observability

Every routing decision produces an event in the Router session:

```json
{
  "type": "tool.result",
  "tool": "route",
  "output": {
    "action": "continue",
    "target_agent": "reviewer",
    "session_id": "sess_abc123",
    "reason": "high similarity (0.87) to most recent session"
  }
}
```

Fallback events:

```json
{
  "type": "system",
  "event": "router_fallback",
  "detail": {
    "reason": "invalid_decision",
    "fallback": "builtin:default",
    "original_error": "unknown target_agent 'reviewerr'"
  }
}
```

---

## 7. Configuration

```toml
[principal.routing]
strategy = "agent:router"
default_agent = "primary"

[principal.routing.heuristic]
similarity_threshold = 0.75
recent_message_window = 3
continuity_default_hours = 24
auto_branch = false
max_router_iterations = 5
```

These settings also apply to `builtin:default` when used as the fallback.

---

## 8. Examples

### Example 1: Continuation

**User:** `peko send alice "What about the second option?"`

**Context:** Alice has an active session with `user:bob` about API design. Last message was about two options for rate limiting.

**Router decision:** `continue` the active session, target agent `primary`.

**User sees:** Seamless answer about the second rate-limiting option.

### Example 2: New topic

**User:** `peko send alice "Write a Python script to rename files in bulk"`

**Context:** Alice's most recent session with `user:bob` is about API design (low similarity).

**Router decision:** `spawn` a new session, target agent `coder`.

**User sees:** A new script is written. A new internal session is created.

### Example 3: Reference to older session

**User:** `peko send alice "Can we go back to the Q4 report discussion?"`

**Context:** Alice has three sessions with `user:bob`: API design (active), Q4 report (2 days ago), onboarding (last week).

**Router decision:** `continue` the Q4 report session.

**User sees:** The Q4 report discussion resumes.

### Example 4: Router failure

**User:** `peko send alice "..."`

**Failure:** Router Agent emits `route(target_agent="unknown")`.

**Policy:** Fallback to `builtin:default`, which routes to `primary` and continues the most recent session.

**User sees:** A correct response; no visible error.

---

## 9. Open questions

1. Should the Router Agent be allowed to emit **multiple** `route` calls in one turn for parallel delegation, or should it chain via the target Agent?
2. Should `spawn_async` be a separate action, or is `async: true` on `continue`/`spawn` sufficient?
3. How should the Router session be protected from memory poisoning given its high privilege?
4. Should the Router session have a tighter default `max_session_tokens` than normal sessions?
5. Should the similarity model be configurable (embedding model, fallback keyword rules)?

---

## 10. References

- [ADR-041: Principal-as-Container](../../architecture/adr/ADR-041-principal-as-container.md)
- [Router Agent Spec](2026-06-25-router-agent-spec.md)
- [Rust Types Spec](2026-06-25-principal-rust-types.md)
- [DATA_MODEL.md](../../../DATA_MODEL.md)
