# 2026-08-07 — Peko CLI field test: cron, internal session management, subagent (11 adaptive turns)

**Tester:** automated (Kimi Code CLI, MiniMax-M3 model), acting as a non-technical human user ("Sam")
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, real `minimax-MiniMax-M3` LLM via `$MINIMAX_API_KEY`, host `~/.peko` untouched
**Built binary:** `target/debug/peko` (debug build, v0.1.0, built 2026-08-07)
**Scenario:** multi-turn adaptive conversation focused on this round's three areas: **cron** (natural-language scheduling, one-shot + recurring jobs, cancel), **internal session management** (memory across `peko send` invocations, fresh-conversation discovery, user isolation, on-disk session index), and **subagent** delegation (light probe — covered deeply in the 2026-08-02 report).
**Setup:** `/tmp/peko-cron-session-setup.sh` (one-off isolated setup; daemon left running detached for the adaptive phase; removed in cleanup)
**Logs:** `/tmp/peko/cron-session-logs/01-…` through `11-…` (removed in cleanup; key excerpts inline below)

## Setup

```
HOME                  = /tmp/peko/cron-session-83845-mephnz/home
PEKO_HOME             = …/home/.peko
PEKO_DAEMON_SOCK      = …/run/daemon.sock
model                 = minimax-MiniMax-M3 (`peko model add --template minimax --model MiniMax-M3`)
principal             = helper (`peko principal create helper --model minimax-MiniMax-M3`)
daemon                = `peko daemon start --foreground -v`, ready in ~2 s
```

Notable vs. the 2026-08-02 report: this principal's `/help` output shows the
full **Async* tool family is now granted and bound** (`tool:AsyncSpawn`,
`tool:AsyncOutput`, `tool:AsyncStatus`, `tool:AsyncList`, `tool:AsyncStop`
all in "Allowed extensions" and in the 22 runtime tool definitions). That
closes last round's Finding 2 for default-created principals.

## Conversation transcript (with timing)

| # | User said (paraphrased) | Wall | Iterations / Tokens (in/out) | Outcome |
|---|---|---|---|---|
| 1 | "My name is Sam. Remember: cat = Biscuit, coffee = oat-milk latte." | 5 s | 1 / 7,349 → 40 | Clean confirmation. |
| 2 | *(new `peko send`)* "What's my cat's name, what coffee do I order?" | 4 s | 1 / 6,910 → 29 | **Correct recall across CLI invocations** — session continuity works. |
| 3 | `/new` (trying to start a fresh conversation) | <1 s | — | ❌ `unknown slash command '/new'. Only /help is available in v0.` — Finding 1. |
| 4 | "In about 2 minutes, remind me to water the plants. Set it up for me." | 14 s | 2 / 13,867 → 272 | **Model has no cron tools** — honestly says so, offers phone-alarm workarounds. Finding 2. |
| 5 | "Are you sure? Double-check your scheduling tools, search your catalog." | **64 s → FAIL** | — | `PacketStream timeout … no packet received within 60s` → `❌ Stream closed unexpectedly`. Finding 3. |
| 6 | "Short version: do you have any tool starting with Cron? Yes or no." | 3 s | 1 / 8,409 → 3 | "No." — confirms Finding 2; session recovered cleanly after the aborted turn (good). |
| 7 | *(CLI)* `peko cron at --name water-plants --at <UTC ISO> --announce` | — | — | One-shot scheduled; fired 14 s late (scheduler tick), delivered to `peko log`. Finding 4/5. |
| 8 | *(CLI)* `peko cron every --name heartbeat --interval-ms 60000` | — | — | Recurring job; fired every **75 s**, not 60 s. Finding 6. |
| 9 | "Have one of your helper agents draft a polite 3-sentence hallway note — delegate it." | 21 s | 4 / 34,572 → 380 | **Subagent delegation worked**; separate session file written. Finding 7. |
| 10 | `peko -U alex send helper "what's my name?"` | <1 s | — | ❌ `permission denied: user:alex cannot perform Chat on principal:helper` — secure, but no multi-conversation path. Finding 1. |
| 11 | "Recap: my name, cat, hallway note?" | 5 s | 1 / 9,155 → 56 | All three correct after 11 turns + cron noise. Session memory solid. |

CLI-only probes (not model turns): `peko cron list` / `history` / `remove`,
`peko log helper`, `peko quota status helper`, on-disk inspection of
`sessions.json` / chat logs / announcements.

## Findings

### Finding 1 — There is no way to start a new conversation (session management UX gap)

- `peko send` has **no `--session` / `--new` / `--reset` flag** (full help checked).
- `/new` → `unknown slash command '/new'. Only /help is available in v0.`
- There is **no `peko session` command at all** (`error: unrecognized subcommand 'session'`; the CLI "tip" suggests `version`, which is salt in the wound).
- `-U alex` gives a *permission error* (`user:alex cannot perform Chat on principal:helper`), not a fresh conversation — correct security behaviour, but it means the only conversation a CLI user can ever have with a principal is the single append-only `root:user:local.jsonl`, forever.
- Everything a user says accumulates in one session; turn-1 input was 7,349 tokens and by turn 11 the per-turn input was still climbing (~9–35k depending on tool use). With no reset, long-lived principals will hit compaction or cost ceilings with no user recourse short of deleting files by hand.

**Impact:** high for real usage. Fix sketch: add `peko send --new-session` (or `/new` slash command) that forks `root:user:local` into a new session id, and a `peko session list` surface over the existing `sessions.json` index (the data is already there — see Finding 7).

### Finding 2 — The agent has no cron tools, so natural-language scheduling is impossible

The daemon-side cron engine works (Findings 4–6), and the principal even
holds the `principal:write_cron` + `principal:write_cron_history`
capabilities — but the agentic loop's dynamically built toolset for
`helper` contains exactly 22 tools and **none of them are cron tools**
(daemon log: `Dynamically built 22 tool definitions from ExtensionCore:
["AsyncOutput","AsyncSpawn","Bash",…,"Agent","TaskList","TaskUpdate"]` —
no `CronCreate`/`CronList`/`CronDelete`). Per AGENTS.md these live in
`peko-cron/src/tools/` with a global runtime registry; they are simply
never registered into the principal's ExtensionCore.

User-visible result (turn 4): the model honestly answers *"I don't
actually have a built-in reminder or scheduling tool"* and recommends the
user set a phone alarm — for a runtime whose daemon's raison d'être
includes cron. This is the same described-vs-bound mismatch shape as the
2026-08-02 report's Finding 2, one layer up: the *capability* is granted,
the *tool* is not bound.

**Impact:** high — the single most natural use case for cron ("remind me
to…") is unreachable through conversation. ~~Fix sketch: bind
`CronCreate`/`CronList`/`CronDelete` into `Agent::init_builtins_async`
gated on `principal:write_cron`, mirroring how the Task*/Plan* families
are wired.~~ **Erratum (2026-08-07, post-investigation):** the tools are
already registered by `ToolRuntime::register_builtins`
(`peko-rs/core/src/engine/tool_runtime.rs:209-211`); the actual gate is
the missing `tool:CronCreate`/`tool:CronList`/`tool:CronDelete` grants in
`Capabilities::starter_bundle()` (`peko-rs/extension-api/src/capabilities.rs`).
Fixed by adding the three grants to the starter bundle.

### Finding 3 — A >60 s tool call kills the whole turn: IPC idle timeout aborts healthy runs

Turn 5 asked the model to "search your tool catalog". The model (having no
catalog search tool that covers cron) ran `Bash: find / -type f -name
"config.toml" -path "*peko*"` — a full-filesystem scan that legitimately
took 82 s (07:22:41 → 07:24:03). Meanwhile:

```
2026-08-07T07:23:38Z  WARN peko_core::ipc::stream: PacketStream timeout for request 1: no packet received within 60s
CLI: ❌ Error: Stream closed unexpectedly        (rc=1, 64 s wall)
2026-08-07T07:24:03Z  WARN peko_core::ipc::handlers::principal: failed to send PrincipalSentIteration: Connection refused (os error 61); aborting stream
2026-08-07T07:24:03Z ERROR peko_core::ipc::server: Error handling request 1: Connection refused
```

So the CLI gives up after 60 s of stream silence, and when the daemon
later finishes the (healthy) tool call and tries to emit the next
iteration, the connection is gone and the **entire run errors out**. Any
tool call that takes >60 s without emitting a packet — a big `find`, a
slow `curl`, a long compile — is a guaranteed turn failure in
`--no-stream` mode. No heartbeat/keepalive packets are sent while a tool
executes.

Partial mitigation observed: the session JSONL was left consistent
(dangling tool result, no final assistant message) and the next turn
resumed fine — the failure is per-turn, not corrupting.

**Impact:** high reliability issue. Fix sketch: emit keepalive/progress
packets from the daemon during long tool executions (or tie the
PacketStream timeout to the tool timeout, 300 s, not 60 s of silence).

### Finding 4 — One-shot cron jobs erase their own history

`peko cron at … --announce` worked: the job fired (14 s late — Finding 6),
the reminder landed in `peko log helper`, and an announcement file was
written to `data/runtime/announcements/cron_…_1786087729.json`. But the
daemon then logged `🗑️ Deleting one-shot job 'water-plants' after
successful run`, and afterwards:

```
$ peko cron history cron_3f426633bed044ef88b6536b02e312bd
❌ Error: Failed to get history: Job cron_3f426633bed044ef88b6536b02e312bd not found
$ peko cron remove cron_3f426633bed044ef88b6536b02e312bd
❌ Error: Failed to remove job: Job cron_3f426633bed044ef88b6536b02e312bd not found   (rc=1)
```

A user tidying up after a fired reminder gets an error for a job that
*did* exist and *did* fire; there is no `cron history` record that it
ever ran (history is deleted with the job). The only evidence is buried
in `peko log` and a JSON file in a directory the CLI never mentions.

**Impact:** medium. Fix sketch: keep one-shot history after firing
(mark the job `completed` instead of deleting), or make `cron history`
fall back to archived runs; `remove` on an already-fired one-shot should
be a no-op success or a clearer "already completed and cleaned up".

### Finding 5 — Cron CLI is hidden and its time arguments are machine-format only

- `peko cron` does not appear in top-level `peko --help` (it's marked
  "advanced / hidden") — a non-technical user cannot discover the feature
  at all, and (Finding 2) the model can't reach it either, so cron is
  effectively invisible from both directions.
- `peko cron at --at` requires a raw UTC ISO-8601 timestamp
  (`2026-08-07T07:28:31Z`); `peko cron every --interval-ms` requires
  **milliseconds**. No "in 5 minutes" / "every hour" parsing. This is
  squarely developer-only UX for a feature whose pitch is reminders.
- Job ids are 34-char hex strings (`cron_3f426633bed044ef88b6536b02e312bd`)
  required by `history`/`remove`; there is no `cron remove water-plants`
  by name.
- Minor: `cron remove` prints "(use --force to skip confirmation)" but in
  a non-TTY shell it removed without prompting — the hint implies an
  interactivity that silently didn't happen.

### Finding 6 — Recurring interval jobs drift +25 %: "every 60000 ms" fires every ~75 s

Daemon log for the 60 s heartbeat (tick entries are 15.000 s apart, at
`:x4.718`/`:x9.718` — the cron engine wakes every 15 s):

```
07:26:59.718  Executing job 'heartbeat'     (74.7 s after add)
07:28:14.719  Executing job 'heartbeat'     (+75.0 s)
07:29:29.718  Executing job 'heartbeat'     (+75.0 s)
```

The next-run computation lands on `last fire + 60 s`, which is then
rounded *up to the next 15 s tick*, yielding a stable 75 s effective
period — a permanent +25 % drift, not jitter. The one-shot `at` job due
07:28:31 likewise fired at the 07:28:44.7 tick (14 s late). For
reminders this is harmless; for anything cadence-sensitive (pollers) it
systematically under-fires. Fix sketch: schedule from the *due* time
(not the actual fire time) so quantisation doesn't accumulate.

### Finding 7 — Subagent sessions are written but orphaned in the index (and `turn_count` is always 0)

The turn-9 delegation worked end-to-end (21 s, 4 iterations, note drafted
by the subagent). On disk, the subagent run got its own session:

```json
"af3982b4-…": {
  "agent_name": "root",
  "message_count": 2,
  "turn_count": 0,
  "parent_session_id": null,          // ← spawned by root:user:local, but unlinked
  "peer_type": "principal",
  "peer_id": "spawn_b8168436-…"
}
```

- `parent_session_id: null` for a session that only exists because the
  root session spawned it — session genealogy is lost, so no future
  `peko session list --tree` or cost-attribution-by-conversation can be
  built on this index.
- `turn_count: 0` on **both** sessions despite `message_count: 25` on the
  root session — the counter is never maintained. Dead or broken field.
- Subagent sessions accumulate unbounded: nothing in the CLI surfaces
  them, names them (`title: null`), or cleans them up.

Positive: subagent output flowed back to the parent correctly, and the
subagent's own context (7.8k tokens) stayed out of the parent's session —
the isolation itself is right.

### Finding 8 — `peko log` silently swallows failed turns

Turn 5 (the 60 s timeout, Finding 3) appears in `peko log helper` as the
user's question followed by… nothing. No error entry, no "run failed"
marker — the next line is turn 6's question. A user scrolling their
history sees a question the principal "ignored". Given the log is the
only post-hoc visibility a CLI user has into cron runs and errors, failed
runs should leave a record there. (Minor adjacent nit: log timestamps
render as `2026-08-07 T07:20:39` — stray space before the `T`.)

## Positive findings

- **Session continuity across CLI invocations is solid** — facts seeded in
  turn 1 recalled perfectly in turns 2 and 11, across 11 turns and
  interleaved cron traffic. The JSONL session model does its job.
- **Session survives a hard mid-run failure** — after turn 5's aborted
  stream, turn 6 resumed normally from the JSONL without corruption.
- **Cron engine fundamentals work** — one-shot fired, `--announce`
  delivered to the log + announcement file, recurring job fired 3×,
  `cron remove` stopped it cleanly (no post-removal fire; removal
  confirmed at 07:29:36.302 in the daemon log with no later executions).
- **Subagent delegation works out of the box** — the model used the
  `Agent` tool without prompting detail, isolated context, result
  returned inline.
- **Async* tool family now bound by default** — regression fix vs.
  2026-08-02 Finding 2 confirmed in `/help` and the daemon's tool list.
- **User isolation enforced** — `-U alex` correctly denied
  (`user:alex cannot perform Chat`), no cross-user session leakage.
- **Quota accounting kept up** — `peko quota status helper` after the run:
  119,918 input / 945 output / 15 requests, matching the turn telemetry.
- **Turn wall times are good** — median ~5 s for plain turns, 21 s with a
  subagent, on a debug build.

## Bug/issue list (priority order)

| # | Finding | Severity | One-line fix sketch |
|---|---|---|---|
| 3 | >60 s tool call ⇒ CLI stream timeout + daemon aborts the run | **High** | Send keepalive packets during tool execution, or scale PacketStream timeout to the tool timeout |
| 2 | No cron tools bound to principals despite `principal:write_cron` capability | **High** | Register `CronCreate/List/Delete` in `init_builtins_async` gated on the capability |
| 1 | No new-conversation path: no `--new-session`, `/new` rejected, no `peko session` cmd | **High** | Add `/new` or `send --new-session` + a `session list` over `sessions.json` |
| 4 | Fired one-shot deletes itself *and* its history; later `history`/`remove` error out | Medium | Mark completed instead of deleting; keep history; make repeat-`remove` idempotent |
| 6 | `every 60000ms` actually fires every ~75 s (15 s tick quantisation accumulates) | Medium | Compute next run from due-time, not fire-time |
| 5 | Cron hidden from help; ISO-UTC-only `--at`; ms-only `--interval-ms`; hex-only job ids | Medium | Unhide or document; accept relative times; allow `--name` in history/remove |
| 8 | Failed turns leave no trace in `peko log` | Medium | Write a run-failed entry to the chat log on abort |
| 7 | Subagent sessions have `parent_session_id: null`; `turn_count` never incremented | Low | Link spawn sessions to their parent; maintain or drop `turn_count` |

## Cleanup performed

1. Stopped daemon (pid 83995) — confirmed gone.
2. Removed tempdir `/tmp/peko/cron-session-83845-mephnz` (sessions, chat
   logs, vault, announcements — all inside it).
3. Removed helper artefacts `/tmp/peko-cron-session-setup.sh`,
   `/tmp/peko/cs-env.sh`, `/tmp/peko/cron-session-logs/`.
4. Host `~/.peko` never touched (isolation via `HOME` + `PEKO_HOME` +
   `PEKO_DAEMON_SOCK` held throughout).

---

# Addendum — fix-pack verification (same day, second pass)

All eight findings were root-caused and fixed (see CHANGELOG "Field-test
fix pack"). Verified live against MiniMax-M3 in a fresh isolated env
(`fixpack-*`, daemon `--interval 2`):

| Finding | Fix | Live verification |
|---|---|---|
| 3 (60s timeout) | daemon emits `Heartbeat` packets during the stream drain | 70s `Bash sleep` turn completed in 81s, rc=0 — previously died at 60s |
| 8 (silent failures) | failure branches record `⚠ Run failed: …` to the chat log | accidental check: a provider-400 turn showed its error in `peko log` |
| 2 (no cron tools) | `tool:Cron*` added to `starter_bundle()` | `/help` lists them; daemon logs 25 tool defs incl. `CronCreate/List/Delete`; model successfully used `CronCreate` conversationally |
| 4 (one-shot history) | `delete_job` keeps runs; handler resolves via run records | `cron history <fired-one-shot-id>` returns the run after auto-delete |
| 6 (75s drift) | `calculate_next_interval_anchored` | 10s interval job fired at exactly 10.000s spacing; a slot missed during execution was skipped without bursting |
| 5 (CLI ergonomics) | `--interval 10s`, `--at "in 45s"`, `--name` for remove/run/history | all three verified; `remove --name tick10` worked |
| 7 (index integrity) | entry stamping + `turn_count` + `SessionCreateOptions` default trigger + `AgentTool` prefers `ctx.session_id` | root entry: `trigger='user', turns≥1`; spawn entry: `parent=root:user:local, trigger='spawn'` |

`turn_count` fix note: wiring the field exposed that
`SessionCreateOptions::default()` left `trigger: ""` (now `"user"`), and
that the daemon path's `AgentTool` session-key provider held the
placeholder `agent:root:cli:default` (names no session) — the tool now
prefers `ToolContext.session_id`, which the engine always populates.

## New findings surfaced by the verification pass

### N1 (High) — Interleaved traffic bricks the session: tool_use/tool_result adjacency broken

During the pass, the root session became permanently unusable: every
subsequent turn failed with MiniMax `400 invalid params` (explicit
variant: "tool call result does not follow tool call (2013)"). Cause
visible in the session JSONL: cron-fired messages (`Reply with exactly:
tick`) are injected into the same `root:user:local` session *between* an
assistant `tool_call` and its `tool_result` while a turn is mid-flight,
and consecutive same-role messages also appear. Anthropic-style APIs
require a tool_result to immediately follow its tool_use — the
interleaving violates it, and there is no recovery path (no session
reset; even "say hi" 400s afterwards). Any concurrent writer (cron,
steering, channel) can corrupt the live session this way.

Fix sketch (not implemented — needs design decision): sanitize history
at request-build time in the engine (pair or drop orphan
tool_calls/tool_results, merge consecutive same-role messages), or queue
external injections at iteration boundaries instead of appending
mid-turn.

### N2 (Medium) — The model doesn't know what time it is; past `at` is silently parked 100 years out

Asked to schedule "in 2 minutes", the model guessed the date
(2026-05-14 — weeks stale) and created a one-shot in the *past*.
`calculate_next_run`'s `At` arm maps a past timestamp to the
`after + 100 years` sentinel (meant to park already-fired jobs), so the
job registered "active" with `next_run_at: 2126-07-14` and will never
fire. Two gaps: (a) the agent's system prompt carries no wall-clock, so
relative-time requests are guesswork; (b) cron creation should reject a
past `at` with a clear error instead of the sentinel. (The CLI's new
`--at "in 10m"` path computes from the real clock and is unaffected.)

### N3 (Low) — `peko cron at` confirmation echoes the raw input

`✅ Added one-shot job cron_… at in 45s` — prints the unparsed `--at`
string instead of the resolved UTC timestamp. Cosmetic.

### N4 (Low, known design) — `peko send` sessions are CLI-user-owned, not principal-owned

`peko send fixbot2 …` loaded fixbot's 62-message history — both share
`principals/local/local/sessions/root:user:local.jsonl` (F30 split:
the session belongs to the CLI user `local`). Cross-principal context
bleed is by design, but combined with N1 it means one poisoned session
bricks *every* principal the CLI user talks to.

## Cleanup (verification pass)

Daemon stopped; `/tmp/peko/fixpack-*`, `/tmp/peko/fixpack-logs`,
`/tmp/peko-fixpack-setup.sh`, `/tmp/peko/fx-env.sh` removed. Host
`~/.peko` untouched.

---

# Addendum 2 — N1–N3 fixed and verified (same day, third pass)

Root-cause investigation found N1's true mechanism: **the daemon ran two
independent `InboxRegistry` instances** (`PrincipalManager`'s private one
vs `AppState`'s), so the per-session run permit never serialized cron
turns against CLI turns. Fixes landed per the approved action plan:

| Finding | Fix | Live verification (isolated env, real M3) |
|---|---|---|
| N1 | PM shares `AppState`'s registry (`with_inbox_registry`); in-loop steering drain now persists the user turn at the iteration boundary; new `peko_message::repair::repair_history` (re-pairs tool calls/results, backfills interrupted calls, drops orphans, merges consecutive roles) applied at engine intake | 25s `Bash sleep` turn with a 6s cron job firing throughout: turn completes, JSONL shows `tool_call → tool_result` adjacency preserved, cron injections land at iteration boundaries, and the session answers normally afterwards (the repair pass merges the consecutive queued user messages at load) |
| N2a | volatile `{{current_time}}` prompt placeholder (per-turn suffix, out of the cached prefix) | asked bare "what date/time is it?" the model answered exactly right (Fri 7 Aug 2026, 18:31 local = 10:31 UTC) |
| N2b | `CronScheduler::add_job` rejects `at <= now` (single chokepoint for tool/CLI/IPC); CronCreate schema text updated | `cron at --at 2020-01-01…` → `'at' time … is in the past`; a relative `--at "in 90s"` fired on time and kept its history |
| N3 | confirmation prints the resolved UTC timestamp | `Added one-shot job cron_… at 2026-08-07T10:33:09…` |

N4 remains as documented design (no code change).

Behavior change to note: `peko cron at` with a past timestamp previously
fired immediately on the next poll; it is now rejected.

Tests: 24 peko-message (6 new repair tests), 125 peko-engine, 29
peko-cron, 1495 peko core lib, 116 peko-cli — all green;
`check_workspace_deps.py` clean.

---

# Addendum 3 — InboxRegistry unification made future-proof (same day, fourth pass)

Review of the Addendum-2 N1 fix flagged that `with_inbox_registry` was
an **opt-in override on top of a private default** — the bug was fixed
at the one known call site, but any future construction path would
silently recreate a private registry (exactly how N1 happened). The
shared registry is now a **required constructor parameter**, so the
compiler forces the choice instead of convention:

- `PrincipalManager::{new, with_path_resolver}` take
  `inbox_registry: Arc<InboxRegistry>`; the private default and
  `with_inbox_registry` are deleted.
- `AsyncExecutor::{new, with_registries}` take the registry the same
  way; `with_inbox_registry` and the `Default` impl (which called the
  old private-default `new()`) are deleted.
- `ExtensionAsyncAdapter::new(core)` + `with_executor(core, executor)`
  merged into `new(core, executor)` (was effectively test-only).
- Components that genuinely own a private registry — daemon placeholder
  executors, `BashTool`'s process-global background executor, subagent
  executors, local async transport, per-call CLI scopes, tests — call
  the explicitly named
  `extensions::framework::async_exec::executor::standalone_inbox_registry()`,
  whose doc comment states the daemon composition root must NOT use it.

Latent private-registry paths made visible by this pass (left as-is,
behavior-preserving, candidates for a future follow-up): the `BashTool`
background `OnceLock` executor and the per-subagent `AsyncExecutor`s
build standalone registries, so their inbox-keyed completion delivery
does not reach the daemon-shared inboxes. Their completion flows use
the task registry / task files / announcement channel instead, so this
is not a live bug — but if inbox delivery is ever wanted for them, the
registry must be threaded from `AppState`.

Verification: `cargo check --workspace --all-targets` clean;
`check_workspace_deps.py` clean; test suites green — 1495 peko core
lib, 116 peko-cli.
