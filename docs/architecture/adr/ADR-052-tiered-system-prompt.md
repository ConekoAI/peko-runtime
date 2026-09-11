# ADR-052: Tiered System Prompt (T0 Principal / T1 Role / T2 Instance)

**Status:** Proposed
**Date:** 2026-09-11
**Author:** rlsn
**Related:** [ADR-050](ADR-050-capabilities-as-workspace-files.md)
(file-backed, per-iteration prompt content — the precedent this ADR
generalizes), [ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(workspace as trust boundary), ADR-019 (rebuild-per-iteration;
superseded by the 2026-09-10 frozen-prefix fix).

---

## 1. Context

The system prompt today is a **single flat Markdown body per agent**,
frozen at run start (`render_cache_stable`,
`peko-rs/engine/src/prompt/renderer.rs`) for provider prefix-cache
stability, plus an append-only `<runtime-context>` user message at the
conversation tail that re-renders every iteration with per-section
change detection (ADR-050 D2, amended 2026-09-10). That static/dynamic
split is cache-motivated, not semantic — nothing in the prompt reflects
*who* the agent is in the agent tree.

A 2026-09 audit of the prompt pipeline surfaced four structural gaps:

1. **Principal identity is boot-frozen and partly invisible.** The root
   persona (`agents/root.md`) is resolved once at principal boot and
   cached in the router and the `PeerChildTurns` bundle — mid-flight
   edits take effect only on reload. The `[identity]` and `[intent]`
   sections of `principal.toml` (display name, description, goals,
   values, preferences) are never rendered into any prompt; they are
   registry display metadata only.
2. **Agent roles do not reach spawned agents.** When the Agent tool
   spawns a named subagent, the agent's `AGENT.md` is resolved per
   spawn but consumed only for capability validation and spawn audit.
   The child runs with the *root persona* as its system prompt plus a
   `[Subagent Context]` user-message wrapper
   (`SubagentExecutorRuntime::execute_and_wait`,
   `peko-rs/core/src/agents/subagent_runtime_impl.rs`). A
   coder/tester/researcher agent is a root agent with a task
   description. This is a plumbing bug, not a design choice.
3. **Instance context barely exists.** An agent's own session path is
   not rendered for root/peer agents (only *other* peers' paths, via
   `PeersSessionContextHandler`); subagents get parent/child session
   keys in the task wrapper but no purpose or project context. The
   AGENTS.md auto-injection helpers in
   `peko-rs/engine/src/prompt/memory.rs` (`discover_shared_context`,
   64 KiB cap) survive but are dead code in production.
4. **Cache invalidation is add/remove-only.** The agents/skills catalog
   caches key on `(dir mtime, child count)`; in-place edits to an
   existing file do not bump the key, so "edit a file, see it next
   iteration" is false for content edits.

Reference design: OpenAI's codex CLI assembles per-turn context as a
**world-state diff** — ~15 typed sections, each with a persisted
`snapshot()` and a `render_diff(previous)` that emits a replacement
fragment only when the section changed ("These AGENTS.md instructions
replace all previously provided…" / "…no longer apply"), with hard byte
budgets (32 KiB across the AGENTS.md hierarchy) and sub-agent roles
modeled as capability-reducing config overlays on the parent session
("roles may customize the child … but never replace the parent
session's authority"). Peko's `RuntimeContextState` already implements
the dedupe half of this; the replacement/removal notices and the
tiering are missing.

## 2. Decision

### D1 — Three scope tiers, one section pipeline

Prompt content is layered by **scope**, and every tier uses the same
mechanics (file-backed, placeholder-filled, change-detected,
byte-capped):

- **T0 — Principal.** Who this principal is: persona, purpose,
  `[identity]` + `[intent]` from `principal.toml`, long-term memory
  (MEMORY.md), general tool and agent-path conventions. Applies to
  every agent in the tree.
- **T1 — Role.** What this agent is: the body of the agent's
  `agents/<name>.md` / `agents/<name>/AGENT.md` file — `root`,
  `channel-comm`, `coder`, `tester`, `writer`, `researcher`, …
  Selected when the agent is spawned (root at principal boot,
  channel-bound agents by their binding, Agent-tool spawns by the
  `agent` parameter). A role customizes the agent within the
  principal's authority; it never replaces T0.
- **T2 — Instance.** Where this agent sits: its own session slug path,
  its specific purpose/task, and project context (project path,
  AGENTS.md notes, caveats, rules) for the work it is doing.

Each tier is read from a file in the principal workspace and re-read
on agentic iteration boundaries, so a human or the principal itself can
edit any tier and have it take effect at the nearest iteration
boundary.

### D2 — Tiers ride the tail, not the frozen prefix

"Edits take effect immediately" and "byte-stable prefix for provider
prompt caching" conflict by construction. The resolution is the
existing ADR-050 D2 mechanism, generalized:

- The frozen `messages[0]` prefix stays minimal and stable (role body
  + generated runtime sections), composed **once per run** from T1.
- T0 and T2 render as **sections of the append-only
  `<runtime-context>` tail message**, with per-section change
  detection extended to codex-style diff semantics: when a tier's
  rendered text changes, the section is re-injected with an explicit
  "replaces previously provided …" notice; when a tier's source file
  is removed, a "… no longer applies" notice is injected instead of
  silently dropping the section. Unchanged tiers cost zero tokens
  after first injection.
- Freshness is per-tier cheap: a `(mtime, len)` **file-level** stat
  key per tier file per iteration (a handful of `stat` calls), fixing
  the add/remove-only invalidation of the existing dir-keyed catalog
  caches.

### D3 — T1 plumbing fix: spawned agents get their own role body

`SubagentExecutorRuntime::execute_and_wait` threads the resolved
named-agent prompt (`request.subagent_config`) into the child
`AgentConfig` instead of discarding it:

- Agent-tool spawn with `agent: "coder"` → child system prompt body is
  `agents/coder.md`'s body (T1), with T0/T2 arriving via the tail.
- Spawns with no named agent keep the root persona as the default T1
  (today's behavior, unchanged).
- Peer-DM "persona inheritance" (channel-bound children running the
  root persona) becomes an explicit per-binding policy rather than the
  only option: a channel binding may name a T1 role file
  (e.g. `channel-comm`) that the bound child runs instead of `root`.

### D4 — T0 composition

T0 is composed from sources, not one file, each its own change-detected
tail section:

- principal persona file (the `agents/root.md` resolution order stays:
  `[routing].root_prompt` → `agents/root/AGENT.md` / `agents/root.md`
  → compiled-in default) — re-read per iteration, so the boot-frozen
  router cache no longer gates prompt freshness;
- `[identity]` / `[intent]` from `principal.toml`, rendered as a
  compact section (name, description, goals, values, preferences);
- MEMORY.md (existing section, unchanged).

### D5 — T2 instance section

A `## Your position` tail section renders: the agent's own session
slug path, its purpose (spawn task summary for subagents; binding
description for channel-bound agents; "root session" for the trunk),
and — when the agent's working context is a project directory — the
nearest AGENTS.md content via the revived
`discover_shared_context` helper, byte-capped (32 KiB, matching the
codex budget) with truncation notice. `TurnPromptContext.session_id`
is already threaded to the renderer; this section consumes it for
self-reference.

### D6 — Extension seam

The `PromptSystemSection` hook point generalizes from the hardcoded
`"agents"` / `"skills"` dispatch to a registry of named tail sections
with priority ordering, so workspace hooks (ADR-047 §5 Phase 4) can
contribute prompt sections in addition to today's observe-only
`PreToolUse` / `PostToolUse` / `Stop` / `AfterAgent` points. The
built-in tiers are registrations in the same pipeline, not bespoke
systems.

## 3. Consequences

**Positive:**

- Every agent in the tree knows what it needs to know and no more:
  T0 makes the principal act as a whole; T1 gives each agent its role;
  T2 grounds it in its position and project — without duplicating
  principal-level content into per-agent prompt bodies.
- Prompt truth converges with file truth at every iteration boundary
  for all three tiers (D2), extending the ADR-050 guarantee from
  catalogs to identity.
- Prefix-cache stability is preserved: the frozen prefix changes only
  per run; volatile tiers are append-only tail sections with change
  detection, so unchanged tiers cost nothing after first injection.
- The T1 fix (D3) removes an unused-parameter smell that made named
  agents cosmetic.

**Negative / costs:**

- Tail messages age out via ordinary session compaction; a compacted
  tier is re-injected on the next change or context rebuild, but a
  long stable run accumulates one tail message per change — bounded by
  the change detector, not eliminated.
- Per-iteration `stat` calls grow by a handful per agent; negligible
  next to the existing per-iteration MEMORY.md read.
- Replace/remove notices add tokens on change; bounded by per-tier
  byte caps.
- Channel bindings that switch from persona inheritance to a named T1
  role change peer-facing behavior; migration is opt-in per binding.

**Migration:** none required for existing workspaces — absent tier
files render as absent sections (presence = visibility, ADR-050 D3).
The T1 plumbing fix changes spawned-subagent behavior immediately:
named agents finally run their own prompt, which is the behavior
`agents/<name>.md` files were written for.
