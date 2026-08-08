# Non-technical-user field test — round 3: subagent, cron, principal UX (2026-08-07)

**Tester:** automated (Kimi Code CLI), acting as a non-technical human user ("Sam", pottery studio owner)
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK`, real `minimax-MiniMax-M3` via `$MINIMAX_API_KEY`, host `~/.peko` untouched
**Build:** branch `fix/field-test-cron-session-hardening` @ `d42d3df9` (rounds 1–2 fix packs + InboxRegistry hardening, all committed)
**Focus this round:** subagent delegation, cron, principal-centered UX. Long-horizon: one daemon, one principal, ~25 min of continuous mixed traffic.
**Setup:** `/tmp/peko-field3-setup.sh` (isolate.sh pattern; daemon detached with `--interval 2`; env persisted to `/tmp/peko/f3-env.sh`; removed in cleanup)

## Narrative

Sam created principal `nova` (MiniMax-M3), seeded persona + memory facts
(studio "Clay & Ember", Saturday wheel class 10am, favorite glaze celadon,
prefers short answers), then over ~10 conversational turns: asked for a
90-second reminder (cron), a 2-minute reminder, a reminder list, two
parallel helper jobs (subagents), a 75-second wait while a 20s cron tick
fired into the same principal, a memory quiz, and a "what happened
earlier?" recovery probe. Daemon log and on-disk state were inspected
after each phase.

## What works (regression pass on the round 1–2 fixes)

- **Unified InboxRegistry under stress:** 20s cron ticks during the 75s
  `Bash sleep` turn were serialized by the run permit; 3 queued tick
  messages drained together at the iteration boundary; `tool_call →
  tool_result` adjacency in the JSONL was preserved; the session answered
  perfectly afterwards. The N1 session-bricking path stays fixed.
- **Spawn session stamping:** both `Agent`-spawned sessions indexed with
  `parent_session_id: "root:user:local"`, `trigger: "spawn"`.
- **Memory & persona:** all three facts recalled exactly after ~10 turns
  of cron spam; "short answers" preference respected throughout.
- **Subagent delegation:** two sequential `Agent` spawns succeeded with
  good results (caption used the studio name + 10am class; glaze names
  riffed on celadon — context propagated).
- **CLI cron path:** `cron every --interval 20s`, `cron remove`,
  `cron history` after removal (round-1 history preservation) all fine;
  20s interval fired anchored exactly every 20.00s.
- **`{{current_time}}`:** "remind me in 90 seconds" produced a correct
  absolute `at` on the first try.
- **Failure tone:** when tools failed, the model was honest, apologized,
  and offered fallbacks — good non-technical-user handling.

## Findings (priority order)

### F1 (Critical) — Conversational cron tools always fail: in-daemon IPC receiver goes deaf after 60s idle

Every `CronCreate`/`CronList` call from the agentic loop fails with
`Cron add stream closed unexpectedly` after exactly 60s, **even with no
other traffic**. Reproduced 4× across two turns (plus a CronList).

Root cause: `spawn_receiver` (`peko-rs/core/src/ipc/stream.rs:156-168`)
reads with `recv_timeout(CLI_TIMEOUT_SECS=60s)` and on timeout treats it
as "daemon dead": notifies streams and **`break`s — the receiver task
exits permanently**. For short-lived CLI clients this never matters
(connect → request → response → exit, all < 60s). But
`DaemonCronAdapter` (`daemon/cron_runtime.rs:41`) connects **once at
daemon startup** and is used sporadically; its receiver dies 60s after
startup, long before the first cron tool call. Sends still work — the
jobs ARE added (2ms handler execution observed in the log) — but the
`CronAdded` response is never read, so the client waits the full 60s and
bails.

Blast radius check: `DaemonCronAdapter` is the only long-lived
in-daemon `DaemonClient` (`client_service.rs` connects per-call). This
was latent until the round-1 starter-bundle grants
(`tool:CronCreate/List/Delete`) exposed the conversational cron path —
round 2 verified cron via CLI and engine firing, never through a live
tool call.

Consequences observed:
- Model retried the identical failing call up to 3× per turn →
  **duplicate jobs created and fired twice** (two "Flip kiln switch"
  jobs, two "Check kiln temp" jobs); 11 `CronCreate_*` async task files.
- Turns ballooned: 8 iterations / 68k input tokens for one reminder
  request; 3.5 min wall clock.
- The model eventually told Sam to use his phone timer instead — twice.
  The product has a cron feature the model has learned not to trust.

Fix sketch: the receiver's read timeout is not a death signal — replace
`break` with `continue` (liveness is already handled per-request by
`PacketStream::next`'s own 60s timeout, and heartbeats cover streaming
requests). Remove or gate the "Daemon connection lost" fan-out.

### F2 (High) — Cron turns inject into the user's shared conversational session and hijack the turn

Cron `send` jobs targeting principal `nova` append their message to the
**same `root:user:local` session** Sam chats in, as ordinary user
messages (`role_metadata.User.source: "user"` — indistinguishable from
Sam's own messages). During the 75s turn, 3 queued ticks drained at the
iteration boundary right after the tool result; the model answered the
newest message ("Say only: tick" → "tick") and **never completed Sam's
pending request**. Sam asked for a timer and got "tick". On the
follow-up probe the model confabulated ("I think I did reply tick on
time") because the session record looks like it answered.

Side effects: history flooded with tick pairs; each 20s tick costs ~11k
input tokens (full session replay) with quadratic growth as ticks
accumulate; every tick is a full LLM turn.

Fix direction: cron turns should not share the user's conversational
session — separate session per job (indexed `trigger: "cron"`), or tag
injected messages with a distinct source and teach the prompt renderer
to treat them as background, with delivery back to the user via a
notification surface rather than a fake user turn.

### F3 (Medium) — Cron run records are duplicated and never reach a terminal state for spawn_tool jobs

`schedule.toml` `runs[]` shows every run twice with the **same run id**:
first `status: "running", finished_at: null`, then a closing record
appended (not updated). For `send` jobs the closing record is
`status: "success"`; for `spawn_tool` (Agent) jobs it stays
`"running"` forever. Consequences:
- One-shot spawn_tool jobs are never reaped: they linger enabled with
  `next: 2126-07-14…` (the 100-year sentinel) in `peko cron list`.
- `peko cron history` shows confusing duplicate rows (🔄 + ✅ same id).

### F4 (Medium) — Reminder semantics: model picks `spawn_tool`, user never sees the nudge

For "remind me" requests the model chose `action=spawn_tool` (spawning
an `Agent` task) rather than a send-message action. The jobs fired on
time and "succeeded", but with `DeliveryMode::None` nothing ever reached
Sam. The CronCreate schema doesn't steer reminder-style requests toward
a user-visible delivery path.

### F5 (Low) — Spawn session JSONL header disagrees with the index

`session.created` event in spawn session JSONLs says `trigger: "user"`;
the index says `trigger: "spawn"`. Internal inconsistency only.

### F6 (Low, unexplained) — First CronCreate attempt of a fresh turn was handler-delayed ~68s

First-turn attempt 1: request at T+0, add logged at T+68s (later
attempts: 2–3ms). Unblock coincided with the in-flight turn's LLM
stream completion. Not reproduced in the second turn (idle-ish, 3ms).
Possibly first-request init contention; needs a targeted repro.

### Prompting notes

- **P1:** nova's turn-1 self-description promised "specialist helpers
  (writers, researchers, planners)" but `agent_catalog` exposes only
  `primary`. The model handled it gracefully (asked to run
  sequentially), but the product overpromises. Seed specialist
  subagent prompts or tone down the persona claim.
- **P2:** No circuit breaker on identical failing tool calls — 3
  identical CronCreate retries in one turn (68k tokens). Engine-level
  "same tool + same args + same error → stop retrying" or prompt
  guidance.

### UX notes

- **U1:** `peko cron list` leaks the raw 2126 sentinel as "next run".
- **U2:** `peko cron history` duplicate rows per run (see F3).
- **U3 (positive):** persona warmth, memory, honest failure handling,
  and preference adherence were consistently good.

## Token/performance log

| Turn | Iterations | Input tokens | Note |
|---|---|---|---|
| greeting | 1 | 8.1k | |
| memory seeds | 1 | 8.3k | |
| 90s reminder (F1) | 8 | **67.9k** | 2 failed CronCreate + AsyncSpawn detour |
| 2min reminder (F1) | 5 | 47.6k | 1 failed CronCreate |
| list reminders (F1) | 2 | ~20k | CronList timeout |
| delegation catalog check | 2 | 20.1k | |
| sequential delegation | 3 | 31.6k | 2 Agent spawns |
| 75s wait + tick storm | 2 | 22.1k | answered "tick" (F2) |
| memory recall | 1 | 11.2k | session intact |
| recovery probe | 1 | 11.3k | confabulation (F2) |

Root session after ~10 turns: `total_input_tokens ≈ 265k` — dominated by
the F1 retry storms and F2 tick spam.

## Cleanup performed

Daemon stopped (pid file under the tempdir's `run/`); all cron jobs
removed via CLI before teardown; `/tmp/peko-field3-setup.sh`,
`/tmp/peko/f3-env.sh`, `/tmp/peko/field3-logs/`, and the
`/tmp/peko/field3-*` tempdir removed. Host `~/.peko` untouched.

---

## Addendum (2026-08-08): fixes + live re-verification

All findings were fixed on `fix/field-test-cron-session-hardening`
(commit after `d42d3df9`) and re-verified live against real MiniMax-M3
in an isolated environment (`scripts/e2e/lib/isolate.sh`, 2s cron poll).

### Fix map

| Finding | Fix |
|---|---|
| F1 (receiver death) | `ipc/stream.rs` receiver survives idle silence (`continue` on timeout, exit only on genuine socket error with fan-out); `DaemonClient::send_request` fast-fails if the receiver task is dead. Root cause removed entirely: `DaemonCronAdapter` is now **in-process** over the new `daemon::cron_ops::CronOps` (owner-cap gate + schedule/history writes extracted from the IPC handler) — no `DaemonClient` loopback on the conversational cron path. |
| F2 (cron hijack) | Channel-aware root routing: `ChannelKind::Cron` runs in `root:cron:{peer}`; the human's `root:{peer}` stays human-only. Fired Send jobs append a labeled `⏰ [cron job '<name>' fired] …` note (`MessageSource::Cron`, user-role on purpose — Anthropic-style adapters map system-role to the top-level system param, last-one-wins) to the conversational session. |
| F3 (dup rows / no reap) | One row per run: completion closes the start row (`finalize_run`) or attaches the task id (`attach_run_output`) instead of a second `record_run`. One-shot reaping keys on the fire, not `status == "success"`. |
| F4 (no reminder output) | `CronCreate` gained `message` → builds a Send job; schema steers the model and states `prompt` has no user-visible output. Bonus fix during verification: `recurring: false` was silently ignored, and `at` jobs now default to one-shot when no recurrence hint is passed. |
| F5 (trigger mismatch) | Spawn overlays record `trigger: "spawn"` in the JSONL header via `SessionTrigger::from_label`. |
| P1 (specialist overpromise) | Root prompt: catalog is the COMPLETE agent list; never invent specialists; never retry an identical failing call. |
| P2 (retry storm) | `peko-engine` ToolExecutor identical-failure circuit breaker: same name+args failing 2× short-circuits the 3rd call with a stop-retrying tool error. Per-turn scope (executor is per-run), so later turns can retry legitimately. |
| U1 (2126 sentinel) | `peko cron list` renders `next: —` for sentinel `next_run`. |

### Live verification results

| Check | Result |
|---|---|
| "remind me in 90 seconds" (F1) | Turn completed in **7.8s**, 2 iterations, `tools_failed=0` (round 3: 60s+ hangs, 8 iterations, 68k tokens). Job created as `send` with the reminder text — the model picked the new `message` arg unprompted. |
| Reminder fires (F2/F4) | Fired on schedule; turn ran in `root:cron:user:local`; conversational session received only the labeled note (`role_metadata.User.source = "cron"` persisted); raw payload did not leak. |
| Tick storm during 75s sleep turn (F2) | 4 ticks fired mid-turn, all in the cron session; the user's turn finished with the requested joke (round 3: answered "tick"). Conversational JSONL: only human messages + notes. |
| Run history (F3) | One row per fire, all closed `success`; 20s interval anchored (no drift). |
| One-shot reap (F3) | Fired `at` job is **gone** from `cron list` (round 3: parked on year-2126 sentinel). The pre-fix job from the first turn correctly showed `next: —` (U1) until removed. |
| Subagent (P1) | Model guessed `Agent(type="writer")` once, got the typed "not found" error, recovered by doing the work itself, and told the user plainly that only `primary` exists. Residual: the schema/prompt still invites one guessed-name attempt before recovery — acceptable, but a future tweak could list valid types in the `Agent` tool error (it already does) and the model did read it. |
| Circuit breaker (P2) | A later turn hit two *different* CronCreate failures (past `at`, malformed RFC3339 — model time-handling, caught by the round-2 N2b guard) and was correctly NOT blocked, since the breaker only trips on byte-identical calls. |
| F6 (first-attempt delay) | Not reproducible post-F1: first turn answered in 7.8s. |
| Notes surface in chat (UX payoff) | "did I miss anything?" → accurate summary of fired reminders + haiku, 1 iteration, 9.8k input tokens. |

Token comparison, reminder turn: round 3 = 67.9k input / 8 iterations → round 4 = 16.8k input / 2 iterations.

### Remaining observations (not blocking)

- **Model time math is the top remaining turn-cost driver.** Two of five
  conversational turns spent extra iterations on `at`-timestamp mistakes
  (past time, malformed RFC3339) despite the `{{current_time}}` prompt
  placeholder. The schema already nudges "in N minutes" requests toward
  `interval_ms`; strengthening that nudge (or accepting relative
  durations natively in `CronCreate`) would cut most of it.
- Cron turn replies see prior cron-session history and produce
  continuity-aware text ("You've got 90 seconds until the next nudge") —
  working as designed; note excerpts truncate at 500 chars by design.

### Regression coverage added

- `ipc/stream.rs::test_receiver_survives_idle_silence`
- `peko-cron`: `test_run_row_attach_then_finalize_single_row`,
  `test_resolve_one_shot_derivation`
- `cron_engine`: `test_send_job_isolates_cron_session_and_delivers_note`,
  `test_one_shot_spawn_tool_job_reaped_after_failed_fire`
- `tests/cli_cron.rs`: `cron_agent_tool_create_message_makes_send_job`
- Suites: peko-cron 31, peko-engine 129, peko-session 221 (+3 doctests
  fixed, stale `peko_core::` paths), peko-message 24, peko lib 1498,
  peko-cli 116 — all green; `check_workspace_deps.py` clean.

### Cleanup (round 4)

Daemon stopped; `/tmp/peko/field4-*` tempdir, `/tmp/peko-field4-setup.sh`,
and `/tmp/peko-field4-env.sh` removed. Host `~/.peko` untouched.
