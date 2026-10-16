# ADR-084: Goals as Plans, and Check-ins

**Status:** Accepted
**Date:** 2026-10-16

**Amends:**
- [ADR-083](ADR-083-quiet-ticks.md) D2: plan work that wakes a heartbeat.

**Builds on:**
- The `peko-plan` DAG and the `Plan` tool; the open-plans tail section every
  owner run sees.
- [ADR-080](ADR-080-daily-memory-maintenance.md): the nightly memory pass, whose
  brief is supplied when it fires.
- [ADR-072](ADR-072-human-started-runs-and-durable-delivery.md): only humans and
  cron start runs.

---

## 1. Context

Muse's headline feature is goals: describe one once, and the agent keeps a plan
current and checks back when something is due. Peko had the parts but not the
whole:

- `principal.toml [intent].goals` are prose strings. Nothing turns them into
  tracked work.
- Plan steps have no notion of "come back to this later". A step waiting on a
  reply or a date looks the same as one to start now.
- Under ADR-083 every ready or in-progress step made each attention-gated tick
  run. A principal with a step parked for next week paid a full turn every ten
  minutes, and could do nothing to quiet it.

## 2. Decision

### D1 — A plan step can carry `check_at`

`PlanNode.check_at` (`checkAt` on disk and in the tool, optional) says when to
come back to the step.

- **Setting it.** `Plan` `create` (per node), `add_step` and `mark_step` take
  `checkAt`: an RFC 3339 time, or a duration from now (`90m`, `2d`, parsed like
  cron's `delay`) so the model does no clock arithmetic. In `mark_step` an
  empty string clears it.
- **Completing clears it.** Marking a step completed drops its `check_at`.
- **Before the time.** A ready or in-progress step is snoozed.
- **After the time.** The check-in is due, whatever the step's status.
  `PlanRecord::due_check_ins` and `PlanRecord::active_nodes` express this.
- The open-plans section lists due check-ins first, each once, and shows
  `check at <time>` beside the steps that come back later.

### D2 — Plan work that wakes an attention-gated heartbeat (amends ADR-083 D2)

For an owner run, plan work counts as waiting when:

1. a ready or in-progress step is not snoozed by a future `check_at`; or
2. a check-in came due after the job's last turn (any due one, when the job has
   had no turn).

A due check-in therefore wakes the heartbeat once. After that turn it stays in
the plans section until the principal acts, reschedules it, or clears it, but it
does not wake the principal again. This mirrors how ADR-083 counts failed jobs.

### D3 — No job of its own per check-in

Check-ins ride the attention-gated heartbeat. The seeded keepalive checks every
10 minutes and stays quiet otherwise, so a due check-in lands within one cadence
(and the nightly memory pass sees it too). Setting `checkAt` does not create a
cron job.

### D4 — Goals are kept as plans

- **New principals.** The genesis brief asks the trunk to turn each goal into a
  plan: the goal as its title, the next concrete steps as its nodes, and
  `checkAt` on steps that wait on a time or a person. It does not start the
  work.
- **Existing principals.** The nightly memory brief (supplied when the job fires,
  ADR-080 D5) lists the principal's goals and asks it to create a missing plan,
  add the next steps the day revealed, close a plan whose goal is met or
  dropped, and set `checkAt` on waiting steps. Goals stay owned by
  `principal.toml`; plans are how they are tracked. The runtime does not link a
  plan to a goal.
- The keepalive tick text mentions `checkAt` for newly seeded keepalives.

## 3. Alternatives considered

- **A one-shot cron `Send` per check-in.** It would wake the principal even
  without a heartbeat. But it spreads jobs across the schedule, has to follow
  every reschedule, clear and completion (or fire stale), and couples the plan
  tool to cron. The heartbeat already exists for exactly this, and a principal
  that removes its heartbeat has opted out of acting on its own.
- **Waking on a due check-in until it is handled.** Like the ADR-072 waiting
  list, but a check-in the model looks at and decides to leave would then cost a
  turn every tick. Once per due time bounds the cost, and the plans section
  keeps it visible.
- **A goal registry linked to plans.** It adds a second source of truth beside
  `[intent].goals` and the plans themselves. The briefs keep the two in step
  through ordinary turns.
- **Keeping ADR-083's rule (every ready step wakes).** Then there is no way to
  park work, and plans make the heartbeat noisier the more the principal plans.

## 4. Consequences

- Principals can park work: "check back on Friday" costs nothing until Friday.
- A principal's goals become visible as plans in every owner run, with due
  check-ins at the top.
- Precision of a check-in is the heartbeat cadence (10 minutes by default).
- Plan records gain an optional field; older records read unchanged.

## 5. Deferred

- Warning when `checkAt` is set but no attention-gated heartbeat exists.
- Telling the owner about a due check-in that needs them (the principal can
  already `ChannelSend`).
- Recurring check-ins (a review every Monday) as a step property.
