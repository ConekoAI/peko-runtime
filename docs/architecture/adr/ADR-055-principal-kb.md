# ADR-055: The Principal Knowledge Base (`kb/`) — One Persistent, Revision-Only Tree

**Status:** Proposed (rev 1)
**Date:** 2026-09-13 (rev 1: 2026-09-14)
**Author:** rlsn (with WorkBuddy)
**Related:** [ADR-052](ADR-052-tiered-system-prompt.md) (T0/T1/T2 tiered
prompt — the hot set renders as tail sections),
[ADR-054](ADR-054-principal-genesis-pipeline.md) (genesis pipeline —
D5 "memory is a convention" this ADR gives a floor),
[ADR-050](ADR-050-capabilities-as-workspace-files.md) (presence =
visibility, per-turn catalog rendering),
[ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(workspace as trust boundary),
[ADR-051](ADR-051-compaction-pages-as-addressable-archive.md)
(the archival mechanism whose reserved Shared-tier slot this ADR deletes).

---

## 1. Context

ADR-054 guarantees every principal a heartbeat and a genesis turn, but
the workspace floor underneath that turn is thin: `/tmp` + `/trash`
sessions and an agent prompt. Memory today is a single convention —
`MEMORY.md` at the workspace root, rendered as a T0 prompt section with
a 256 KiB cap (`engine/src/prompt/memory.rs`). Everything else a
principal needs to persist — notes about people, group conventions,
daily logs, reference material, datasets — has no default home. The
genesis brief responds with "decide what standing structure you need",
so every principal invents ad-hoc structure from scratch.

Two reserved/declared surfaces make the gap sharper:

- `SharedLayout::memory_snapshots_dir` (`memory/snapshots/`) is a
  declared-but-never-used path ("deferred to Phase A.5", packager doc
  comment) — a vestige of pre-ADR-051 assumptions about portable
  memory.
- The tiered-prompt work (ADR-052) established the machinery a richer
  memory surface needs — per-section change detection, byte caps,
  catalog rendering — but only one file feeds it.

Meanwhile `memory` and `knowledge base` were shaping up as two separate
trees. They are the same thing: **files the principal owns, persists
indefinitely, and revises in place — no compaction, only revision**.
The perceived difference (beliefs vs. reference material) is
*provenance*, not mechanics; both behave identically at runtime.

### Rev 1 additions (2026-09-14)

Review of the v0.1.0 hot-set design surfaced two problems, both about
*what rides in every prompt*:

1. **The people/groups catalogs were a redundant copy.** `index.md` is
   already hot — the agent always sees the map and can `Read`
   `kb/people/` on demand. Injecting a one-line-per-file catalog of
   both directories on every turn, for every agent, pays tokens for
   information that is one tool call away and is irrelevant to most
   agents. The failure mode a catalog defends against (the agent does
   not know the kb exists) is already solved by the index being hot.
2. **The hot set had no per-context member.** Every hot section was
   principal-level: every agent in the tree saw the same tail. But a
   channel-bound agent needs "who is in this room, what did I commit
   to here" at turn start, and a named agent has durable context of
   its own — neither had a home. There was also no per-agent
   *persistent* surface at all: T1 (role) and T2 (instance) from
   ADR-052 are per-agent, but everything durable belonged to the
   principal.

The resolution is a **pointer-only hot set** (D2 rev 1) plus **two
targeted, scope-matched injections** (D8) — scope as data, not
materialized per-agent copies. A materialization design (spawn-time
template → per-agent copy in the workspace) was considered and
rejected: it forks the template per spawn (the stale-copy problem this
ADR's D6 already cites), pollutes the packaged Shared tier with
per-spawn artifacts, and hands ephemeral subagents a persistent
identity they should not have. Ephemeral subagent learnings flow back
into the principal's kb via the spawn result; per-agent persistence is
reserved for long-lived, named agents.

## 2. Decision

### D1 — One tree: `kb/` is the principal's persistent mind

The Shared tier gains a single conventional tree. `memory/` as a
separate concept is **deleted** — including the never-used
`memory/snapshots/` reservation, which is removed from
`SharedLayout` (`paths.rs`) rather than renamed.

```
{config_dir}/principals/<name>/            (Shared tier — packaged)
├── principal.toml                         # [identity] [intent] — T0 source
├── identity.json
├── agents/ skills/ tools/ mcp/ hooks/ plugins/ peers.json
└── kb/                                    # THE persistent tree
    ├── MEMORY.md                          # HOT — curated long-term memory
    ├── index.md                           # HOT — the map of the tree
    ├── people/<who>.md                    # cold — per-person notes, via index
    ├── groups/<channel>.md                # cold — per-group notes; the file
    │                                      #   matching a binding is injected
    │                                      #   into that binding's agents (D8)
    ├── agents/<name>.md                   # cold — per-agent notes; a named
    │                                      #   agent's note rides with it (D8)
    └── …everything else…                  # COLD — refs/, journal/, imports/,
                                           #        projects/, datasets, any shape
```

Revision rules, as README convention (not machinery): principal-authored
files are **revised in place**; imported material is **replaced on
refresh** while the principal's notes *about* it are revised. Nothing in
`kb/` is ever compacted away — that word belongs to sessions.

### D2 (rev 1) — Pointer-only hot set: `kb/MEMORY.md` + `kb/index.md`

The runtime contract is exactly two hot files: `kb/MEMORY.md` (curated
long-term memory) and `kb/index.md` (the map of the tree). They render
as ADR-052 tail sections (change-detected, replace/remove notices),
each byte-capped. **The people/groups catalog scans from v0.1.0 are
dropped** — people/, groups/, and everything else in `kb/` are cold:
reachable via Read/Glob/Grep, discovered through the hot index
(presence = visibility, ADR-050).

Rationale: the index is the pointer; a full catalog of two directories
duplicated the index's job at per-turn cost for every agent, with no
selection. A principal whose index discipline is weak has a curation
problem, not a runtime problem — the same stance the ADR takes on
packaging (D7: curation is the filter).

The hot set shares a codex-style budget (32 KiB across the hot
sections, `PRINCIPAL_MEMORY_MAX_BYTES` tightened accordingly);
`index.md` additionally gets its own per-section cap so a runaway map
cannot starve memory. (Rev 1 ships per-section caps for index and the
D8 notes; unifying MEMORY.md onto the shared budget stays in
Deferred.)

`MEMORY.md` moves from the workspace root to `kb/MEMORY.md`
(`PRINCIPAL_MEMORY_FILE` resolves through the new `KB_DIR` constant).
The prompt section header ("Your long-term memory (MEMORY.md)") stays
accurate.

### D3 — P0 seeds the scaffold as convention docs, create-once

`PrincipalManager::create` (via the CLI provision path) seeds the
scaffold: `kb/`, `kb/people/`, `kb/groups/`, `kb/agents/`, and six
files — `kb/README.md`, `kb/MEMORY.md`, `kb/index.md`,
`kb/people/README.md`, `kb/groups/README.md`,
`kb/agents/README.md`. Every seeded file is a **convention doc**: it
explains its own purpose and the revision rules, giving the genesis
turn something concrete to read and rewrite. README files double as
non-empty-directory placeholders (empty dirs are invisible in tar and
`.principal` packages).

Semantics deliberately match `/tmp` + `/trash` (ADR-054 `default_nodes`):

- **Create-once, create-if-missing.** Seed writes only files that do
  not exist; it NEVER overwrites. The principal owns its kb the moment
  it exists.
- **No reserved semantics, no boot re-seeding.** A principal that
  deletes `kb/index.md` keeps it deleted (the section simply renders
  absent). Absence is a valid state.

### D4 — One-time migration at boot for legacy principals

Existing principals hold `MEMORY.md` at the workspace root; the contract
move would silently orphan their memory. The ADR-054 boot pass
(`seed_boot_defaults`) gains a one-time, idempotent migration: if
`<workspace>/MEMORY.md` exists and `kb/MEMORY.md` does not, it is MOVED
into `kb/`. No other legacy structure is touched — a legacy principal
without people/groups/agents/index simply renders absent sections until
its own turns (or its human) fill the tree in. The runtime does not
re-create deliberately removed files, ever.

### D5 — Genesis curates, it does not invent

The genesis brief's step 3 changes from the open-ended "decide what
standing structure you need" to: survey the seeded `kb/` scaffold, keep
what fits, restructure what does not, and record the conventions you
adopt in `kb/index.md`. The floor is the runtime's; the structure is
the principal's.

### D6 — Framework self-knowledge is a pointer, not a copy (deferred)

PEKO.md and the framework manuals are **runtime truth** — versioned
with the binary, shared by every principal on the host. Copying them
into each principal's `kb/` would mint N stale copies per update and
bloat every `.principal` package. The eventual shape is the existing
**Runtime tier** ("installed once for the runtime; principals access
via capability grants") plus a pointer file seeded into `kb/`.

**Deliberately deferred:** the pointer mechanism itself. The intended
delivery is a web page hosting the always-current manual (the runtime
version pins the compatible docs version), so the pointer is a URL
rather than a filesystem path. Until then the genesis brief remains the
bootstrap self-knowledge surface. A principal may, of course, take its
own marginalia (e.g. `kb/notes/peko.md`) — those notes are principal
property and travel with the bundle.

### D7 — Bulk and opaque artifacts

`kb/` accepts any file shape (md, images, xlsx, sqlite, …). Two
conventions, README-carried, no machinery:

- **Every opaque artifact gets a one-paragraph `.md` shadow** next to
  it — what it is, where it came from, how to use it. Markdown is what
  the model navigates; `Read` handles images natively; structured
  formats need a skill.
- **Size policy for packaging:** curation is the packaging filter. Huge
  artifacts that shouldn't travel in `.principal` bundles live cold
  under the Local tier (or a principal-chosen path) and are referenced
  by pointer files from `kb/`.

### D8 (rev 1) — Targeted scope injections: binding notes and agent notes

The hot set gains two context-scoped members, keyed by *where the agent
sits*, not by catalog scan:

- **Binding note.** When a run's triggering channel matches a file
  `kb/groups/<channel>.md`, that file renders as a `<runtime-context>`
  tail section for that run only. This is the ADR-049 counterpart: a
  channel-bound agent sees its room's conventions at turn start
  without the principal-wide catalog. Agents with no channel binding
  (or no matching file) render nothing — the same content stays
  reachable cold.
- **Agent note.** A named agent (`ctx.agent_name`) with a file
  `kb/agents/<name>.md` gets that file as its durable, per-agent
  memory section — the persistent layer ADR-052's T1/T2 lacked. The
  note is principal-owned (lives in the principal's kb, packaged,
  travels with the bundle); the named agent edits it through normal
  kb access. Ephemeral, unnamed spawns get no note — their learnings
  return through the spawn result into the principal's kb.

Both are ordinary ADR-052 tail sections: on-change injection with
replace/remove notices, per-section byte caps, absence renders absent,
segment names validated (no path traversal). **Scope as data, not
copies**: nothing is materialized at spawn; the section is a resolved
view over the principal's kb. Deleting a binding does not orphan
anything — the note simply stops being injected and stays a cold file
the principal can retire.

## 3. What ships in this change

- `engine/src/prompt/memory.rs` — `KB_DIR` constant;
  `load_principal_memory` reads `kb/MEMORY.md`; new `load_kb_index`,
  `load_binding_note`, `load_agent_note` loaders with per-section
  caps and segment validation.
- `engine/src/prompt/renderer.rs` — `SectionSlot::{KbIndex,
  BindingNote, AgentNote}` tail sections (on-change, capped).
- `core/src/common/paths.rs` — `SharedLayout::memory_snapshots_dir`
  removed (field, layout construction, `ensure_principal_dirs`, tests).
- `core/src/principal/kb.rs` — `seed_kb_scaffold` (create-once,
  create-if-missing, six convention files incl. `kb/agents/README.md`)
  + `migrate_legacy_memory` (the D4 one-time move); unit tests for
  idempotence and no-clobber.
- `core/src/principal/genesis.rs` — boot pass runs the migration per
  principal (reported in `SeedReport`); genesis brief amended (D5,
  mentions `kb/agents/` and the cold conventions).
- `cli/src/commands/principal.rs` — `provision_principal` seeds the
  scaffold (P0, model-free).
- `docs/architecture/PRINCIPAL_WORKSPACE.md` — layout table updated.
- `core/src/resources/agents/root/AGENT.md` — root agent brief updated
  (memory + index ride; the rest is looked up through the index).

## 4. Consequences

**Positive:**

- Every principal gets a healthy memory floor at creation: hot
  identity-adjacent memory, a map of its own knowledge, and places for
  people/groups/agent conventions — without any new runtime machinery.
- The hot set is pointer-only: two files instead of two open-ended
  catalog scans, so per-turn hot cost is bounded by curation of the
  index, not by contact-list growth.
- Channel-bound agents and named agents get context-scoped memory at
  turn start — the missing per-agent persistent layer — with zero
  duplication machinery (no materialized copies, no forks).
- One mental model ("everything I persistently know lives in `kb/`"),
  one packaging story (the kb travels in the Shared tier; curation is
  the filter).
- The vestigial `memory/snapshots/` reservation is gone; ADR-051 page
  exports, when they land, are ordinary cold kb files.
- Genesis curates instead of inventing, and the hot set is byte-bounded
  by construction.

**Negative / costs:**

- The `MEMORY.md` move is a contract change: any tooling or habit
  reading `<workspace>/MEMORY.md` directly must follow `kb/MEMORY.md`.
  The D4 migration covers on-disk state; external consumers must update.
- Six seeded files per principal — trivial bytes, but the scaffold is
  an opinion; a principal that wants a different shape rewrites or
  removes (and absence renders absent).
- The D8 injections key on the triggering channel and agent name;
  a principal whose channel ids differ from its `kb/groups/` filenames
  gets no injection until it names a file to match (the README says
  so). Renames are principal-side convention, not runtime sync.
- Binding/agent notes add two small per-iteration file reads per run;
  negligible next to the existing per-iteration MEMORY.md read.

## 5. Deferred (tracked follow-ups, not promised)

- **Runtime-tier docs + web-hosted manual pointer** (D6) — the URL
  pointer file seeded into `kb/`, once the manual site exists.
- **Shared hot budget**: tighten `PRINCIPAL_MEMORY_MAX_BYTES` (256 KiB
  single-file) onto the 32 KiB across-hot-sections budget from D2,
  registered via ADR-052 D6 `PromptSystemSection`.
- **Binding-note stamping**: whether a fresh `kb/groups/<id>.md` is
  seeded (create-once) when an ADR-049 channel binding is created, or
  stays pure convention. Injection reads whatever exists either way.
- **ADR-051 page exports as kb files.**

## 6. References

- [ADR-052](ADR-052-tiered-system-prompt.md) — tiered prompt; the tail
  section pipeline the hot set and D8 injections ride on.
- [ADR-054](ADR-054-principal-genesis-pipeline.md) — genesis pipeline;
  the boot pass hosting the D4 migration.
- [ADR-050](ADR-050-capabilities-as-workspace-files.md) — presence =
  visibility; per-turn catalog rendering.
- [ADR-049](ADR-049-multi-party-group-channels.md) — channel model; the
  bindings D8's binding-note injection keys on.
- `peko-rs/core/src/principal/kb.rs` — scaffold + migration primitives.
- `peko-rs/engine/src/prompt/memory.rs` — hot-set loaders.

---

*Version 0.2.0 · Principal Knowledge Base · 2026-09-13 (rev 1 2026-09-14)*
