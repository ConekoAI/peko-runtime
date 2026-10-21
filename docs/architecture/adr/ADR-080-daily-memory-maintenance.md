# ADR-080: Daily Memory Maintenance

**Status:** Accepted
**Date:** 2026-10-15 (rev 2 2026-10-16: D4–D6; rev 3 2026-10-16: D7–D9)
**Amends:**
- [ADR-054](ADR-054-principal-genesis-pipeline.md) D4 (the runtime never touches an `organized` principal's schedule) and D5 (memory is a convention, not machinery).
- [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) D6: `kb/dreams/` joins the protected trees (rev 3, D7).
- [ADR-083](ADR-083-quiet-ticks.md) D1: `when` gains `quiet` (rev 3, D9).

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

**Rev 3.** Bookkeeping records what happened. Nothing looks at *how* the
principal works: the corrections its owner made, the mistakes it repeats, the
way its owner wants things done. Agents keep relearning these. Muse runs a
nightly "dream" that reviews recent conversations into `~/dreams/`, and a
synthesis step turns those notes into standing guidance. The dream notes
themselves are not put into the prompt. Peko's standing guidance is
`kb/CONVENTIONS.md`, which every agent reads. ADR-080 D4 left that file
unchecked. The pass also runs every 24 hours from whenever it was seeded,
often in the middle of the owner's day.

## 2. Decision

### D1 — The runtime seeds a daily `memory` job once per principal

The boot pass seeds one recurring job into every principal's schedule:

| Field | Value |
|---|---|
| id | `memory` |
| schedule | every 24 hours, first run one day after seeding; each due fire waits for a quiet stretch (D9) |
| action | `Send` to the trunk, carrying the maintenance brief (D2) |
| created by | the runtime's stamp (acts for the owner, ADR-078) |

An interval rather than a wall-clock cron expression: the runtime does not know
the owner's timezone, and any fixed hour is wrong for someone.

Seeding happens **at most once per principal**, whatever its boot state, and is
recorded in `seeded_jobs.json` beside the schedule. From then on the job is the
principal's: it may retune, disable, or delete it, and the runtime never
re-creates it. (Amended 2026-10-21: only a run the owner started in person —
their own message, or a request they approved — may delete or disable it, or
the other seeded jobs `genesis` and `keepalive`. Genesis and scheduled turns
retune them in place. A genesis run on MiMo deleted this job, and nothing
noticed; `peko_cron::seeded_job_refusal`.) If the principal already has a job named `memory`, the runtime
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

### D4 — A scheduled run writes hot memory only from the owner's conversations

A cron turn acts for the owner (ADR-078 D5), yet it reads everyone's
conversations: the memory pass reviews every channel, and the keepalive checks
them. Without a check, a guest's words reach `kb/MEMORY.md`, and from there every
agent's prompt, as the owner's own memory. ADR-078 sends non-owner writes to the
inbox, but these writes come from an owner run.

- When an owner run started by a cron job (`Origin::Cron`, any job but the
  one-shot `genesis` turn) writes `kb/MEMORY.md` with `Write` or `Edit`, the
  dispatcher compares the file as the call would leave it with the file as it
  is. The comparison is by entry: a list item with the lines that continue it,
  or a paragraph. Blank lines, headings, rules and fences only separate
  entries. Each added entry, meaning not already in the file (compared with
  whitespace collapsed, so moving or re-wrapping adds nothing), must cite
  `(source: a, b)` on any of its lines. A source is a session path or id, or
  `<channel id>#<event id>`; `chan_x#4, #5` is short for `chan_x#4, chan_x#5`.
- A source passes when it exists and its compartment's ceiling is owner: the
  principal's internal tree, or a conversation where only the owner speaks
  besides the principal. An entry passes when it cites at least one source and
  all of them pass.
- When any added entry does not pass, the whole call is held: nothing of it is
  written, and each failing entry is filed in `kb/inbox/` under the
  conversation it cites (uncited or unknown ones go under the run's own
  compartment). The tool result lists them and says to repeat the change
  without them. *Amended 2026-10-17:* the gate used to remove only the
  failing lines and write the rest. A run that rewrote an entry then lost
  it: the old version's removal applied and the new version was held. On
  MiMo v2.6 Flash this dropped four of five owner facts in one nightly pass.
- The genesis turn is exempt. It organizes a new principal from its own
  definition, reads no one's conversation, and its brief asks for no sources;
  screening it filed every line it wrote to `kb/MEMORY.md` in the inbox.
- The memory run (`Origin::Cron { job_id: "memory" }`, and agents it spawns)
  gets no `Bash`, so the file tools are its only way to the file. Other cron
  turns keep `Bash`: fixed-tool jobs (`tool: "Bash"`) run under the same stamp.

What this does not cover:
- A line citing an owner source it did not come from, such as a reader agent's
  internal session. Internal sessions count as the owner's own work.
  *Addressed by [ADR-085](ADR-085-memory-claims-reviewed-against-their-sources.md):*
  a reviewer model checks each sourced line against excerpts of its sources,
  and holds what it does not find supported.
- `kb/index.md`, and the group and role notes injected into specific agents.
  (`kb/CONVENTIONS.md` is covered since rev 3, D8.)
- `Bash` in other cron turns, and MCP servers that write files.

### D5 — The brief is supplied when the job fires

The seeded job stores a fixed marker message, not the brief. The cron engine
writes the current `memory_maintenance_brief` into the turn when a job carrying
the marker fires, so a runtime upgrade that improves the brief reaches every
principal without touching its schedule, and the brief names the principal by
its current display name. A principal that replaces the message keeps its own.
A `memory` job still carrying the full brief stored by the first build is read
like the marker.

`Cron` `update` refuses to replace the marker (2026-10-17). It changes the
job's schedule, `when` and `model` in place, so the job keeps its id, and with
it the brief and the `Bash`-free run (`is_memory_run`). A principal that wants its own routine creates a separate job.
Before `update` took a schedule, principals deleted and re-created the job to
retune it, and each re-created job lost all of this.

### D6 — A message job can run on its own model

A cron `Send` action may carry `model`, a configured model id. Each fire's
turn runs on it instead of the principal's pinned model, in the trunk or in
the creating session. `Cron` `create` takes `model` with `message`, and
`update` sets it, or clears it with an empty string. The runtime refuses a
model it cannot resolve when the job is created or updated. The seeded memory
job has no model; the principal may give it a cheaper one.

### D7 — The pass reflects on how the principal worked, into cold `kb/dreams/`

After the bookkeeping (D2 steps 1–4), the brief adds two steps:

5. **Reflect** on how the principal worked, not on what happened: what went
   well and what went badly, corrections the owner made, mistakes it repeated,
   and preferences about how to work (tone, length, when to ask, which tools).
   The lessons go to `kb/dreams/YYYY-MM-DD.md`: one file per night, added to
   and never rewritten, each lesson ending with `(source: …)` as in D2.
6. **Promote** (D8).

`kb/dreams/` is cold, and nothing ever injects it. The hot set stays
`MEMORY.md`, `index.md`, `CONVENTIONS.md`, and the two scope notes under
`kb/groups/` and `kb/roles/` (ADR-055). A lesson changes how the principal works
only once it is promoted. The kb scaffold seeds `kb/dreams/README.md` describing
this, and `kb/index.md` and `kb/README.md` list the directory. Existing
principals get no scaffold files; the brief has them create the file and list
the directory in their index.

`kb/dreams/` is a protected tree (ADR-078 D6): only owner runs write it, and
anyone else's write becomes a proposal. The promotion step reads it, so a guest
who could write there could plant a "lesson" that cites the owner.

### D8 — A lesson becomes standing guidance only through `kb/CONVENTIONS.md`

Promotion is an edit to `kb/CONVENTIONS.md`, which every agent of the
principal reads. The brief asks the trunk to read the last week of dream files
and promote a lesson only when the owner stated it, or when it came up on two or
more nights. Before promoting, it reads the conversation the lesson cites. It
writes a short rule about how to work, revises an existing rule rather than
adding a near-duplicate, cites the conversations the lesson came from (not the
dream file), keeps the file under its 4 KiB prompt cap, and marks the lesson
promoted in its dream file.

The D4 source rule now covers `kb/CONVENTIONS.md` as well as `kb/MEMORY.md`.
Every line a scheduled owner run adds to either file must cite only owner
sources. Other lines are filed in `kb/inbox/` under the conversation they cite,
and the tool result says that lessons from conversations with others stay in
`kb/dreams/`. A dream file is not a source the gate accepts, so a lesson cannot
skip its origin by citing its own note. An owner-driven run (the owner telling
the principal "from now on, do X") writes conventions as before.

### D9 — The pass waits for a quiet stretch

A cron `Send` action's `when` (ADR-083 D1) gains `quiet`. A due `quiet` fire
runs its turn once nobody has talked to the principal for 4 hours
(`QUIET_STRETCH_MS`), or 12 hours after it fell due (`QUIET_MAX_WAIT_MS`),
whichever comes first. Until then it is held. `next_run` moves to the earlier of
those two times, `last_status` reads `quiet`, and no run row is written. If
someone talks to the principal in the meantime, the next check holds it again.
"Talked to" is the idle detector's activity: an ingress turn starting or ending,
or a message queued for a busy session. Cron turns, wakes and other principals'
posts do not count. The due time is the schedule's next slot after the job's
last turn, so holding the fire does not move the deadline. `quiet` needs an
interval or cron schedule. An `Idle` schedule already waits for quiet, and a
one-shot always runs. Manual triggers always run.

The seeded `memory` job keeps its 24-hour interval and stores `when: quiet`.
Because the next slot counts from the turn that ran, the pass settles at the
start of the principal's longest regular quiet stretch, which for most owners
is the night. A late night pushes it later. If it spills past the night, it
waits out the day (at most 12 hours) and settles back the next night.

Existing seeded jobs are **reinterpreted when they fire**, as rev 2 did for the
brief and ADR-083 D4 for the keepalive. A `memory` job that still carries the
runtime's marker (or the first build's brief) and no `when` is read as `quiet`.
The runtime still never rewrites an organized principal's schedule. A principal
that wants the fixed cadence sets `when: always`. One that knows its owner's
timezone can replace the job with a cron expression and keep `when: quiet`.

Why not a plain `Idle` schedule. It fires once per quiet stretch, which is the
right trigger. But it never fires for a principal nobody talks to, even when its
own cron work and peer posts fill the day. It fires twice on a day with a long
afternoon gap. It has no ceiling for a principal that is never quiet. The 24-hour
interval supplies the floor, the once-a-day spacing and the ceiling, and the
idle detector supplies the timing.

### Weekly synthesis: deferred

A separate weekly job that reads the week's dream files and proposes
`CONVENTIONS.md` edits is not added. Step 6 already reads the last week of
dreams every night and promotes recurring lessons, so a second job would repeat
the same reading at the cost of another seeded job and another turn. Revisit
this if nightly promotion proves too eager or too timid, or if dream files grow
past what one pass reads comfortably.

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
- **Put the dream notes in the prompt** (rev 3). Every passing reflection would
  steer every agent, and nothing would separate a one-off from a lesson. Muse
  keeps them out of the prompt for the same reason.
- **Let the reflection write `CONVENTIONS.md` directly, with no dream file.**
  Leaves no record of lessons that were not promoted, so nothing can recur
  across nights. A promotion threshold needs that record.
- **A nightly cron expression in the machine's timezone** (3 a.m. local). The
  `Cron` tool already defaults cron jobs to the machine's zone. But a hosted
  daemon's zone is not its owner's, and a fixed hour misses owners who keep
  late hours. The principal can still choose this once it knows the owner's
  timezone.
- **A plain `Idle` schedule, with or without a separate 24-hour fallback job.**
  See D9. Two jobs would also need to coordinate to avoid running the pass twice.
- **Change only newly seeded jobs.** Existing principals are the ones whose pass
  runs mid-day. Reading the unchanged marker as `quiet` reaches them without
  rewriting their schedules (ADR-054 D4).

## 4. Consequences

- Every principal gets one maintenance turn a day by default, at the cost of one
  trunk turn a day. A principal that wants none deletes the job once.
- `MEMORY.md` gets a regular chance to stay within its budget and free of
  contradictions, and new entries carry their sources.
- Upgraded runtimes seed the job into existing principals on their next boot.
- Hot memory written by scheduled turns carries checkable sources, and what
  others said reaches it only through the owner. Lines without owner sources pile
  up in `kb/inbox/` until the owner reviews them.
- The memory run cannot use `Bash`.
- Each pass also reflects into `kb/dreams/`, which costs no prompt bytes. Rules
  reach `CONVENTIONS.md` from scheduled runs only when cited to the owner's own
  conversations; others wait in `kb/inbox/`.
- The pass usually runs once a night, after the principal has been quiet for 4
  hours. A principal nobody talks to still gets one every 24 hours. A principal
  that is never quiet gets one at most 36 hours after the last. A daemon restart
  counts as activity for principals with no recorded activity, so their pass
  waits 4 hours after boot.

## 5. Deferred

- Per-turn evidence capture (an `AfterAgent` hook recording what each turn
  touched) for the review to start from.
- A second-model check of proposed memory edits against their sources, and
  compare-and-swap writes to the hot kb files.
- The same source rule for owner runs other than cron turns that read other
  conversations, and for `kb/index.md` and the injected scope notes.
- A weekly dream synthesis job (see "Weekly synthesis: deferred" in §2).
- Recording the owner's timezone so the runtime can seed a true nightly cron
  expression.
