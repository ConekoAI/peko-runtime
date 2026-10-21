# ADR-083: Quiet Ticks for Attention-Gated Cron Jobs

**Status:** Accepted
**Date:** 2026-10-16

**Amends:**
- [ADR-054](ADR-054-principal-genesis-pipeline.md) P4: the seeded keepalive no
  longer runs a full turn on every tick.

**Amended by:**
- [ADR-086](ADR-086-peer-posts-bring-the-keepalive-forward.md): a post by
  another principal brings an attention-gated keepalive's next fire forward.

**Builds on:**
- [ADR-072](ADR-072-human-started-runs-and-durable-delivery.md): only humans and
  cron start runs; other principals' posts wait on the waiting list (D4a).
- [ADR-080](ADR-080-daily-memory-maintenance.md): the daily memory job gives
  every principal at least one review turn a day.

---

## 1. Context

Every principal gets a keepalive: a trunk `Send` every 10 minutes, about 144
full LLM turns a day, each carrying the hot kb and the session context. Most
find nothing to do. Persistent bots elsewhere separate cheap background checks
from full agent turns. The keepalive's job is to notice things no run was woken
for: other principals' posts (which never wake anyone), group posts nobody was
mentioned in, standing work, and failures. Every one of these can be checked
without a model.

## 2. Decision

### D1 — A message job can wait for attention

A cron `Send` action may carry `when`:

| `when` | A due fire… |
|---|---|
| `always` (default) | runs its turn |
| `attention` | runs its turn only when something waits on the principal, or when the job's last turn is an hour old (`ATTENTION_MAX_QUIET_MS`); otherwise it is a **quiet tick** |

`Cron` `create` takes `when` with `message`; `update` changes it. `attention`
needs a recurring schedule; a one-shot always runs.

ADR-080 D9 (2026-10-16) adds a third value, `quiet`: a due fire waits until
nobody has talked to the principal for 4 hours, at most 12 hours past due.

### D2 — What counts as waiting

The engine checks, without a model and without changing anything, in the session
the job fires in (the trunk, or its creating session):

1. **Channel posts.** The posts that session's channel digest would show:
   other principals' posts on its waiting list (ADR-072 D4a) and unread
   activity. The digest's read marks and waiting lists are not touched.
   Showing and consuming them is the turn's job.
2. **Standing work.** For an owner run, the steps of open plans that are ready
   or in progress. *Amended by [ADR-084](ADR-084-goals-as-plans-and-check-ins.md):*
   a future `check_at` snoozes such a step, and a check-in that came due since
   the job's last turn also counts. Since 2026-10-18 (ADR-084 D2), a step
   counts only when its plan changed after the job's last turn.
3. **Failures.** Another of the principal's jobs whose last run failed after
   this job's last turn.

Pending proposals and owner approvals do not count. They wait on the owner,
and counting them would keep every tick busy until the owner acted.

### D3 — A quiet tick leaves a light trace

A quiet tick advances `next_run` and sets `last_status` to `quiet`. It writes
no run row, so the 1000-row run history is not flooded and other jobs' history
survives. It leaves `last_run`, `run_count` and the failure counter untouched,
so `last_run` stays the time of the last turn. It emits a `cron.quiet` audit
event. Manual `trigger` always runs. A fire whose job cannot fire (no stamp, or
its conversation gone) is never quiet, so the failure shows in its run.

### D4 — The keepalive waits for attention

New seeded keepalives carry `when: attention`. A `keepalive` job that still
carries the runtime's tick text and no `when` (seeded before this ADR) is read
the same way. This decision is made when the job fires; the runtime still
never rewrites an organized principal's schedule (ADR-054 D4). A principal
that wants every tick sets `when: always` with `Cron` `update`. The tick text
says so, and asks the principal to keep standing work in `Plan` so it counts.

The hourly floor keeps habits the trunk keeps on its heartbeat (checks it runs
every tick without a plan) running, an hour apart instead of ten minutes.

## 3. Alternatives considered

- **Keepalive as a `Workflow` that calls `ModelCall` when the check is
  ambiguous.** It would let the principal script its own check. But it moves the
  heartbeat out of the cron/trunk path every other piece relies on (stamps,
  admission, idle), and the checks here need no judgment.
- **A cheap triage model call per tick.** It still costs a call every 10
  minutes, and the inputs are already structured.
- **Count everything unread, including owner-facing items.** Proposals and
  approvals sit until the owner acts, so the keepalive would never be quiet.
- **Keep quiet ticks as run rows.** 144 rows a day would push the memory job's
  history (which the memory brief reads) out of the 1000-row window within a
  week.
- **No floor.** A principal that relies on per-tick habits would stop silently.
  An hour bounds that, and costs at most 24 turns a day.
- **Apply only to newly seeded keepalives.** Existing principals are the ones
  paying for the 144 turns. Reading the runtime's own unchanged tick text as
  `attention` reaches them without touching their schedules.

## 4. Consequences

- An idle principal's keepalive costs at most 24 turns a day instead of 144,
  plus one turn per fire where something waits.
- The peer-post latency ceiling stays the keepalive cadence (10 minutes by
  default). A tick with a waiting post always runs.
- `Cron list` shows `last_status: quiet` between turns; `history` lists only
  turns.

## 5. Deferred

- A `check_at` on plan nodes that schedules a one-shot follow-up.
- Pending async completions as a waiting signal. Completions already wake the
  session that awaited them.
- A per-job quiet ceiling instead of the fixed hour.
