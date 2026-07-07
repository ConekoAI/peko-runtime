# ADR-041: Principal-as-Container and Session Blackboxing

**Status:** Accepted (rev. 2026-07-05)
**Date:** 2026-06-25 (original); revised 2026-07-05
**Author:** rlsn
**Supersedes / Deprecates:** The runtime entity formerly known as `Agent` is now a kind of `Principal`. `Agent` is retained only as a thin Markdown prompt file. The top-level `peko agent *` and `peko session *` command trees are removed.
**Related:** [ADR-021](ADR-021-daemon-as-central-runtime.md) (daemon as central runtime), [ADR-023](ADR-023-minimal-a2a-messaging.md) (agent-to-agent messaging), [ADR-027](ADR-027-unified-packaging.md) (packaging), [ADR-039](ADR-039-principal-model.md) (principal type unification), [ADR-042](ADR-042-no-external-session-concept.md) (no external session surface).

**Note:** This is a clean-slate pre-production design. Backward compatibility with Peko 0.1.0 is intentionally discarded so the codebase and UX remain coherent. This ADR was originally proposed with a broader feature surface (Team subject variant, three router strategies, six principal-layer hooks, a `peko principal memory *` CLI tree). On 2026-07-05 it was revised to record what was actually shipped, and the broader surface was removed entirely rather than deferred.

---

## 1. Context

### 1.1 Pre-041 model

In Peko 0.1.0 the **Agent** was the top-level entity:

- `peko agent create` was the starting point for every persistent actor.
- `AgentConfig` owned identity (DID), provider preference, capabilities, hooks, and ownership.
- Sessions were user-visible first-class objects: `peko session list/show/branch/switch/compact`.
- The canonical message was `peko send <AGENT> <MESSAGE>`, which resolved or created a `Session` keyed on `(agent, peer)` and stored the conversation in a JSONL file.
- `.agent` packages exported a single agent; `.team` packages bundled N agents.
- `Principal` (ADR-039) was an *authority* type used for ownership, permissions, and session peer tagging, but it owned no state and had no lifecycle.

This model works, but it leaks implementation into the user experience. A human talking to another human does not ask "which session is this?" — continuity is implicit. The user-visible session ID, session branching, and session switching are artifacts of the runtime's persistence layer, not the user's mental model.

### 1.2 The principal thesis

The principal thesis argues that the next architectural category above the agent is a **persistent, identity-bearing Principal** that owns:

- **Identity** — stable DID, multi-anchor continuity.
- **Memory** — multi-tier, with lifecycle management.
- **Intent** — goals and preferences that survive across sessions.
- **Agency** — bounded delegation to ephemeral agents.

Its core structural claim is the **principal-as-container**: a Principal contains Agents, Agents contain tool executions, and communication happens Principal-to-Principal. The agent is an *extension*; the principal is the *actor*.

---

## 2. Decision (what shipped)

### 2.1 Principal is the container

A `Principal` is the only top-level runtime entity. It owns:

- A stable DID (typed as [`PrincipalDID`]).
- A memory layer (per-peer session JSONL at `<workspace>/memory/sessions/{session_id}.jsonl`, plus agent prompts).
- A governance layer (`owner: Subject`, `permissions: Vec<PermissionGrant>`, `exposure: InstanceExposure`).
- An identity block (display name, description).
- Zero or more **Agent prompts** (thin Markdown files at `<workspace>/agents/{name}.md`).
- A workspace directory.

`Agent` is no longer a top-level runtime identity. It is a **thin markdown prompt file** — a specialization prompt that the Principal's root context can overlay onto its execution. Agent prompts have no runtime identity, no config, and no extension declarations; all extensions (tools, skills, MCPs) are declared on the Principal.

### 2.2 Subject is the actor enum; Principal is one variant

ADR-039 introduced `Principal` with variants `User / Agent / Team / Public`. This ADR renames the type to `Subject` (to avoid terminology collision with the container entity, which is also called `Principal`) and collapses the variants:

```rust
// src/subject.rs:37
pub enum Subject {
    User(String),                       // pekohub user or local DID
    Principal(PrincipalDID),            // an AI principal, identified by DID
    Public,                             // unauthenticated access
}
```

The `Subject::Agent` variant is removed; the `Agent` concept is no longer a subject because agents are not peers. The `Subject::Team` variant is removed because the team-of-principals concept is not part of v1.

`Subject::is_session_peer()` returns `false` for `Subject::Public` only; `User` and `Principal` are valid session peers.

### 2.3 The container entity: `PrincipalConfig` and `Principal`

The runtime container type lives at `src/principal/mod.rs`:

```rust
pub struct Principal {
    pub name: String,
    pub did: PrincipalDID,
    pub config: RwLock<PrincipalConfig>,
    pub memory: Arc<dyn PrincipalMemory>,
    pub router: Arc<dyn PrincipalRouter>,
    pub workspace_path: PathBuf,
}
```

`PrincipalConfig` (deserialized from `principal.toml`) carries:

- `name`, `did`, `owner: Subject`
- `identity: PrincipalIdentityConfig` — display name, description
- `governance: PrincipalGovernanceConfig` — permissions list
- `memory: PrincipalMemoryConfig` — consolidation / TTL policy
- `routing: PrincipalRoutingConfig` — `root_prompt: Option<PathBuf>`, `context_window_messages`, `recall_top_k`
- `allowed_extensions: AllowedExtensions`

### 2.4 Sessions are internal mechanics

A session is no longer a user-facing endpoint. It is the per-(principal, peer) JSONL file inside the Principal's memory layer. From the user's point of view:

- `peko send <principal> "..."` talks to a Principal.
- A subsequent message from the same peer resumes continuity automatically; there is no `--new` / `--fresh` flag.
- There is no `peko session list/show/branch/switch/compact` command tree.

Sessions still exist internally. They are created on first peer message, resumed on subsequent messages from the same peer, and read back via `peko log`. They are stored as JSONL at `<workspace>/memory/sessions/{session_id}.jsonl` and indexed in the principal's `peers.json`. Advanced inspection is available through `peko log` and (for operators) the on-disk JSONL itself. There is no dedicated `peko session` command tree, and there never will be (see [ADR-042](ADR-042-no-external-session-concept.md)).

The legacy agent-keyed session store at `<data_dir>/workspaces/<agent>/personal/` and the IPC variants that surfaced it (`SessionList`, `SessionRemove`, `SessionBranch`, `SessionCompact`, `SessionCompactDryRun`, `SessionSteer`, `SessionSteerList`, `SessionSteerCancel`) were retired in this PR under ADR-042. The internal storage path remains for the per-principal executor (`StatelessAgentService`) but is no longer reachable through the IPC surface.

### 2.5 Routing

Each `Principal` owns a `PrincipalRouter` built by `DefaultPrincipalRouterFactory` (`src/principal/factory.rs`). The router:

1. Receives an incoming message via `PrincipalManager::receive` or `PrincipalSendStream`.
2. Builds a `RouterContext` (peer, channel, recalled memory, allowed extensions, etc.).
3. Resolves the per-peer session (via `PrincipalMemory::find_latest_session_for_peer`).
4. Hands off to the root agent (`StatelessAgentService`) for execution.

There is a **single** router implementation in v1; there is no user-facing `strategy` config field on `PrincipalRoutingConfig`, no `agent:router` baseline, no `extension:my-router` extension point, and no principal-layer hook family (`PrincipalReceive`, `PrincipalRoute`, `PrincipalContextBuild`, `PrincipalAgentSelect`, `PrincipalMemoryStore`, `PrincipalRespond`). If a future ADR introduces them, they will need their own design pass and are out of scope here.

### 2.6 CLI surface

The shipped top-level command trees are `peko principal *`, `peko send`, and `peko log`. The 0.1.0 `peko agent *` and `peko session *` trees are removed.

Default UX:

```bash
# Talk to a principal. No session argument, no --new / --fresh flag.
peko send alice "Review this PR"

# Inspect the principal's owner-root activity (ADR-042).
peko log alice
peko log alice --peer user:bob
peko log alice --since 24h --json

# Manage principals.
peko principal create alice
peko principal list
peko principal show alice
peko principal agent list alice
peko principal permit alice user:bob chat
```

There is no `peko principal memory *` command tree and no `peko principal context` command in v1. Read access to a principal's per-peer conversation is `peko log`; there is no separate "memory" / "context" / "agent-prompt init" surface.

### 2.7 Packaging

A `.principal` package is a content-addressable archive containing `principal.toml`, the agent prompts directory, and the principal's memory (sessions, etc.). The format is defined alongside ADR-027's layer system; the on-disk shape is owned by `PrincipalMemory::workspace_path`.

There is no built-in import path for legacy `.agent` or `.team` packages. Migration is a one-time external concern (a converter tool may be provided, but it is not part of the runtime's primary surface).

---

## 3. Consequences

### 3.1 Positive

- **One runtime actor.** `Principal` is the only top-level entity; `Agent` is a thin prompt file.
- **Natural UX.** The default surface is "talk to `alice`", not "talk to `alice` in session `sess_xxx`". Continuity is automatic.
- **Continuity by design.** The Principal is the persistence unit, so memory, goals, and preferences have a clear owner.
- **Codebase stays coherent.** No dual model, no auto-promotion shims, no deprecated command trees.
- **Hard privacy boundary.** `peko log --peer X` requires `caller == X || caller == principal.owner` (ADR-042); the no-`peko session` external surface keeps that boundary hard to widen by accident.

### 3.2 Negative

- **Breaks 0.1.0 users.** No in-place migration; existing `.agent` / `.team` files require external conversion.
- **Single router strategy.** v1 ships one router (`DefaultPrincipalRouterFactory`). Power users who want a `agent:router` or `extension:my-router` style router have no escape hatch today. If a real user need surfaces, a follow-up ADR designs the extension point.
- **No principal-layer hooks.** The 22 agent-layer extension hooks remain. A principal-layer hook family (`PrincipalReceive`, `PrincipalRoute`, etc.) does not exist. Routing and memory behavior is fixed by `PrincipalRouter` and `PrincipalMemory` impls.
- **Team semantics deferred.** The `Subject::Team` aggregate and the team-of-principals concept are not part of v1.

---

## 4. Out of scope (separate ADRs / issues)

The original 2026-06-25 proposal mentioned the following follow-up ADRs. They are recorded here for traceability but **not deferred through this ADR** — they are simply not implemented and there is no commitment to ship them.

- **Governance primitives.** Delegation chains, monotonic narrowing, cryptographic audit trails.
- **Team-as-Principal.** A team that maps to a Principal containing other Principals, or a `Subject::Team` aggregate.
- **Memory lifecycle algorithms.** Consolidation, forgetting, refresh, staleness policies.
- **Principal discovery and registry.** How Principals are published, versioned, and discovered on PekoHub.
- **Router strategy extension point.** A `strategy` config field plus `agent:router` and `extension:my-router` implementations.
- **Principal-layer hook family.** `PrincipalReceive`, `PrincipalRoute`, `PrincipalContextBuild`, `PrincipalAgentSelect`, `PrincipalMemoryStore`, `PrincipalRespond` as a second extension surface.
- **`peko principal memory *` / `peko principal context` CLI trees.**
- **`peko agent-prompt init/install` for sharing reusable agent prompts.**
- **Cross-runtime P2P transport.** Principal-level addressing over the tunnel protocol; builds on ADR-035 but needs its own wire format decisions.
- **P2P reinterpretation of `principal_send`.** Routing as principal-to-principal rather than agent-to-agent. (Originally framed as the `a2a_send` work item; renamed with the tool.)

If any of these become real product needs, a new ADR is filed. This ADR does not promise them.

---

## 5. References

- [ADR-021](ADR-021-daemon-as-central-runtime.md) — daemon as central runtime
- [ADR-023](ADR-023-minimal-a2a-messaging.md) — minimal a2a messaging
- [ADR-027](ADR-027-unified-packaging.md) — unified packaging
- [ADR-037](ADR-037-agent-extension-bundling-and-layer-rationalization.md) — bundling and layer rationalization
- [ADR-039](ADR-039-principal-model.md) — principal type unification (predecessor)
- [ADR-042](ADR-042-no-external-session-concept.md) — no `peko session` external surface
- [`src/subject.rs`](../../src/subject.rs) — `Subject` enum and helpers
- [`src/principal/`](../../src/principal/) — `Principal`, `PrincipalConfig`, `PrincipalManager`
- [`src/principal/factory.rs`](../../src/principal/factory.rs) — `DefaultPrincipalRouterFactory`
- [`src/principal/memory.rs`](../../src/principal/memory.rs) — `PrincipalMemory` trait + impl