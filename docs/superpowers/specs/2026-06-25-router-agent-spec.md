# Router Agent Specification

**Status:** Draft  
**Date:** 2026-06-25  
**Author:** rlsn  
**Related:** [ADR-041: Principal-as-Container](../../architecture/adr/ADR-041-principal-as-container.md), [ADR-023: Minimal Agent-to-Agent Messaging](../../architecture/adr/ADR-023-minimal-a2a-messaging.md), [principal_thesis_compact.md](../../../../principal_thesis_compact.md).

---

## 1. Purpose

The **Router Agent** is the default entry-point Agent inside a Principal. Its only job is to decide what happens to an incoming message. It does not (usually) perform the actual work; it routes the work to other Agents inside the Principal, recalls relevant context from the Principal's memory, and optionally synthesizes the final response.

In this model, an **Agent** is a thin Markdown prompt file. The Principal owns all runtime identity, tools, skills, MCPs, and memory. The Router Agent simply selects which prompt specialization to apply for a given message.

The Router Agent is one of three supported routing strategies in ADR-041:

- `builtin:default` — hard-coded router, no LLM.
- `agent:router` — this specification.
- `extension:my-router` — custom Rust extension.

The `agent:router` strategy is the baseline for a natural, human-like communication experience: the Principal has a "front mind" that decides how to handle what you just said.

---

## 2. Role in the Principal model

```
User / External Principal
         │
         ▼
   Principal boundary
         │
         ▼
   Router Agent session
   (decides what to do)
         │
    ┌────┴────┐
    ▼         ▼
 recall    route()
 memory    decision
    │         │
    └────┬────┘
         ▼
   Target Agent session
   (does the work)
         │
         ▼
   Response → Caller (or back to Router for synthesis)
```

Key properties:

- The Router Agent is itself an Agent, instantiated from the Principal's default prompt plus router-specific instructions.
- It has its own session inside the Principal's memory store (`memory/sessions/router.jsonl`).
- It sees the incoming message, the caller identity (`peer`), and recalled context — not the raw internal session list.
- It emits a structured routing decision via a special `route` tool that the runtime intercepts and executes.
- Its routing decisions are auditable artifacts in its session.
- It selects from the Principal's registered **agent prompts** (thin Markdown files), not from agent images or runtime identities.

---

## 3. Inputs

When a message arrives at the Principal boundary, the runtime constructs a **Router Request** and injects it as the first user message in the Router Agent's context.

```json
{
  "peer": {
    "kind": "user",
    "id": "user:bob"
  },
  "message": {
    "role": "user",
    "content": "Review this PR for bugs"
  },
  "channel": "cli",
  "principal": {
    "name": "alice",
    "did": "did:peko:local:alice:abc123"
  },
  "recalled_context": [
    {
      "type": "memory",
      "source": "session:sess_abc123",
      "summary": "Previous PR review on 2026-06-20. Bob prefers...",
      "relevance": 0.91
    }
  ]
}
```

The Router Agent may request additional recalls using the `recall_memory` tool.

---

## 4. Tools available to the Router Agent

The Router Agent has a minimal tool set. All other tools are withheld so it cannot accidentally do work that should be delegated.

### 4.1 `recall_memory`

Searches the Principal's memory for relevant context.

**Parameters:**

```json
{
  "query": "Bob's preferences for PR reviews",
  "k": 5,
  "filters": {
    "types": ["session", "todo", "file"],
    "peer": "user:bob"
  }
}
```

**Response:** list of memory snippets with relevance scores.

### 4.2 `list_agents`

Lists the agent prompts registered with this Principal.

**Response:**

```json
{
  "agents": [
    { "name": "primary", "role": "default", "description": "General-purpose assistant" },
    { "name": "reviewer", "role": "specialist", "description": "Code reviewer focused on bugs" }
  ]
}
```

Agent prompts are thin Markdown files owned by the Principal. They have no runtime identity and no capabilities of their own.

### 4.3 `route` (runtime-intercepted)

Emits the final routing decision. This tool is **not** executed by a tool runtime; the Principal's dispatcher intercepts the tool call and executes the decision.

**Parameters:**

```json
{
  "action": "continue",
  "target_agent": "reviewer",
  "input_message": "Review this PR for bugs. Bob prefers small, testable changes and wants security issues flagged explicitly.",
  "resume_session_id": null,
  "context_injection": [
    {
      "type": "memory",
      "id": "sess_abc123",
      "summary": "Bob's review preferences from 2026-06-20"
    }
  ],
  "synthesize": false,
  "async": false,
  "timeout_seconds": 120
}
```

**Action types:**

| Action | Meaning |
|--------|---------|
| `continue` | Resume an existing experience/session with the target Agent. |
| `spawn` | Start a fresh session with the target Agent. |
| `respond` | Answer directly from the Router Agent; no sub-agent invocation. |
| `defer` | Do not respond now; queue for later (e.g., user is away). |

**Field semantics:**

- `target_agent`: required for `continue`/`spawn`; the name of a registered agent prompt (thin Markdown file) owned by the Principal.
- `input_message`: the message as seen by the target Agent prompt. The Router may rewrite the user's message to add context or instructions.
- `resume_session_id`: explicit session to resume. If `null`, the runtime picks the most recent peer-specific session for `continue`.
- `context_injection`: memories/documents to prepend to the target Agent's context.
- `synthesize`: if `true`, the target Agent's response is fed back into the Router Agent for final synthesis before returning to the caller.
- `async`: if `true`, the target Agent runs as an async task; caller gets a task receipt.
- `timeout_seconds`: optional override for the target Agent execution.

### 4.4 `respond_directly` (alias for `route` with `action: "respond"`)

Convenience tool for the common case of answering without delegation.

---

## 5. Session model

The Router Agent has a dedicated, long-lived session at:

```
principal/{principal-id}/memory/sessions/router.jsonl
```

This session accumulates the Router Agent's view of the Principal's interactions. It is **not** the user's conversation history (that lives in the target Agent sessions). It is the Principal's internal reasoning trace.

Properties:

- The Router session is created on first message to the Principal.
- It is compacted using the normal session compaction rules.
- It can be inspected for debugging via `peko principal memory session alice router`.
- It is included in `.principal` package exports (subject to memory TTL policy).

The Router Agent's session key is derived from `(principal, peer=router, trigger=system)` so it does not collide with user-facing sessions.

---

## 6. Prompt design

The Router Agent's system prompt is assembled from:

1. Base router instructions (below).
2. The Principal's `[principal.intent]` block (goals, values).
3. The Principal's `[principal.governance]` block (permissions, max delegation depth).
4. A dynamically generated section describing the Principal's registered agent prompts (thin Markdown files).
5. A dynamically generated section describing the Principal's available capabilities (tools, skills, MCPs).

### 6.1 Base instructions (sketch)

```markdown
You are the session router for Principal {{principal.name}}.

Your job is to read the incoming message and decide what to do with it.
You do NOT perform the work yourself. You route it to the appropriate
agent prompt inside this Principal, or respond directly if no agent
prompt is needed.

Rules:
- Use `recall_memory` if you need context from past interactions.
- Use `list_agents` if you are unsure which agent prompt to invoke.
- Emit exactly one `route` decision per incoming message.
- When rewriting the user's message for the target agent prompt, preserve intent
  but add necessary context (caller identity, recalled memories, style prefs).
- If the user is just chatting or asking about the Principal itself,
  use `route` with `action: "respond"`.
- If the request clearly matches a specialist agent prompt's role, route to it.
- Prefer `continue` for follow-ups; prefer `spawn` for new topics.
- Respect `max_delegation_depth` from governance.

Available agent prompts:
{{agents_list}}

Available capabilities (tools, skills, MCPs):
{{capabilities_list}}
```

### 6.2 Intent injection

The Principal's goals and values are appended as:

```markdown
## Principal Goals
{{#each principal.intent.goals}}
- {{this}}
{{/each}}

## Principal Values
{{#each principal.intent.values}}
- {{this}}
{{/each}}
```

---

## 7. Runtime execution flow

```text
1. Caller sends message to Principal alice.
2. Runtime loads alice's Router session.
3. Runtime builds Router context:
     base prompt + intent + governance + recalled context + user message.
4. Router Agent runs. It may call recall_memory / list_agents.
5. Router Agent emits a route() decision.
6. Runtime validates the decision:
     - target_agent exists
     - caller has permission to invoke it
     - action is valid
7. Runtime executes the decision:
     - For continue/spawn: load the selected agent prompt (Markdown file), combine it with the Principal's capabilities and context, and execute the agentic loop.
     - For respond: return Router Agent's own response.
     - For defer: queue and return acknowledgment.
8. If synthesize=true, target Agent output is appended to Router session and
   Router Agent produces final response.
9. Resulting target Agent session(s) are persisted in Principal memory.
10. Response returned to caller.
```

---

## 8. Failure modes and fallbacks

### 8.1 Invalid route decision

If the Router Agent emits a `route` call that does not match the schema (missing `action`, unknown `target_agent`, etc.), the runtime:

1. Logs the invalid decision.
2. Falls back to the `builtin:default` router.
3. Records a `system` event in the Router session noting the fallback.

### 8.2 Router loops

If the Router Agent emits more than `max_router_iterations` (default 5) tool calls without emitting `route`, the runtime:

1. Aborts the Router Agent run.
2. Falls back to `builtin:default`.
3. Records the loop in the Router session.

### 8.3 Router Agent error

If the Router Agent itself errors (LLM failure, tool error, timeout), the runtime:

1. Logs the error.
2. Falls back to `builtin:default`.
3. Records the error in the Router session.

### 8.4 Target Agent error

If the routed-to Agent errors, the error is propagated to the caller unless `synthesize=true`, in which case the Router Agent may decide how to present the failure.

---

## 9. Configuration

In `principal.toml`:

```toml
[principal.routing]
strategy = "agent:router"
router_prompt = "./agents/router.md"   # optional; default builtin router prompt
max_router_iterations = 5
auto_recall = true
auto_recall_k = 5
```

If `router_prompt` is omitted, the runtime uses a built-in router prompt shipped with the runtime. The router prompt is just another agent prompt; it has no special capabilities beyond access to the routing tools.

---

## 10. Extension hooks

The Router Agent participates in the principal-layer hooks defined in ADR-041:

| Hook | How the Router Agent uses it |
|------|------------------------------|
| `PrincipalReceive` | Runtime may reject or transform the message before it reaches the Router Agent. |
| `PrincipalRoute` | Extensions may override the Router Agent's decision before execution. |
| `PrincipalContextBuild` | Extensions may inject additional memories into the Router Agent's context. |
| `PrincipalAgentSelect` | Extensions may approve/reject the selected target Agent. |
| `PrincipalMemoryStore` | Router session + target Agent sessions are persisted; extensions may tag or route them. |
| `PrincipalRespond` | Final response may be transformed before returning to caller. |

---

## 11. Relationship to `builtin:default`

The `builtin:default` router is the fallback and the fast path. It:

- Routes to `principal.routing.default_agent` (a registered agent prompt name).
- Resumes the most recent peer-specific session for `continue`.
- Does not run an LLM.

The Router Agent is used when:

- The Principal needs contextual routing (which agent prompt depends on the message).
- The Principal needs memory-aware routing.
- The Principal needs to synthesize responses from multiple agent prompts.

A Principal can switch between them by changing `principal.routing.strategy`.

---

## 12. Example flow

**User:** `peko send alice "Review this PR for bugs"`

**Router session events:**

1. `user.message` — the Router Request.
2. `tool.call` — `recall_memory(query="PR review preferences user:bob")`.
3. `tool.result` — memory snippets.
4. `tool.call` — `route(action="spawn", target_agent="reviewer", input_message="...", synthesize=true)`.
5. `spawn.request` — agent prompt `reviewer.md` loaded and executed.
6. `spawn.result` — reviewer returns findings.
7. `assistant.message` — Router Agent synthesizes final answer for caller.

**Target Agent session events (`reviewer` session):**

1. `user.message` — rewritten input from Router Agent.
2. `assistant.message` — review findings.

**Caller sees:** the synthesized review.

**Principal memory now contains:** Router reasoning trace + reviewer work session.

---

## 13. Open questions

1. Should the Router Agent be allowed to emit **multiple** `route` calls in one turn (parallel delegation), or exactly one?
2. Should the Router Agent have access to a `spawn_async` action, or should `async: true` suffice?
3. How is the Router session protected from memory poisoning? (It has high privilege; should its writes require extra verification?)
4. Should the Router Agent's own session be compacted more aggressively than normal sessions because it accumulates reasoning traces?
5. What is the exact JSON schema for `context_injection`? (Inline summaries vs. memory IDs vs. full artifact content.)

---

## 14. References

- [ADR-041: Principal-as-Container](../../architecture/adr/ADR-041-principal-as-container.md)
- [ADR-023: Minimal Agent-to-Agent Messaging](../../architecture/adr/ADR-023-minimal-a2a-messaging.md)
- [principal_thesis_compact.md](../../../../principal_thesis_compact.md)
