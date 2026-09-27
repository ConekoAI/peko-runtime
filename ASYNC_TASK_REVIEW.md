# Review: Peko async tool execution

**Date:** 2026-09-27
**Scope:** the async tool-calling mechanism —
`peko-rs/core/src/extensions/framework/async_exec/**`,
`peko-rs/core/src/tools/builtin/async_control/**`,
`peko-rs/engine/src/async_completion.rs`,
`peko-rs/extension-api/src/async_*.rs`,
plus the wiring in `agentic_loop.rs`, `ipc/handlers/`, `daemon/mod.rs`.
**Design baseline:** ADR-040 (tool timeout & async refactor), ADR-020
(daemon-based async execution).

**Explicitly out of scope:** `peko-rs/core/src/daemon/background_runtime/**`.
Despite the name it is not part of this mechanism — it supervises long-lived
infrastructure (MCP server child processes and external endpoints) and never
touches the session inbox, the agentic loop, or any `Async*` tool. It is
mentioned here only so the omission is deliberate rather than accidental:
nothing in §3–§6 depends on it.

---

## 1. Verdict

The capability is **real and mostly coherent, but it is not yet
production-grade for multi-principal use.** The happy path works: spawn a
tool, get a receipt, and — if the run is still in flight — have the result
folded into the next agentic iteration as a synthetic user message. That
round-trip is genuinely implemented and is the right architecture.

Beneath the happy path there is a cluster of defects that share one root
cause: **the subsystem was built up incrementally (F37 funnel, F38 cancel
signal, ADR-061 session stamping) and neither the old paths nor the old
delivery plumbing were removed when the new ones landed.** The result is two
parallel delivery stacks, an async layer with no backpressure and no
durability, and a task registry with no ownership model.

**The headline gap is delivery semantics, and it is easy to assume away.**
The natural reading of "background task" is *the calling agent gets awakened
if idle and steered if running*. Neither half holds. A running agent gets a
`CompletionEvent` folded into its next iteration — not a steer, and only if
another iteration happens. An idle agent gets **nothing**: there is no
completion-driven wake anywhere in the codebase. The only async work that
receives true steering treatment is **cron**. See §1.1.

Four issues are severe enough to be user-visible today (§3, P0). The rest are
the normal accumulation of a subsystem that has been patched but not
consolidated.

Two structural notes that change how to read §3:

- **One whole subsystem is dead.** The IPC async-spawn transport — the
  "CLI hands background work to the daemon" machinery from ADR-020 — has no
  live producer, because ADR-021 moved all execution into the daemon and the
  CLI is now just an IPC client. It should be deleted, not fixed (§3-D, D1).
  My initial severity call on it was wrong; it is recorded as latent.
- **The most consequential finding is P0-1**, added late: the daemon's
  `AsyncExecutionRouter` is wired to a *standalone* inbox registry because
  `cli/main.rs` installs a global core before `AppState` can install the
  correct one. Completions from router-dispatched tools therefore land where
  nothing drains them.

**Recommendation:** fix the four P0s, then close the §1.1 delivery gap
(§6.1b) — it is the difference between "background" and "fire and forget" —
then do one consolidation pass (§6.2) that deletes the dead delivery stack
and gives the task registry a principal identity + concurrency bound. The
consolidation is worth more than any individual fix.

---

## 1.1 The delivery gap: completion is not steering

This is worth its own section because the code *looks* like it does what you
would expect, and the flag names actively suggest it does.

**Two different things live in the same inbox.**

| | `AsyncInboxItem::Completion` | `AsyncInboxItem::Steering` |
|---|---|---|
| Produced by | async task terminal state (`executor.rs:474-487`) | user IPC `Steer`, channel messages, cron (and cron-derived task completion) |
| Consumed by | agentic loop's per-iteration drain only | agentic loop drain **and** the successor-run machinery |
| Can start a turn? | **No** | **Yes** — `run_steering_successor` (`principal.rs:2588-2620`), `drive_turn` (`channel_binding.rs:528-574`) |

**What actually happens when a task completes**

- **Run in flight** → drained at the top of the next iteration
  (`agentic_loop.rs:1131-1201`) → injected as a synthetic `User` message with
  one `ToolResult` block per event. The agent is nudged, but this is *not* a
  steer: nothing extends the run, and if the completion lands during the final
  iteration it is never seen at all.
- **Agent idle** → **nothing happens.** There is no completion-driven wake:
  - `SessionInbox` holds an `Arc<Notify>` and calls `notify_one()` on every
    push (`inbox.rs:87, 120, 126`), but there is **no `notified().await`
    consumer anywhere in the workspace** (only unrelated ones in
    `parallel_gate.rs`, `cli/commands/send.rs`, `daemon_process_service.rs`).
  - The daemon has no completion watcher — its only background ticks are cron
    poll (15 s), idle check (1 min), async janitor (1 h)
    (`daemon/mod.rs:423-426`).
  - `try_acquire_run`'s two production call sites are the steering successor
    (`principal.rs:2755`) and the session-delete guard
    (`session_runtime_impl.rs:960`); neither is completion-driven.
  - The result waits for the next user message, and P0-2 may delete it first.

**Why `wake_on_completion` misleads.** It defaults to `true`
(`mod.rs:147-151`) and reads like a promise. But the steer branch requires
`principal_root_session_key` to be `Some` (`executor.rs:449-459`), and that
field defaults to `None` (`mod.rs:159`). Grepping every production setter:

- `cron_engine/mod.rs:897` — `Some(caller_session_key)` ← **the only one**
- `subagent_executor.rs:2022` — `None`
- `AsyncSpawn` → `AsyncToolConfig { ..Default::default() }` — `None`

So `wake_on_completion` is **inert for every agent-spawned task**. Cron is the
sole path where a task completion becomes a real `SteeringMessage` and can
drive a successor turn.

**Consequence.** For `AsyncSpawn`, "background" currently means *the model
must remember to poll `AsyncOutput`*. That is the gap between the feature as
named and the feature as built.

---

## 2. What exists

### 2.1 The agent-facing surface

Five tools in `tools/builtin/async_control/`, thin wrappers over the
`AsyncRuntime` port (`mod.rs:288-305`):

| Tool | Purpose | Notes |
|---|---|---|
| `AsyncSpawn` | run any tool in background | `{tool, params, label?, wake_on_completion?=true, timeout_secs?}` |
| `AsyncOutput` | read result | `{task_id, block?=false, timeout?=300000ms, tail_lines?=0}` |
| `AsyncStatus` | single-task status | `{task_id}` |
| `AsyncList` | list tasks | **`status_filter` / `tool_filter` only — no session or agent filter** |
| `AsyncStop` | cancel | `{task_id}`; already-terminal → `success:true` (Claude-Code `TaskStop` shape) |

All five are constructed per-agent in `Agent::init_builtins_async`
(`agents/agent.rs:1670-1731`). The capability gate is `tool:<name>`, and the
default principal bundle ships `tool:*`, so in practice they are always
exposed. The per-agent `enable_async_tools` gate was dropped
(`agent.rs:1627-1632`).

### 2.2 The state machine

Six states, defined once and cleanly —
`extension-api/src/async_status.rs:17-25`:

```
Pending → Running → Completed { result } | Failed { error } | TimedOut | Cancelled
```

Terminal set at `async_status.rs:120-128`; wire forms at `:131-140`. This part
is well done. There is **no enforced transition table** — `update_status` is a
blind assignment (`executor/registry.rs:269-278`) — and `Pending` is
effectively unobservable (the receipt hardcodes it while the spawned task flips
to `Running` immediately).

### 2.3 The round-trip (this is the good part)

```
AsyncSpawn → AsyncExecutor::dispatch_tool → tokio::spawn + tokio::time::timeout
   → on terminal: registry.update_status + task_file write
   → inbox_registry.get_or_create(parent_session_key).push(Completion(event))
   → agentic_loop drains at top of NEXT iteration  (agentic_loop.rs:1131-1201)
   → build_async_completion_message → synthetic User message with
       one ToolResult block per event, preview truncated to 2048 bytes
```

The drain runs at the start of every iteration; mid-iteration arrivals wait
(`agentic_loop.rs:1153-1154`). Results are filtered to the current session
(`async_completion.rs:138-141`). `Agent`-tool completions are additionally
persisted to the parent JSONL (`async_completion.rs:194-221`).

This design is correct and is a genuine improvement over ADR-040's original
`AsyncTaskCompletionQueue`, whose `process_queue()` the ADR itself flagged as
"defined but never called".

**But note what this path is *not*.** It is not steering, and it is not a
wake. It is a passive drain that only helps when a run is already in flight —
so the round-trip above covers one of the two cases a user would expect from
"background". See §1.1 and P1-6.

---

## 3. Findings

### P0 — user-visible correctness bugs

**P0-1 · Daemon's `AsyncExecutionRouter` is wired to a standalone inbox
registry, so completion delivery is silently misrouted.** *(New — found while
tracing the IPC path; see also D1.)* `AppState::new` hoists the daemon-shared
inbox registry and
builds a correctly-wired router with `create_local_transport_with_inbox(..)`
(`daemon/state.rs:622-646`) — but only if no global core exists yet:

```rust
} else if let Some(existing) = crate::extensions::framework::core::global_core() {
    tracing::info!("Reusing global ExtensionCore initialized by main.rs");
    existing
```

`cli/src/main.rs:44` runs `init_extension_core` **before** `run_command`, and
for `Commands::Daemon(_)` it installs a core built with
`create_local_transport()` (`main.rs:93`) — which uses
**`standalone_inbox_registry()`** (`async_transport.rs:320-324`), a private
registry nobody drains. So the daemon reuses the *wrong* core, and the
carefully hoisted registry is discarded.

Impact: any completion pushed by a router-dispatched tool lands in the
standalone inbox instead of the one the agentic loop drains. This is exactly
the failure mode the code documents twice —
`state.rs:615-620` ("otherwise subagent results are silently dropped on the
floor and `persist_subagent_completions` never fires") and
`async_transport.rs:326-333` ("WS3's `persist_subagent_completions` never
fires in production"). Note the router *is* on the hot path: builtin tools
execute through `BuiltinExecuteHandler` → `execute_from_hook`
(`extensions/builtin/adapter.rs:312`).

Worth confirming empirically before fixing, but the static evidence is strong.
The fix is one line: make `main.rs` not pre-empt `state.rs`, or have
`create_local_transport()` defer to the daemon registry.



*What IPC is here.* Not a remote API — it is the **local daemon transport**: a
Unix domain socket (Windows named pipe, see `ipc/pipe_security.rs`) that lets a
short-lived co-process hand work to the long-lived daemon. Its async variants
exist for exactly one reason, ADR-020's: **the CLI exits after a turn, so
background work has to live in the daemon.** So the caller is not an "external
user" — it is our own CLI. The chain:

```
DaemonClient::spawn_async_task            (ipc/client.rs:100)
  → RequestPacket::AsyncSpawn             (ipc/packet.rs:51)
  → handle_async_spawn                    (ipc/handlers/tool.rs:262)
  → AsyncExecutor::execute(task_id, …)
```

Chosen only for non-daemon commands: `cli/src/main.rs:88-105` picks
`create_transport()` (IPC) unless the command *is* `daemon`, in which case it
uses the in-process `LocalAsyncTransport`.

*The bug.* `ipc/handlers/tool.rs:281` — `let task_id = AsyncTaskId::new();` —
but `AsyncTaskId` is `pub type AsyncTaskId = String;` (`async_status.rs:14`),
so this is `String::new()`. Every IPC-spawned task registers under `""`,
silently overwriting the previous one, and the task file path degenerates to
`.json` (`task_file.rs:95-98`). Note also that
`DaemonIpcTransport::spawn_task` (`async_transport.rs:207-219`) *discards* the
`task_id` the caller generated — it is bound as `_task_id` — so the real
`{tool}:{uuid}` id minted at `async_router.rs:207` never reaches the daemon in
the first place.

*Reachability caveat — read before acting on severity.* The one production
caller of the transport is `AsyncExecutionRouter::execute_with_timeout`
(`async_router.rs:229`), reached from `execute_from_hook`
(`extensions/builtin/adapter.rs:312`). Whether that path is live for ordinary
CLI tool calls is unresolved from static reading alone, and there is
counter-evidence (see P1-7: on the IPC transport, `get_status` can never
report completion, so this path would burn the full 300 s timeout on *every*
call — which suggests it is either rarely hit or already broken). **If the CLI
router path turns out to be dead, this drops from P0 to P2** (unreachable
code with a latent bug). Confirm empirically before spending effort: run
`peko send` with a slow tool against a running daemon and check whether a task
gets registered.

**P0-2 · Completion events are silently destroyed by the steering drains.**
Both post-run drain sites call `drain_all()` and keep only `Steering`:

- `ipc/handlers/principal.rs:2705-2717` — `filter_map(... Steering(env) => ..., _ => None)`, called on every successful principal run (`:2603`)
- `daemon/channel_binding.rs:536-543` — `let AsyncInboxItem::Steering(envelope) = item else { continue; };`

The comment at `principal.rs:2695-2697` acknowledges "the inbox is shared with
the async-task executor" and then drops the completions anyway, on the
reasoning that "the IPC path never enqueues them" — true, but irrelevant: the
*async executor* enqueues them, and they are the ones being dropped. Any task
finishing after the loop's final drain is gone, with no log.

**P0-3 · The documented 2-hour default timeout does not exist.** Schema says
`"Defaults to 7200 (2h)"` (`spawn.rs:76-78`) and `mod.rs:74-77` repeats it. In
reality `AsyncSpawnTool` passes `timeout_secs: Option<u64>` through unmodified
(`spawn.rs:125`), `AsyncExecutorRuntime::spawn` forwards the `Option`
(`async_runtime_impl.rs:165`), and `executor.rs:277-280` resolves `None` →
**no timeout at all**. `AsyncToolConfig::default()`'s `Some(7200)`
(`mod.rs:154`) is overwritten by `..Default::default()` before it can apply. A
task the model forgets about runs forever.

**P0-4 · `Bash { run_in_background: true }` results never reach the agent.**
`BashTool::background_executor()` builds its executor with
`standalone_inbox_registry()` — a private `InboxRegistry`, not the
daemon-shared one (`bash.rs:121-135`) — and derives
`parent_session_key = format!("{agent_id}_{session_id}")` (`bash.rs:138-149`),
which is not the session key the agentic loop drains under
(`agent.rs:1613-1614` uses `session_key`). The completion lands in an inbox
nobody reads. The `Async*` tools can still *find* the task via the
global-registry fallback, but the *completion event* is unreachable — so the
model must poll `AsyncOutput` forever. The module comment at `bash.rs:118-120`
documents the task-lookup fallback but appears not to notice the inbox split.

### P1 — robustness and resource control

**P1-1 · No concurrency bound of any kind.** `executor.rs:293` is a bare
`tokio::spawn` with the `JoinHandle` dropped — no semaphore, no worker pool, no
max-parallel setting anywhere in `async_exec` (verified by grep). A model that
loops `AsyncSpawn` spawns unbounded tasks, and nothing can `abort()` or join
them. Compare: subagent runs *are* bounded (`max_concurrent: 5`,
`subagent_executor.rs:919-926`) — the bound exists one layer over and was never
applied here.

**P1-2 · Cancellation is cooperative and mostly not wired.** The `watch`
channel mechanism (`registry.rs:94-103`) is right, but only
`dispatch_tool{,_with_signal}` pass it (`executor.rs:615-721`). The `execute` /
`execute_with_metadata` / `execute_boxed` paths pass `None` (`:528`, `:566`,
`:592`) — and those are exactly the paths used by background `Bash` and
subagent runs. Consequence, documented in-code at `executor.rs:750-753`:
*"Tool bodies that don't poll `is_aborted()` are unaffected — only the registry
status flips."* For background `Bash` the closure passes `ctx = None`, so
`wait_for_abort` parks on `std::future::pending()` forever (`bash.rs:317-327`)
and the child process keeps running — `Command` has no `kill_on_drop(true)`
(`bash.rs:167-172`). **`AsyncStop` on a background shell command reports
success while leaving the process alive.**

**P1-3 · No durability; nothing survives a daemon restart.** The registry is
pure in-memory (`registry.rs:241-245`). Task *files* are written to
`<data_dir>/async_tasks/*.json` — three times per task — but
`TaskFileWriter::read` (`task_file.rs:110-115`) has **no callers**. After a
restart, `AsyncOutput` returns `{"error": "Task not found"}`
(`output.rs:106-112`), and tasks interrupted at `running` leave stale records
until the 24 h janitor deletes them. This directly contradicts ADR-020's
motivation ("centralized task management", "background tasks die with
process") — the daemon hosts the tasks, but the state was never made durable.

**P1-4 · No ownership or isolation between principals.** `AsyncTaskEntry`
carries only `parent_session_key` (`registry.rs:76-104`); there is no
`principal_id`. Principals exist only inside the dispatch closure. Meanwhile
`GLOBAL_ASYNC_TASK_REGISTRIES` (`registry.rs:605-623`) is keyed by agent name
and shared process-wide, and:

- `AsyncExecutorRuntime::list` merges the per-agent registry **and every global
  registry** (`async_runtime_impl.rs:206-235`) → `AsyncList` shows other
  agents' tasks, unfiltered
- `lookup` falls back to `find_task_across_all_registries` (`:190-204`)
- `cancel` falls through to `cancel_task_across_all_registries` (`:237-260`)
- IPC `handle_async_cancel` takes only a task id and uses the daemon-global
  executor (`ipc/handlers/tool.rs:439-461`) → **any IPC caller can cancel any
  task in the process**
- `AsyncStopTool` performs no ownership check (`stop.rs:63-71`)

The only session-scoped API (`list_tasks(Some(session_key))`,
`registry.rs:398-404`) is called with `None` in production.

**P1-5 · Cancel races the completion write (TOCTOU).** `was_cancelled` is read
at `executor.rs:328-334` and `already_terminal` at `:350-361`, but the status
write happens much later at `:382-384` under a separate lock. A cancel landing
in that window is overwritten by `Completed`/`Failed`. There is no CAS across
the read-modify-write.

**P1-6 · The delivery gap: no wake when idle, no steer when running.**
*(Promoted from P2-8 after the semantics were traced end-to-end — full write-up
in §1.1.)* An async completion is a `CompletionEvent`, which only the agentic
loop's per-iteration drain consumes; it can never start a turn.
`SessionInbox`'s `Arc<Notify>` has **no `notified().await` consumer in the
workspace**, the daemon has no completion watcher (`daemon/mod.rs:423-426`),
and `wake_on_completion` is inert for anything an agent spawns because the
steer branch needs `principal_root_session_key: Some(..)` — set in production
**only** by `cron_engine/mod.rs:897`. So: a running agent gets a synthetic
message at the next iteration (if there is one); an idle agent gets nothing
until the user speaks again, and P0-2 may delete the completion first. Cron is
the only async path with real steering.

**P1-7 · On the IPC transport, `get_status` can never report completion.**
*(Dies with D1 — listed only because it is the cleanest proof the path was
never exercised.)*
`DaemonIpcTransport::get_status` returns `Ok(None)` unconditionally
(`async_transport.rs:221-231`) — there is no IPC status channel. The router
treats `None` as "still running" and keeps polling until the deadline
(`async_router.rs:263-283`), then returns
`{"_async_status": "queued", "task_id": …}`. So *every* tool call routed
through IPC reports as timed-out-and-queued after the full 300 s
(`DEFAULT_TOOL_TIMEOUT_SECS`), regardless of whether the work actually
finished in 2 s. This is the direct counterpart to P0-1 and is the strongest
evidence that the CLI/IPC execution path is either rarely exercised or already
broken — worth confirming at the same time.

### P2 — consolidation debt, dead code, and hazards

**P2-1 · The entire legacy delivery stack is dead and still being written
to.** `AsyncResultQueueManager::process_queue` (`executor/queue.rs:146`) has
**no callers**. `QueueDelivery` is the fallback that always wins
(`executor.rs:261-264`), so every completion also goes into an unbounded,
never-drained `Vec` (`queue.rs:13, 29-31`). `ChannelDelivery::clone_box` is a
literal `panic!` reachable through `impl Clone for Box<dyn ResultDelivery>`
(`delivery.rs:90-94`, `:158`). ADR-040 identified this exact queue as the
thing to fix; the fix routed around it instead of deleting it, so the bug and
its workaround now coexist.

**P2-2 · Other unreachable surfaces.** `TaskFileWriter::read`
(`task_file.rs:110-115`) — the task-file "audit trail" has no reader.
`AsyncTaskEventBus` is never constructed (`event_bus.rs:20`);
`cleanup_old_subagents` (`registry.rs:508`), `cleanup_empty_queues`
(`queue.rs:153`) and `pending_announcements` (`registry.rs:244` —
`queue_announcement`/`get_pending_for_session`/`pending_count` all callerless)
are never called; `ExtensionAsyncTool` is never constructed in production and
hardcodes session `"default_session"` (`:77`, `:83`).

**P2-3 · Unbounded growth without backpressure.**
`GLOBAL_ASYNC_TASK_REGISTRIES` is insert-only and never pruned. Inboxes "live
as long as the daemon" by design (`session/src/inbox_registry.rs:17-20`).
Subagent entries are never GC'd because they set `cleanup_after_delivery:
false` (`subagent_executor.rs:2019`) and the 300 s purge is gated on that flag
(`registry.rs:406-418`). `SessionInbox::push` uses `try_lock` and, on
contention, **spawns a detached task per item** (`inbox.rs:121-128`) instead of
applying backpressure — which both reorders completions and will panic outside
a Tokio runtime. Task results are stored uncapped in memory
(`registry.rs:83, 89`) and on disk (`task_file.rs:11-33`).

**P2-4 · Panic paths on production routes.** `executor.rs:376` and `:411` —
`timeout_secs.expect("Timeout implies Some timeout_secs")` inside the spawned
task (a panic here leaves the task stuck at `Running` with no final record).
`registry.rs:619, 629, 644, 668, 700, 721` —
`std::sync::Mutex::lock().unwrap()` inside `async fn`, which both panics on
poisoning and blocks the reactor. `inbox_registry.rs:173-178` — `Default`
installs a factory that `panic!`s when called.

**P2-5 · Locks held across `.await`.** `executor.rs:425-428` holds an `RwLock`
read guard across `delivery.deliver(...).await`; `executor.rs:453-487` holds
one across two inbox mutex acquisitions. The documented starvation hazard in
`wait_for_completion` (`registry.rs:723-743`) is still present and callable.

**P2-6 · Provider-compatibility risk in the synthetic message.**
`build_async_completion_message` emits `ToolResult` blocks with
`tool_call_id: "synthetic:<task_id>"` (`async_completion.rs:158`) that have
**no matching `ToolUse`** anywhere in history. `agentic_loop.rs:643-649`
explicitly notes that orphan `tool_result`s cause provider 400s and runs
`repair_history` on load — but the synthetic message is constructed *after*
that repair point, so it is never repaired.

**P2-7 · Silent failures and LLM-ergonomics drift.**
`AsyncStatus`/`AsyncOutput` return `{"error": "Task not found"}` as a
*successful* tool result (`status.rs:66-70`, `output.rs:107-111`) so the model
cannot distinguish it from a real answer. `output.rs:128-132` discards the
blocking wait's error entirely. `steer.rs:57-58` instructs the model to use
**`TaskOutput`**, a tool that does not exist (the family is `AsyncOutput`) —
and a test asserts on that string (`steer.rs:77`). `executor.rs:453` drops a
completion with no `else` branch and no log when the registry entry is missing.
ADR-040 says `task spawn`/`task output`; the shipped names are
`AsyncSpawn`/`AsyncOutput`.

**P2-8 · *(Promoted to P1-6 — see §1.1.)***

**P2-9 · Ordering and dedup.** `build_async_completion_message` preserves push
order, not `completed_at` — and P2-3's deferred pushes can reorder. No dedup: a
task pushed twice appears twice.

### D — dead path: delete, don't fix

**D1 · The IPC async-spawn path is unreachable — and would misbehave if
revived.** Investigated at the suggestion that everything already runs on the
daemon, so this path has no reason to exist. **Confirmed.** The evidence:

1. **The CLI never executes tools.** `peko-rs/cli/src/` contains no
   `execute_tool`, `ToolRuntime`, `invoke_hook`, or `tool_executor` at all.
   `peko send` is a pure `DaemonClient` IPC client (`cli/src/commands/send.rs:111`);
   the daemon runs the turn. (The `StatelessAgentService` in-process agent that
   ADR-020 was written around no longer exists.)
2. **The daemon always uses a local transport.** `daemon/state.rs:622-646`
   builds its router with `create_local_transport_with_inbox(..)` in both the
   test and production branches, and the reuse branch picks up the core that
   `main.rs` installed for `Commands::Daemon(_)` — also local (`main.rs:93`).
3. **The IPC router is CLI-only.** `create_transport()` (the IPC factory) has
   exactly one caller: `cli/src/main.rs:96`. `with_async_router` with an IPC
   transport appears nowhere else in production.
4. **ADR-020 is superseded** by ADR-021 (Daemon as Central Runtime) — which is
   precisely why the "CLI hands background work to the daemon so it survives
   CLI exit" rationale no longer applies.

So `RequestPacket::AsyncSpawn` / `AsyncCancel` have no live producer, and
`handle_async_spawn`'s `AsyncTaskId::new()` → `""` bug is latent, not active.
That bug is still worth recording because the path is wired and would fail the
moment anyone used it — but it is **not** a P0, and my earlier severity call was
wrong.

**Removal list** (all reachable only from this path):

- `RequestPacket::AsyncSpawn` / `AsyncCancel`, `ResponsePacket::AsyncReceipt`
  (`ipc/packet.rs:51`)
- `handle_async_spawn` / `handle_async_cancel` (`ipc/handlers/tool.rs:262`, `:440`)
- `DaemonClient::spawn_async_task` / `cancel_async_task` (`ipc/client.rs:100`, `:175`)
- `DaemonIpcTransport` (`transport/async_transport.rs:179`), `create_transport_with`
  (`:312`), and the `DaemonTransport` trait (`transport/mod.rs:68`) — all three
  methods are async-task-only
- `ipc/create_transport.rs` (the whole file) and the IPC branch of
  `init_extension_core` (`cli/src/main.rs:88-105`)
- `UnavailableAsyncTransport` (`transport/async_transport.rs:~236`) — the
  "daemon not running" failure mode no longer exists

Deleting this also deletes P1-7 (the missing status channel), since that is
purely a property of `DaemonIpcTransport`.

---

## 4. Design-level gaps

Beyond the bugs, capabilities the subsystem does not have. First and most
important: **no completion-driven wake or steer (§1.1, P1-6)** — the gap
between "background task" and "fire and forget". It has a section of its own
because it is the one that changes how the feature should be described to
users; the fix is small and well-localized (§6.1b): make the steering drains
non-destructive, then route `Completion` through the same successor-turn path
as `Steering` when the session holds no run permit.

1. **No progress or partial output.** You get nothing until terminal state, and
   on cancel/timeout the partial output is discarded entirely
   (`executor.rs:336-342`, `:371-378`; `set_result` is only called on success
   at `:387-391`). For a "run a long build in the background" feature this is
   the single most-missed affordance.
2. **No retry, backoff, or dead-letter** in the async layer. Cron has
   `DEFAULT_MAX_RETRIES = 3` one layer up (`cron/src/tools/mod.rs:52`);
   `AsyncSpawn` has none.
3. **No orphan handling for non-subagent tasks.** Deleting a session is guarded
   only for active *subagent* runs (`session/session_runtime_impl.rs:980-985`).
   A background `Bash` or `AsyncSpawn` task survives its parent session's
   deletion, and its completion calls `get_or_create` (`executor.rs:483-486`),
   **recreating** an inbox for the dead session — which is then drained by
   nobody, or destroyed by P0-2.
4. **No observability.** No metrics, no audit event for spawn/cancel, no
   structured task log beyond the task file. `AsyncTaskEventBus` exists but is
   never constructed.

---

## 5. What is genuinely good

- The state machine is defined once, in one place, with a clean terminal set
  and wire serialization (`async_status.rs`).
- The **drain-at-iteration-boundary** model is the right shape for injecting
  async results into an LLM conversation, and it is actually implemented —
  unlike ADR-040's original queue.
- The capability gate is **fail-closed**: `capabilities == None` is denied
  (`core/registry.rs:352-359`) and grants are re-checked at spawn time against
  the spawning principal's snapshot (F37, `async_runtime_impl.rs:164-184`). The
  security intent is sound even though the ownership model is missing (P1-4).
- `AsyncStop`'s already-terminal-returns-success semantics
  (`common.rs:121-158`) is a thoughtful concession to LLM ergonomics.
- 2048-byte preview truncation keeps the context window bounded and tells the
  model where to fetch the full result (`async_completion.rs:91-114`).
- Cooperative abort via `watch` channel is the correct mechanism — it is just
  under-wired (P1-2).
- Test coverage is reasonable for the happy paths: ~37 tests across
  `async_exec`, `async_control` (including a 524-line `integration_tests.rs`),
  and `async_completion.rs`.

---

## 6. Recommendations, in order

### 6.1 Do the four P0s first (small, independent, high value)

1. **Fix the router's inbox wiring (P0-1).** Stop `cli/main.rs` from
   pre-empting `AppState`: either skip `init_extension_core` for the daemon
   command, or make `create_local_transport()` resolve the daemon-shared
   registry instead of `standalone_inbox_registry()`. Confirm first that
   router-dispatched completions are indeed landing in the wrong registry.
2. Make the two steering drains non-destructive: peek-and-partition, or return
   the non-Steering items so they can be re-queued instead of dropped. (P0-2)
3. Make `AsyncExecutorRuntime::spawn` fall back to
   `AsyncToolConfig::default().timeout_secs` when `request.timeout_secs` is
   `None`, so the documented 7200 s actually applies. (P0-3)
4. Give `BashTool::background_executor()` the daemon-shared inbox registry and
   the real session key. (P0-4)

### 6.1b Then close the delivery gap (P1-6 / §1.1)

This is the smallest change that makes "background" mean what the name
implies, and it is worth doing immediately after the P0s:

1. **Stop destroying completions.** P0-2 (§6.1 item 2) is a prerequisite —
   there is no point waking on events that the steering drain has already
   deleted.
2. **Give `Completion` a wake path.** When a completion is pushed and the
   session holds no run permit, start a turn — the same way a `SteeringMessage`
   does. The machinery already exists (`run_steering_successor`,
   `principal.rs:2588-2620`; `drive_turn`, `channel_binding.rs:528-574`); it is
   just keyed on the wrong variant. Concretely: either consume `SessionInbox`'s
   already-wired `Notify` (`inbox.rs:87, 120, 126`) or add `Completion` to the
   successor-run trigger.
3. **Decide what `wake_on_completion` means** and make it honest. Today it
   defaults to `true` and does nothing unless `principal_root_session_key` is
   `Some`, which only cron sets. Either default `principal_root_session_key` to
   the spawning session for agent-spawned tasks (making the flag real), or
   rename it to something like `steer_cron_root` so it stops promising a
   behavior it does not have.
4. **Add one test per branch**: completion while a run is in flight, completion
   while idle, and completion racing the final iteration. Those three cases are
   exactly where the current behavior diverges from the documented one, and
   none of them is covered today.

### 6.2 Then one consolidation pass (this is the real work)

- **Delete the IPC async-spawn path (D1)** — full removal list in §3-D. This
  is the highest-value deletion in the subsystem: it removes a whole transport
  layer, the `DaemonTransport` trait, and P1-7 in one go, and it settles P0-1's
  severity question permanently.
- **Delete** the legacy delivery stack: `AsyncResultQueue`, `QueueDelivery`,
  `ChannelDelivery`, `CallbackDelivery`, `AsyncTaskEventBus`,
  `ExtensionAsyncTool`, `pending_announcements`.
- **Add `principal_id` + `session_id` to `AsyncTaskEntry`** and filter
  `list`/`lookup`/`cancel` by it. Replace the cross-registry scans with an
  explicit "find by id, then authorize" check. This is the fix for P1-4 and it
  is a prerequisite for multi-principal safety.
- **Add a concurrency bound** — a `Semaphore` with a configurable max-parallel
  (default ~8) at `executor.rs:293`, plus a bounded queue for the overflow
  rather than unbounded spawn.
- **Make the inbox push synchronous and ordered** (`inbox.rs:116-129`): drop
  the `try_lock` + detached-spawn behavior.

### 6.3 Then close the design gaps

- *(P2-8 was promoted to P1-6 and is covered by §6.1b.)*
- Persist task state durably and rehydrate on boot, or drop the task-file
  pretense (P1-3). Right now the files are an audit trail that looks like
  state.
- Emit partial/progress output on cancel and timeout rather than discarding it
  (P1-2, §4 item 1).
- Either pair the synthetic `ToolResult` blocks with matching `ToolUse` blocks,
  or route them through the same repair pass as the rest of history (P2-6).
- Fix `steer.rs:57-58` (`TaskOutput` → `AsyncOutput`) and reconcile ADR-040's
  `task spawn`/`task output` names with what shipped.
