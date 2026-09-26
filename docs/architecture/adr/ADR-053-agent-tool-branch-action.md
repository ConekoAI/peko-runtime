# ADR-053: Agent Tool `branch` Action — Snapshot Sideline Runs

**Status:** Accepted (amended 2026-09-26 — `overwrite` reseeds the
target in place instead of archive-and-repoint; see D3)
**Date:** 2026-09-13
**Author:** rlsn
**Related:** [ADR-051](ADR-051-compaction-pages-as-addressable-archive.md)
(page model / boundary semantics), [ADR-052](ADR-052-tiered-system-prompt.md)
(cache-stable prompt head + volatile tail), round-7 Agent action surface
(`new` / `resume` / `compact`).

---

## 1. Context

The Agent tool's round-7 action surface — `new`, `resume`, `compact` —
covers delegation (spawn a fresh worker), continuation (re-attach a
spawned session), and maintenance (compact-and-continue). One recurring
shape is missing: **a sideline run that starts from a snapshot of an
existing session's live context and never touches that session.**

The motivating case is cron. A scheduled fire today has two bad options:

- Run context-free — useless when the task depends on what the main
  agent has been doing ("write today's briefing from this session's
  state").
- Run *in* the main session — pollutes the main lineage with sideline
  traffic, risks concurrent-turn corruption, and couples the schedule's
  failure modes to the main job.

What is needed is a third action: **copy the calling session's live
context into a fresh (or explicitly overwritten) session at a target
path, then run a prompt there.** Read-only w.r.t. the source, durable
for the sideline, composable with cron SpawnTool.

Three facts about the existing runtime shape this decision:

- **The LLM-facing live context is already a cache window.**
  `Session::load_history_native` restarts at the newest compaction
  boundary (`[summary, post-boundary messages]`), so "everything the
  source session would put on the wire" is well-defined without any new
  snapshot machinery.
- **The prompt head is cache-stable and contains no session identity**
  (ADR-052 D2/F23): `render_cache_stable` excludes `{{session_context}}`
  and every volatile section; identity, clocks, and catalogs ride the
  append-only tail `<runtime-context>` user message, re-rendered each
  iteration with per-section on-change injection, and persisted tagged
  `MessageSource::Hook`.
- **A prior "branch" exists at the CLI layer only**
  (`SessionManager::branch_session_by_id`): a full-storage copy with no
  path addressing, no spawn linkage, no guards, and no run — an
  addressable-archive convenience, not an agent surface.

## 2. Decision

Add a fourth Agent-tool action, **`branch`**:

```json
{"action": "branch", "path": "/briefing", "agent": "writer",
 "prompt": "Write today's briefing from the context above.",
 "source": "/main", "overwrite": true}
```

`source` defaults to the calling session; `overwrite` defaults to
`false`. The run is registered, executed, waited on, and announced
exactly like a spawn.

### D1 — Snapshot semantics: verbatim events, cache-window context

The branch performs a **point-in-time, read-only** copy of the source
session:

- **Storage level: raw `SessionEvent`s verbatim** from the newest
  compaction boundary *inclusive* to the end of the stitched log
  (`Session::events_since_last_boundary`), appended in order into the
  freshly minted target session. No re-serialization, no re-framing, no
  filtering. This deliberately *includes* the source's persisted
  `<runtime-context>` tail messages (`MessageSource::Hook`): they are
  part of the source's conversation history, and filtering them would
  diverge the copied history from what the source recently put on the
  wire.
- **LLM level: the cache window.** Because `load_history_native` is
  boundary-aware, the child's first turn sees exactly what the source's
  next turn would have seen: `[summary, post-boundary messages]`. The
  boundary event rides along so ADR-051 bookkeeping (page detection,
  cache-window totals) stays coherent in the child; the child's
  compaction counters are carried from the source (same rule
  `branch_session_by_id` applies) so a later child compaction does not
  renumber against the copied boundary.
- **Consistency: best-effort page-consistent read.** JSONL appends are
  atomic and the read is a single stitched scan, so the snapshot never
  sees a torn event. A source turn in flight at branch time is simply
  not in the snapshot — branch is defined as "context as of the last
  completed turn," which is the only well-defined cut.

The alternative — re-serializing through `copy_session_context` (the
shared-context spawn path) — is rejected: it flattens tool results away
("skip for now as they require tool_call_id linking"), losing the exact
conversation shape, and would break the byte-identity property below.

### D2 — Prompt identity: head copied implicitly, notice rides the tail

The branch does **not** re-render any prompt for the child. The child's
frozen head comes from its own `render_cache_stable` (same agent body,
same `mcp_context` — byte-identical to the parent's when the model and
tool set match); the copied history supplies the middle; the branch
notice + task are the first *new* user turn.

Because the head carries no session identity (ADR-052 D2), nothing in
the prefix is stale. The child's own tail `<runtime-context>` section
injects **the child's** session context from iteration 1 (fresh
`RuntimeContextState` always injects on first observation), so the
self-identification corrects immediately — no waiting for "the next
run." The stale source-path content the child sees is limited to the
copied historical tail messages, which are just history; the tool layer
composes the task prompt with a one-notice preamble:

> This session is a branch of `{source_path}` (snapshot taken at branch
> time). This session's own path is `{target_path}`. {task}

Consequence for cost: with a warm provider prefix cache and matching
model/tool surface, the child's first call pays cache-read pricing on
the copied prefix plus full price on the tail. A cold cache (e.g. cron
intervals longer than the provider TTL) pays full input price for the
window — the principal's cost pre-flight and quota metering apply
unchanged. Filtering the Hook tails would guarantee the full-price case
and is rejected.

### D3 — Target addressing: mint-or-repoint, never truncate

`path` follows the `new` action's addressing rules (relative slug →
child of the caller; single-segment absolute → top-level; multi-segment
absolute → attach-only). Resolution:

- **No session at the target:** mint a fresh spawn session (isolated —
  the lossy shared-context copy is skipped), stamp the slug.
- **Spawn-created session at the target, `overwrite: false`:** refuse
  (structured slug-conflict error, same as `new`).
- **Spawn-created session at the target, `overwrite: true`:** the
  target is OVERWRITTEN IN PLACE (real overwrite, amended 2026-09-26 —
  the original design archived the old session and repointed the
  address to a freshly minted one; that stranded the old session as an
  unreachable tombstone and shifted every descendant's address when the
  slug was cleared). The target keeps its id, slug, parent linkage, and
  whole descendant subtree; the source's cache-window snapshot is
  appended behind a compaction boundary that closes the target's
  previous live transcript, which is retained as an ADR-051 page
  (never truncated, never re-keyed — the page is inspectable via
  `list_pages` / `read_page`). Refuse when the target has an active
  run.
- **Non-spawn session at the target:** refuse with
  `err_name_not_spawned` (same collision rule as `new` — never
  silently displace a user-triggered or peer-bound session).

A peer-bound standing child can never be an overwrite victim: peer
children are not spawn-triggered, so the third rule refuses them
outright.

History is never destroyed and no session is displaced: `overwrite`
means *reseed the target in place*. The previous live transcript stays
readable as the newest archived page of the same session.

### D4 — Guards

The branch reuses the spawn guard stack, with source- and target-specific
rules:

- **Source:** resolved via `resolve_reference` (absolute slug path or
  the caller's own id; raw UUIDs and caller-relative slugs refused).
  Archived sources are allowed — branching is read-only w.r.t. the
  source, and "revive a dead session's context into a working sideline"
  is a legitimate use. No active-run guard on the source (the snapshot
  is page-consistent; D1).
- **Target:** not the caller's own session or an ancestor (mirrors the
  `compact` refusal — overwriting your own lineage would strand the
  calling run), spawn-trigger-only on overwrite, no active run on an
  overwritten target.
- **Shared:** cost pre-flight (`cost_per_call_max`), spawn-depth cap
  (`1 + depth(effective_parent)`), concurrency cap — identical to
  `spawn_and_execute`.

The run registers through the single `register_subagent_run` gate with
the caller as `parent_session_key`, so depth accounting, announcements,
async-task status, and quota attribution all behave exactly like a
spawn.

## 3. Consequences

- **Cron sideline tasks become first-class:** a schedule can fire
  `{"action": "branch", "path": "/briefing", "agent": "writer",
  "prompt": "…", "overwrite": true}` against the main session without
  touching it, repeatedly, with each run a pure function of the main
  context at fire time.
- **Continuity of the sideline across fires is deliberately NOT
  provided** by overwrite (each fire starts from a fresh snapshot; the
  previous fire's transcript is retained as a closed page, not part of
  the live context). A "briefing thread that remembers previous
  briefings" would need snapshot + prior-child-turns merging — a
  different decision, deferred. High-frequency fires grow the target's
  page list by one page per fire; a per-session page-limit retention
  policy is a natural follow-up.
- **The first child call dominates cost** when caches are cold; the
  cost pre-flight uses the existing conservative projection and does
  not model the copied window. Operators running high-frequency cron
  branches against large sessions should watch quota.
- **`copy_session_context`'s lossiness is now bypassed, not fixed.**
  Plain `new` spawns still inherit flattened history; branch is the
  fidelity path. A future decision may unify them.
- **`branch_session_by_id` remains** the CLI archive convenience; the
  agent surface does not depend on it.

## 4. References

- Cache-stable prompt / volatile tail: ADR-052 D2, `prompt/renderer.rs`
  (`render_cache_stable`, `render_runtime_context`), `agentic_loop.rs`
  injection site (2026-09-10 prompt-caching fix).
- Boundary-aware history: `session/src/unified.rs::load_history_native`.
- Page model: ADR-051, `session/src/pages.rs`.
- CLI-level branch precedent: `session/src/manager.rs::branch_session_by_id`.
