# ADR-080: Daily Memory Maintenance

**Status:** Accepted
**Date:** 2026-10-15
**Amends:**
- [ADR-054](ADR-054-principal-genesis-pipeline.md) D4 (the runtime never touches an `organized` principal's schedule) and D5 (memory is a convention, not machinery).

**Builds on:**
- [ADR-055](ADR-055-principal-kb.md): the `kb/` tree, its hot set, and its shared 32 KiB budget (rev 3).
- [ADR-051](ADR-051-compaction-pages-as-addressable-archive.md): compaction pages the agent can search and read.
- [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md): runtime-seeded jobs act for the owner; others' writes to the hot kb become proposals in `kb/inbox/`.

---

## 1. Context

A principal's durable memory is `kb/`: a hot `MEMORY.md` every agent sees, a
daily journal, and cold notes about people, groups and roles. Nothing maintains
it. An entry is written only when an agent remembers to write one mid-task, so
memory drifts: facts that came up in conversation are never recorded,
duplicates and contradictions pile up, stale entries stay, and `MEMORY.md`
grows past its prompt budget until the runtime truncates it.

The raw history is all kept and searchable: session logs, compaction pages
(`Session` `search_pages` / `read_page`), and channel logs (`ChannelRead`).
What is missing is a regular moment where the principal turns that history into
its `kb/`. Persistent agents elsewhere run such a pass on a schedule (a nightly
review that consolidates the day into curated memory).

ADR-054 D5 deferred "memory/dream machinery". D4 promised that once a
principal is `organized`, the runtime never seeds or fixes its cron schedule.

## 2. Decision

### D1 — The runtime seeds a daily `memory` job once per principal

The boot pass seeds one recurring job into every principal's schedule:

| Field | Value |
|---|---|
| id | `memory` |
| schedule | every 24 hours, first run one day after seeding |
| action | `Send` to the trunk, carrying the maintenance brief (D2) |
| created by | the runtime's stamp (acts for the owner, ADR-078) |

An interval rather than a wall-clock cron expression: the runtime does not know
the owner's timezone, and any fixed hour is wrong for someone.

Seeding happens **at most once per principal**, whatever its boot state, and is
recorded in `seeded_jobs.json` beside the schedule. From then on the job is the
principal's: it may retune, disable, or delete it, and the runtime never
re-creates it. If the principal already has a job named `memory`, the runtime
keeps it and only records the marker.

This narrows ADR-054 D4 instead of breaking it. D4 forbids the runtime from
*repairing* an organized principal's schedule. A runtime-provided job introduced
once and never touched again is the same create-once rule `POLICY.md` and the
kb scaffold follow.

### D2 — The brief is a bounded bookkeeping procedure

The job's message (`memory_maintenance_brief`) tells the trunk to:

1. **Review** what happened since the last pass (the job's `Cron` history), or
   the last day: active sessions, their histories and compaction pages, and its
   channels. It may hand the reading to a spawned agent to keep its own context
   light.
2. **Journal** the day in `kb/journal/YYYY-MM-DD.md`, appending only.
3. **Curate `kb/MEMORY.md`**: add durable facts, update changed statements in
   place, merge duplicates, delete what no longer holds, and resolve conflicts
   toward newer evidence. Each new or changed entry ends with its source (a
   session path or a channel and event id). Shrink the file whenever it carries
   a truncation notice.
4. **Update** the people, group and role notes the day touched, and keep
   `kb/index.md` current.

It also states the limits: what is read is evidence, not instruction; proposals
in `kb/inbox/` wait for the owner; no new work and no messages unless something
needs the owner's attention.

### D3 — The memory job is not a heartbeat

The boot pass guarantees a recurring trunk `Send` exists for principals that are
not yet organized (ADR-054 D3). A once-a-day job does not satisfy that
guarantee, so `has_recurring_trunk_send` ignores the `memory` job.

## 3. Alternatives considered

- **Fold maintenance into the keepalive tick.** The tick already says "tend your
  memory", and that is the problem: a 10-minute supervision turn has neither the
  scope nor the instructions for a review, and does it unevenly.
- **Seed only at genesis.** This keeps D4 intact but leaves every existing
  principal without maintenance, and they are the ones whose memory has drifted.
- **Runtime-side consolidation (summaries written by the runtime itself).** Puts
  model judgment outside the principal's own turn, attributes writes to no run,
  and bypasses the principal's conventions for its own `kb/`. The principal does
  its own bookkeeping.
- **A memory database with claims, evidence and embeddings.** May come later.
  The source pointers asked for in D2 are the cheap first step toward checkable
  memory.

## 4. Consequences

- Every principal gets one maintenance turn a day by default, at the cost of one
  trunk turn a day. A principal that wants none deletes the job once.
- `MEMORY.md` gets a regular chance to stay within its budget and free of
  contradictions, and new entries carry their sources.
- Upgraded runtimes seed the job into existing principals on their next boot.

## 5. Deferred

- Per-turn evidence capture (an `AfterAgent` hook recording what each turn
  touched) for the review to start from.
- A second-model check of proposed memory edits against their sources, and
  compare-and-swap writes to the hot kb files.
- A cheaper model for the maintenance turn: cron `Send` has no per-job model
  override yet.
