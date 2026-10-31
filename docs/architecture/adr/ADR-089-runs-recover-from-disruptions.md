# ADR-089: Runs recover from disruptions on their own — they park and continue, and nothing about it reaches the conversation

**Status:** Accepted (2026-10-31)
**Date:** 2026-10-31

**Preserves:** [ADR-072](ADR-072-human-started-runs-and-durable-delivery.md) (only humans and cron start runs — a continuation resumes a run one of them started), [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) (the DM channel is the conversation record), [ADR-083](ADR-083-quiet-ticks.md) (the keepalive calls no model when nothing waits).

---

## 1. Context

A peko is meant to keep going without its owner. Three disruptions stop it today: its own spending limit, the provider's quota, and the provider being unreachable. Verified on `master` @ `002c7f0a`:

| # | Fact | Where |
|---|---|---|
| F1 | A turn retries a transient error in place: transport retries (5, 1 s doubling, capped at 30 s) and stream retries (3), sharing one budget of 8 attempts. After that the turn fails. | `provider-api/retry_config.rs`, `engine/agentic_loop.rs` (`'stream_retry`) |
| F2 | Errors are classified by text. Transient: 429, 5xx, 529, "timeout"/"timed out", "connection reset", "refused", "connection error". Terminal: `insufficient_quota`, `quota_exceeded`, `billing_hard_limit`, `usage_limit_reached`, `usage_not_included`, `server_is_overloaded`/`slow_down`, 401, 400/413. **Anything else is `InvalidRequest`** — a 403 without a quota word in its body, or a DNS failure, fails the turn at once. | `provider-api/retryable_error.rs` `from_msg_substring` |
| F3 | The peko's own limit is typed (`QuotaError`) and its reset is known: the meter's window end. A call over the limit is refused before it is made; the call that crosses it stands (#531). | `quota/meter.rs`, `engine/stacked_metered_provider.rs` |
| F4 | Nothing resumes a failed turn. The session waits for the next message or cron fire. A failed cron turn bumps `consecutive_failures`; the quiet tick then lists "another job failed". | `principal/child_turns.rs`, `cron/lib.rs`, `principal/quiet_tick.rs` |
| F5 | A failed DM turn posts `⚠ Run failed: <error>` into the DM channel — the conversation record, which `peko log` and the desktop show. | `ipc/handlers/principal.rs` (`RUN_FAILED_PREFIX`, four sites) |
| F6 | When the client's stream drops mid-run, the runtime **aborts the turn** and posts `⚠ Run failed: connection lost mid-run`. | same, heartbeat- and send-abort branches |
| F7 | A turn saves as it goes: the user message at the start, then each completed iteration's assistant message and tool results. A failure leaves the completed iterations in the session; the interrupted iteration's partial output is not saved. | `engine/agentic_loop.rs` (`add_user`, per-iteration `assistant_with_blocks`) |
| F8 | A run can continue a session without a new user message, from its saved history (repaired: a tool call without a result gets an `is_error` result, so the history stays valid). | `AgenticLoop::run_streaming_with_resume_skip_user_add`, `peko_message::repair::repair_history` |
| F9 | Messages that arrive while a session has a run are saved and queued as steering; the run (or a successor run) takes them at its next iteration. | `ipc/handlers/principal.rs` `drain_pending_steering`, `run_steering_successor` |

So a disruption longer than about two minutes ends the turn, writes the failure into the conversation, and leaves the peko idle until someone writes again.

## 2. Decision

### D1 — Three disruptions, told apart after the turn's own retries

The in-turn retries of F1 stay. When they give up, or a call fails in a way they don't retry, the error is sorted into one of:

| Disruption | Recognized by | Reset known |
|---|---|---|
| **Peko limit** | `QuotaError` from the peko's own meter (F3) | yes — the meter's window end |
| **Provider quota** | the quota family of F2 (`insufficient_quota`, `quota_exceeded`, `billing_hard_limit`, `usage_limit_reached`, `usage_not_included`), and a 403 or 429 whose body names a quota, limit, or billing | no |
| **Provider unreachable** | the transient family of F2 after the in-turn retries, plus what F2 misfiles: DNS failures, `error sending request`, TLS and connection errors, a stream that ends without a stop reason, `server_is_overloaded` / `slow_down` | no |

Everything else stays a **failure**: 400/413, 401 (credential rotation already handles it), a 403 that is not about quota, a spec-gate refusal, a context window still too small after eviction, the output-limit recovery giving up. A failure ends the run as today, except for D5.

The classifier keeps its "unknown → `InvalidRequest`" default for request-shaped errors, but transport-shaped errors (no HTTP status at all) default to **unreachable**: an unknown network error is far more likely to pass than a malformed request is to fix itself.

### D2 — A disrupted run parks; it does not fail

A run hit by a disruption **parks**:

- The session keeps what F7 already saved. The interrupted iteration's partial output is dropped (as today).
- The session records its parked state: `{ disruption, since, attempts, next_attempt_at }`, in the peko's `<local>/parked.json` (amended: one small file per peko rather than session metadata — the scheduler reads every parked run at once), so it survives a restart.
- The run ends and frees its concurrency slot. A parked session holds no slot, only its place.
- A message that arrives for a parked session is saved and queued (F9) and does not start a second run. For an unreachable or quota-parked session, it brings the next attempt forward to now — at most once a minute — because a person writing is a good moment to check. A peko-limit park keeps its schedule: the limit has not reset.
- `peko stop` on a parked session abandons the continuation (the queued messages stay in the session for the next run).

### D3 — A continuation picks up where the run stopped

At `next_attempt_at` the session continues through F8: its saved history, repaired, plus any queued messages, with no new user message. It never replays the user message, and completed tool calls are not run again — their results are in the history. A continuation is the same run resumed, under the same acting-for stamp and the same role, not a new run (ADR-072).

If it hits the same disruption again, it parks again with `attempts + 1`. If it hits a failure, it ends as a failure (D5).

### D4 — When the next attempt comes

| Disruption | Next attempt |
|---|---|
| Peko limit | at the meter's reset (window end). No probing before — the limit is the owner's decision. |
| Provider quota | 1 min, doubling, **capped at 24 h**, ±10 % jitter, indefinitely. A `Retry-After` or reset header, when the provider sends one, wins. |
| Provider unreachable | 30 s, doubling, **capped at 1 h**, ±10 % jitter, indefinitely. |

Provider waits are kept **per lane** — one model endpoint and credential — not per session. While a lane is down, one session probes it at a time; the others wait on the lane. When a probe succeeds, every session parked on that lane continues, a few seconds apart. When it fails, the lane's wait grows once, not once per session.

Only calls that reach the model cost anything; a failed attempt with no usage charges nothing.

### D5 — Nothing about a disruption or failure reaches the conversation or the model

- The `⚠ Run failed` posts (F5, F6) are removed. The DM channel holds what people and pekos said, nothing else.
- No retry, park, or continuation notice is written to the session. The model sees the conversation as if the outage never happened; a continuation's runtime context may note the time, as every turn's does.
- The state is shown beside the conversation instead. `principal_get` (IPC) and `peko show <peko>` report each parked session: disruption, since, attempts, next attempt. The desktop shows it under the chat ("Waiting — provider unreachable; next try 10:42", with **Try now** and **Stop**) and on the peko's row. A failed run is reported the same way until the next message, so a waiting client still sees why it got no answer.
- `peko log` prints a parked or failed run as a status line that is not part of the transcript.
- What needs the owner — a failure that is a credential or permission problem, or a peko limit — is raised as an owner notification (desktop notification), not a message in a conversation.

### D6 — Scheduled work waits with the run

A cron fire for a peko whose model lane is down, or whose own limit is spent, is **deferred**, not run and failed: it is recorded as deferred (not a failure, so `consecutive_failures` does not grow) and fires once the lane is back or the limit resets. The keepalive (ADR-083) treats a parked session as nothing to do — the continuation will run on its own.

### D7 — A run outlives the client that started it

A dropped client stream (F6) no longer aborts the turn. The run finishes, and its reply lands in the DM channel as any reply does; a client that reconnects reads it there. Stopping a run is an explicit `principal_stop` / `peko stop`.

### D8 — Subagents wait in place (amended 2026-10-31)

A delegated run (`Agent`, and other runs that are not a conversation turn — cron, wakes) hit by a disruption **waits in place**: it sleeps the D4 wait (the peko's own limit until it resets) and continues from its saved history, until it goes through, fails, or is cancelled. Its parent's `Agent` call returns the receipt it returns today for a long run, and the result is delivered to the parent's session when it finishes (the existing async-completion path). A parent never parks because a child did.

*Amended from "subagents park on their own":* parking a child would need the continuation to run it under its own role and route its result back through the task registry; waiting in place gets the same behavior from paths that exist, at the cost of holding a concurrency slot while it waits and not surviving a restart.

## 3. Alternatives considered

- **Retry inside the turn for longer.** Holds a concurrency slot and the client stream for hours, and dies with the process. Parking is durable and costs nothing while waiting.
- **Fail and let the next message or cron fire retry.** Today's behavior: the peko stops until someone acts, and the failure text pollutes the conversation.
- **Write a short "I was offline" note into the session after recovery.** Gives the model context it does not need and changes what it says; the owner sees the state beside the chat instead.
- **Per-session backoff.** N parked sessions would each probe a down provider; per-lane waits make one probe.

## 4. Consequences

- A peko that loses its provider for a day continues by itself when it returns, without a message or a restart, and its conversations read as if nothing happened.
- An owner who sets a budget gets the work done at the next cycle instead of a failed turn.
- A misclassified permanent error (a quota that never comes back) retries once a day forever. That is cheap, visible in the status, and stoppable.
- Clients must show parked and failed state from status, since the conversation no longer carries it.
- Removing the run-failed posts changes `peko log` and the desktop, which today rely on them to explain a missing answer (Finding 8, 2026-08-07); D5's status line takes that job.

## 5. Implementation

1. **Classifier.** A `Disruption` result beside `RetryClassification`, with the D1 table; transport-shaped unknowns default to unreachable; 403/429 quota bodies to provider quota. Tests per row.
2. **No conversation traces, runs outlive clients.** Remove the `RUN_FAILED_PREFIX` posts; a dropped stream no longer aborts the child turn (D5, D7). Report the last failure in `principal_get`.
3. **Parking and continuation.** Parked state in `<local>/parked.json`; ingress to a parked session queues; a daemon scheduler runs continuations as turns with an empty message, which the loop reads as "continue from the saved history"; `peko stop` abandons (D2, D3).
4. **Lanes and schedules.** Per-lane wait and single probe; peko-limit wake at the meter's reset (D4).
5. **Cron and subagents.** Deferred fires; delegated runs wait in place and deliver through the async-completion path (D6, D8).
6. **Surfaces.** `principal_get` / `peko show` / `peko log` status line; desktop banner with Try now / Stop, row indicator, owner notifications (D5).

Each step ships on its own; 1 and 2 alone already stop the conversation pollution and the client-drop aborts.
