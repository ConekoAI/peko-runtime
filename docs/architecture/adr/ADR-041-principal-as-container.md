# ADR-041: Principal-as-Container and Session Blackboxing

**Status:** Proposed  
**Date:** 2026-06-25  
**Author:** rlsn  
**Supersedes / Deprecates:** ADR-039's assumption that `Agent` is a peer-level subject; the runtime entity formerly known as `Agent` is now a kind of `Principal`. `Agent` is retained only as a thin Markdown prompt file. The top-level `peko agent *` and `peko session *` command trees are removed.  
**Related:** [ADR-021](ADR-021-daemon-as-central-runtime.md) (daemon as central runtime), [ADR-023](ADR-023-minimal-a2a-messaging.md) (agent-to-agent messaging), [ADR-027](ADR-027-unified-packaging.md) (packaging), [ADR-039](ADR-039-principal-model.md) (principal type unification), [docs/thesis/principal_thesis_compact.md](../../../../principal_thesis_compact.md).

**Note:** This is a clean-slate pre-production design. Backward compatibility with Peko 0.1.0 is intentionally discarded so the codebase and UX remain coherent.

---

## 1. Context

### 1.1 Current model

In Peko 0.1.0 the **Agent** is the top-level entity:

- `peko agent create` is the starting point for every persistent actor.
- `AgentConfig` owns identity (DID), provider preference, capabilities, hooks, and ownership.
- Sessions are user-visible first-class objects: `peko session list/show/branch/switch/compact`.
- The canonical message is `peko send <AGENT> <MESSAGE>`, which resolves or creates a `Session` keyed on `(agent, peer)` and stores the conversation in a JSONL file.
- `.agent` packages export a single agent; `.team` packages bundle N agents.
- `Principal` (ADR-039) is an *authority* type used for ownership, permissions, and session peer tagging, but it owns no state and has no lifecycle.

This ADR assumes we are redesigning from a clean slate. The 0.1.0 command trees (`peko agent *`, `peko session *`) and package formats (`.agent`, `.team`) are not carried forward as primary surfaces.

This model works, but it leaks implementation into the user experience. A human talking to another human does not ask "which session is this?" — continuity is implicit. The user-visible session ID, session branching, and session switching are artifacts of the runtime's persistence layer, not the user's mental model.

### 1.2 The principal thesis

The [principal thesis](../../../../principal_thesis_compact.md) argues that the next architectural category above the agent is a **persistent, identity-bearing Principal** that owns:

- **Identity** — stable DID, multi-anchor continuity.
- **Memory** — multi-tier, with lifecycle management (consolidation, forgetting, refresh).
- **Intent** — goals and preferences that survive across sessions.
- **Agency** — bounded delegation to ephemeral agents.

Its core structural claim is the **principal-as-container**: a Principal contains Agents, Agents contain tool executions, and communication happens Principal-to-Principal. The agent is an *extension*; the principal is the *actor*.

### 1.3 What this ADR decides

This ADR adopts that frame for Peko:

1. **Principal becomes the top-level existence concept.** It is the thing that persists, owns memory, has a DID, and receives messages.
2. **Agent becomes a contained extension.** An Agent is a specialized execution context that a Principal instantiates, routes to, and supervises.
3. **Sessions are blackboxed.** Users and external Principals address a Principal; sessions are internal routing/continuity mechanics of the Principal's memory layer.
4. **Session routing is extension-controlled.** The Principal's entry point is a pluggable dispatcher that decides how an incoming message maps to agent execution, memory recall, and response synthesis.

This is not a rename of `Agent` to `Principal`. It is a layering change: `Agent` becomes a thin Markdown prompt file, `Principal` becomes the only runtime actor, and the user-facing surface moves up one level.

The ADR-039 `Principal` subject enum is renamed to `Subject` to avoid terminology collision. Its variants become:

- `Subject::User(id)` — a human user.
- `Subject::Principal(id)` — an AI principal (replaces `Subject::Agent`).
- `Subject::Team(id)` — a group of principals (to be refined in a follow-up ADR).
- `Subject::Public` — unauthenticated access.

The runtime container entity is `Principal`.

---

## 2. Decision

### 2.1 Principal is the container

A `Principal` is a runtime entity with:

- A stable DID.
- A memory layer (sessions + semantic/episodic stores + artifacts).
- A governance layer (owner, delegations, audit policy, kill-switch hooks).
- An intent layer (goals, values, preferences).
- Zero or more `Agent` definitions (the principal's capabilities).
- A session-router/dispatcher that handles incoming messages.

`Agent` is no longer a top-level runtime identity. It becomes a **thin markdown prompt file** — a specialized prompt that the Principal can overlay onto its execution context. A Principal instantiates Agents on demand; they guide a session (or part of one) and then terminate, like a process. A Principal may run multiple Agents or sub-agents concurrently.

There is no backward compatibility with 0.1.0 Agent configs. The codebase does not auto-promote `[agent]` blocks to Principals. Agent prompts may still be shared as reusable markdown files, but they are always instantiated inside a Principal and have no runtime identity of their own.

### 2.2 Sessions are internal mechanics

A session is no longer a user-facing endpoint. It is one of several artifact types stored inside the Principal's memory layer. From the user's point of view:

- `peko send alice "..."` talks to Principal `alice`.
- Sending a new message talks to the same Principal; continuity is automatic.
- There is no `peko session switch` and no session ID shown in normal responses.

Sessions still exist internally. They are created, resumed, branched, and compacted by the Principal's session-router. They are stored as JSONL (the proven, auditable format) and indexed in the Principal's memory store. Advanced inspection is available through `peko principal memory *` commands; there is no dedicated `peko session` tree.

### 2.3 Session routing is the principal's front door

Every Principal has a **session-router** that handles incoming messages. The router is responsible for:

1. Deciding whether the message continues an existing experience or starts a new one.
2. Recalling relevant context from the Principal's memory (prior sessions, documents, structured memories, todos).
3. Selecting or spawning the appropriate Agent(s) to do the work.
4. Returning or streaming a response to the caller.
5. Persisting the resulting artifacts (sessions, memory updates, files) back into the Principal.

The default router is built-in and does the obvious thing: route to the Principal's default Agent, using the existing `StatelessAgentService` path, while automatically resuming the most recent peer-specific experience.

Custom routers can be provided by extensions, or by declaring a special "router Agent" inside the Principal. The router Agent is itself an Agent and has its own session as a baseline; its routing decisions are auditable artifacts inside the Principal's memory.

### 2.4 Communication is Principal-to-Principal

A2A (agent-to-agent) becomes P2P (principal-to-principal). The addressing unit is the Principal DID, not the Agent instance ID. When Principal `alice` sends a message to Principal `bob`:

- `alice`'s runtime resolves `bob` to a DID and a reachable runtime (local, PekoHub, or tunnel).
- The message is delivered to `bob`'s session-router, exactly as if a user had sent it.
- `bob`'s router decides how to handle it.
- `bob`'s response is returned to `alice`.

The existing `a2a_send` tool is retained as a compatibility layer but is reinterpreted: it now sends from the calling Principal to a target Principal. The target resolution uses `Principal::principal_wire_id(did, name)` analogous to the existing `Principal::agent_wire_id` helper.

---

## 3. Sub-decisions

### 3.1 Configuration shape

A Principal is configured by a `principal.toml` (or `[principal]` block in a single-file config). Agent prompts are thin Markdown files referenced by the Principal.

```toml
# principal.toml

[principal]
name = "alice"
did = "did:peko:local:alice:abc123"
owner = { kind = "user", id = "user:bob" }

[principal.identity]
display_name = "Alice"
description = "A coding partner that remembers your style and project history."

[principal.intent]
goals = [
    "Help the user write, review, and ship code.",
    "Maintain continuity across coding sessions.",
]
values = [
    "Prefer small, testable changes.",
    "Ask before touching main-branch deploy files.",
]

[principal.governance]
audit = "all"                    # "all" | "commands" | "none"
max_delegation_depth = 2
auto_grant_tools = ["Read", "Bash", "Agent"]

[[principal.governance.delegations]]
to = "principal:bob"
permissions = ["memory:read", "tool:Agent:invoke"]
expires_at = "2027-06-25T00:00:00Z"

[principal.memory]
type = "multi_tier"              # "single" | "multi_tier"
consolidation = { enabled = true, interval = "7d", trigger = "auto" }
ttl_policy = { session = "1y", ephemeral = "30d" }
include_artifacts = ["sessions", "todos", "files", "vectors"]

[principal.routing]
# Options:
#   strategy = "builtin:default"
#   strategy = "agent:router"
#   strategy = "extension:my-router"
strategy = "builtin:default"
default_agent = "primary"
context_window_messages = 50
recall_top_k = 5

# All capabilities live on the Principal.
[principal.capabilities]
tools = ["github", "browser"]
skills = ["research"]
mcps = ["vector-store-memory"]

[[principal.agents]]
name = "primary"
prompt = "./agents/primary.md"   # thin markdown prompt file
role = "default"

[[principal.agents]]
name = "reviewer"
prompt = "pekohub.com/agents/reviewer.md"   # registry ref or local path
role = "specialist"
```

An **Agent prompt** is a thin Markdown file containing a specialization prompt. It has no runtime identity, no config, and no extension declarations. All extensions (tools, skills, MCPs, agents) are declared on the Principal. The Agent prompt simply tells the Principal how to behave for a specific task or persona.

### 3.2 CLI surface

The only top-level command trees are `peko principal *` and `peko send`. The 0.1.0 `peko agent *` and `peko session *` trees are removed.

Default UX:

```bash
# Talk to a principal. No session argument, no --new / --fresh flag.
peko send alice "Review this PR"

# Inspect the principal's living memory/context.
peko principal memory show alice
peko principal memory search alice "Q4 earnings"
peko principal memory compact alice

# View the active context window (what the principal is "thinking with" right now).
peko principal context alice
```

Advanced / debugging UX:

```bash
# Raw session inspection (advanced, under principal memory).
peko principal memory sessions alice
peko principal memory session alice sess_9nrwbf1v

# Agent prompts used by the principal.
peko principal agent list alice
peko principal agent add alice ./agents/reviewer.md --name reviewer

# Ownership and governance.
peko principal grant alice --to user:bob --permission memory:read
peko principal revoke alice --from user:bob --permission memory:read
```

Packaging:

```bash
peko principal export alice --output alice.principal
peko principal import alice.principal --name alice2
```

Agent prompts are reusable markdown files, not runtime identities:

```bash
# Create a new agent prompt from a template.
peko agent-prompt init ./agents/reviewer.md --role code-reviewer

# Share or install agent prompts (no runtime identity; just markdown).
peko agent-prompt install pekohub.com/agents/reviewer.md
```

### 3.3 Principal entry point and routing model

The Principal receives messages through a single entry point:

```rust
async fn receive(
    principal: &Principal,
    peer: Principal,               // who is talking to us
    message: Message,              // the incoming message
    channel: ChannelContext,       // CLI, HTTP, A2A, webhook, etc.
) -> Result<Response, PrincipalError>;
```

`receive` delegates to the configured **session-router**. Three router kinds are supported:

1. **`builtin:default`** — routes to the default Agent, automatically resuming the most recent peer-specific experience. This is the baseline that preserves today's `peko send` behavior while hiding sessions.
2. **`agent:router`** — uses a dedicated Agent inside the Principal as the router. The router Agent sees the incoming message plus recalled context, and outputs a routing decision (which agent to invoke, whether to spawn, what memory to load). This is the "session routing agent" baseline the user requested.
3. **`extension:my-router`** — delegates to a `principal:routing` extension that implements a Rust trait. This is the escape hatch for custom orchestration.

The router is **not** a black box that removes user control. It is itself governed by the Principal's intent, governance, and memory layers, and it emits audit events for every routing decision.

### 3.4 Principal-layer hooks

The existing 22 extension hooks remain agent-layer hooks. This ADR adds a new family of **principal-layer hooks** so the routing and memory behavior is extension-customizable:

| Hook | Fires when | Typical use |
|------|-----------|-------------|
| `PrincipalReceive` | Message arrives at the Principal boundary. | Spam filtering, policy enforcement, logging. |
| `PrincipalRoute` | Before the router selects an execution path. | Override routing (e.g., always send code questions to `reviewer`). |
| `PrincipalContextBuild` | Before the router/agent sees the message. | Inject recalled memories, documents, or synthesized summaries. |
| `PrincipalAgentSelect` | Router has chosen an Agent. | Approve/reject the choice, inject agent-specific instructions. |
| `PrincipalMemoryStore` | Artifacts are about to be persisted. | Transform, tag, or route memories to external stores. |
| `PrincipalRespond` | Response is being returned to the caller. | Format, summarize, or attach citations. |

These hooks are defined in the extension framework alongside the existing 22. They use the same hook registration mechanism (`extension.yaml` manifest).

### 3.5 Packaging

A `.principal` package is a content-addressable archive containing:

```
alice.principal (gzip-compressed tar)
├── manifest.toml              # Package metadata + layer digests
├── principal.toml             # Principal identity, intent, governance, routing
├── identity/
│   ├── did.json               # DID document
│   └── keys.enc               # Encrypted private keys
├── memory/                    # Principal-level memory layer
│   ├── sessions/              # JSONL sessions (the internal experience store)
│   ├── vectors/               # Semantic memory embeddings
│   ├── todos.jsonl            # Planning todos
│   └── artifacts/             # Other persisted artifacts
├── agents/                    # Agent prompt markdown files
│   ├── primary.md
│   └── reviewer.md
└── extensions/                # Principal-specific extensions (optional)
    └── ...
```

The `.principal` format reuses the existing layer system from ADR-027/037. Agent prompts are plain markdown files with no runtime identity — their identity is derived from the Principal DID plus their local name.

There is no built-in import path for legacy `.agent` or `.team` packages. Migration is a one-time external concern (a converter tool may be provided, but it is not part of the runtime's primary surface).

### 3.6 Memory layer integration

The Principal owns a unified memory namespace. Internally it may map to multiple stores (JSONL sessions, SQLite `state.db`, vector DB, files), but externally it presents as:

- `principal.memory.recall(query, k)` — retrieve relevant experiences.
- `principal.memory.store(artifact)` — persist an artifact (session, todo, file, structured memory).
- `principal.memory.compact()` — run consolidation.
- `principal.memory.forget(predicate)` — remove or archive stale memories.

Sessions remain JSONL files because the append-only, audit-friendly format is correct for conversation history. But they are now addressed as `principal/{id}/memory/sessions/{session-id}.jsonl`, not as top-level runtime resources.

---

## 4. Consequences

### 4.1 Positive

- **Clean conceptual model.** One runtime actor (`Principal`), one message target (`peko send <principal>`), one package format (`.principal`).
- **Natural UX.** The default surface is "talk to `alice`", not "talk to `alice` in session `sess_xxx`". Continuity is automatic; there is no `--new` / `--fresh` flag.
- **Continuity by design.** The Principal is the persistence unit, so memory, goals, and preferences have a clear owner.
- **P2P communication matches user intent.** Users think "ask Bob", not "ask Bob's `worker-2` instance on runtime Y".
- **Pluggable orchestration.** The session-router abstraction lets the same runtime host simple chat principals, complex multi-agent principals, and everything in between.
- **Clear governance surface.** Delegations, audit, and kill-switches attach to the Principal, the entity that actually persists.
- **Packaging coherence.** One artifact (`.principal`) represents the whole persistent actor, instead of splitting identity across `.agent` and `.team`.
- **Codebase stays coherent.** No dual model, no auto-promotion shims, no deprecated command trees.

### 4.2 Negative

- **Breaks 0.1.0 users.** No in-place migration; existing `.agent` / `.team` files require external conversion.
- **Agent prompt authors have a simpler job.** An Agent is just a markdown specialization prompt; no config, no identity, no packaging.
- **Router quality becomes critical.** If the default router makes bad session-resumption choices, the user experience degrades because there is no manual override in the default path.
- **Cold-start cost.** Rehydrating a Principal's memory on first message is more expensive than rehydrating a single session. Caching and lazy loading become necessary.
- **Team semantics change.** Today's `.team` is a bundle of agents. Under this model it becomes a Principal-of-Principals or a Principal with sub-agents; the old `team.toml` format is not carried forward.

---

## 5. Migration path

There is no in-place migration from Peko 0.1.0. This design is intended for a clean-slate major version. Existing users re-create their actors as Principals. A standalone converter may be provided to turn 0.1.0 `.agent` files into Principal packages, but it is not part of the runtime core.

### Phase 1: Core Principal model

1. Introduce `PrincipalConfig`, `Principal`, and `PrincipalManager` as the new top-level entities.
2. Rename the ADR-039 `Principal` subject enum to `Subject` with variants `User`, `Principal`, `Team`, `Public`.
3. Remove the `peko agent *` and `peko session *` command trees.
4. Move session storage path to `<principal-workspace>/memory/sessions/`.
5. Implement `peko principal create / send / memory / context`.

### Phase 2: Router abstraction

1. Define the `PrincipalRouter` trait and the three router kinds (`builtin:default`, `agent:router`, `extension:my-router`).
2. Implement the `builtin:default` router (routes to default Agent, auto-resumes peer experience).
3. Implement the `agent:router` baseline (router Agent with its own session).

### Phase 3: P2P and packaging

1. Reinterpret `a2a_send` as principal-to-principal.
2. Introduce `.principal` package format.
3. Add principal-layer extension hooks.
4. Provide `peko agent-prompt init/install` for sharing reusable agent prompts.

---

## 6. Out of scope (follow-up ADRs)

- **Governance primitives.** Delegation chains, monotonic narrowing, cryptographic audit trails, and kill-switch semantics deserve their own ADR.
- **Team-as-Principal.** How a team maps to a Principal containing other Principals, and what happens to `team.toml`.
- **Memory lifecycle algorithms.** Consolidation, forgetting, refresh, and staleness policies are policy decisions, not container decisions.
- **Principal discovery and registry.** How Principals are published, versioned, and discovered on PekoHub.
- **Router Agent specification.** The prompt shape, tool set, and failure modes of `agent:router`.
- **Cross-runtime P2P transport.** The tunnel protocol already supports cross-runtime A2A; the principal-level addressing layer builds on it but needs its own wire format decisions.

---

## 7. Open questions

1. **Resolved:** Agent is not a top-level entity. It is a thin markdown prompt file instantiated by a Principal. The ADR-039 `Principal` subject enum is renamed to `Subject`; the container entity is `Principal`.
2. **Resolved:** Session storage path is `principal/{id}/memory/sessions/`.
3. **Resolved:** The `agent:router` strategy gives the router Agent its own session as a baseline; routing decisions are persisted in the Principal's memory.
4. **Resolved:** There is no `--new` or `--fresh` flag on `peko send`; the Principal's router decides continuity.
5. **Resolved:** Agent prompts have no runtime identity; capabilities live on the Principal.
6. How should the `builtin:default` router decide whether to resume an existing experience or start a new one? (Heuristic: same peer + recency + embedding similarity?)
7. Should `peko principal memory sessions` allow branching/compaction of internal sessions, or are those operations router-driven only?
8. How does a Principal's router access memories delegated from another Principal?
9. Should a team be a Principal that contains other Principals, or a special `Subject::Team` aggregate?
10. What is the failure mode when the router Agent itself errors or loops?

---

## References

- [principal_thesis_compact.md](../../../../principal_thesis_compact.md)
- [ADR-021](ADR-021-daemon-as-central-runtime.md)
- [ADR-023](ADR-023-minimal-a2a-messaging.md)
- [ADR-027](ADR-027-unified-packaging.md)
- [ADR-037](ADR-037-agent-extension-bundling-and-layer-rationalization.md)
- [ADR-039](ADR-039-principal-model.md)
- [`src/auth/principal.rs`](../../src/auth/principal.rs)
- [`DATA_MODEL.md`](../../DATA_MODEL.md)
