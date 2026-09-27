# ADR-063: Async Task Delivery Consolidation — Wake-on-Completion, Ownership, and the Deleted Delivery Stacks

**Status:** Accepted (2026-09-27). Implemented on branch
`fix/async-task-review`.
**Date:** 2026-09-27
**Author:** rlsn (with Kimi Code)
**Related:** [ADR-040](ADR-040-tool-timeout-and-async-refactor.md) (tool
timeout & async refactor), [ADR-020](ADR-020-daemon-based-async-execution.md)
(daemon-based async execution), ADR-021 (daemon as central runtime),
[ADR-061](ADR-061-agent-authored-workflows.md) (per-call session stamping).
**Driver:** a 2026-09-27 audit of the async tool-calling subsystem
(`ASYNC_TASK_REVIEW.md`, since removed — its findings are restated here and in
this ADR's §3 decisions).

---

## 1. Context

The async task subsystem had been built up incrementally (F37 funnel, F38
cancel signal, ADR-061 session stamping) without removing the paths it
replaced. The 2026-09-27 review found, among smaller defects:

- **The delivery gap (§1.1 of the review).** A terminal async task pushed
  a `CompletionEvent` into the parent session's inbox, but only a run
  *already in flight* ever drained it. An idle session got nothing until
  the next unrelated user message — "background" was effectively
  fire-and-forget. `wake_on_completion` defaulted to `true` yet was inert
  for every agent-spawned task. Worse, the two post-run steering drains
  (`ipc::handlers::principal`, `daemon::channel_binding`) called
  `drain_all()` and discarded non-steering items, silently destroying
  completions that raced a run's final iteration (P0-2).
- **Misrouted production wiring.** `cli/main.rs` pre-installed a global
  `ExtensionCore` whose router executor used a *standalone* inbox
  registry; `AppState::new` then reused that core instead of the one it
  had just wired to the daemon-shared registry (P0-1). `BashTool`'s
  background executor had the same standalone-registry problem plus a
  `{agent}_{session}` inbox key the loop never drains (P0-4).
- **A dead IPC delivery stack.** The ADR-020 "CLI hands background work
  to the daemon" transport had no producer after ADR-021 made the CLI a
  pure IPC client (D1) — and its latent `AsyncTaskId::new()` → `""` bug
  would have collapsed every spawned task onto one id had it ever fired.
  Underneath it, the pre-inbox delivery machinery (`AsyncResultQueueManager`,
  `QueueDelivery`/`ChannelDelivery`/`CallbackDelivery`, `AsyncTaskEventBus`,
  `pending_announcements`) was still being written to and never drained
  (P2-1).
- **No ownership, no bound.** Task entries carried no principal identity;
  `AsyncList`/`AsyncStop` merged and mutated across every principal's
  tasks (P1-4), and spawning was an unbounded `tokio::spawn` (P1-1).

## 2. Decision

### 2.1 Delivery semantics

Completion delivery has exactly **one** mechanism: the terminal push into
the per-session inbox the agentic loop drains at iteration start. Around
it:

1. **Post-run drains are non-destructive.** `AsyncInboxLike` gains
   `drain_steering()` (atomic partition on `SessionInbox`) — steering is
   taken, completions stay queued.
2. **Idle sessions wake.** The executor fires a process-global hook
   (`async_exec::executor::wake::notify_completion_wake`) after each
   delivered terminal outcome — completion and cron-steer branches alike.
   The daemon installs the handler (`daemon::completion_wake`): it
   re-acquires the session's run permit (`None` ⇒ a run is in flight and
   will drain itself), resolves the principal from the task's
   `principal_id` stamp, and drives a successor turn via the shared
   `PeerChildTurns` recipe. The turn's first-iteration drain performs the
   actual injection; `WAKE_TURN_MARKER` explains the turn in the
   transcript. The post-run successor chains drive the same way for
   completions that raced a final iteration. Chains are bounded
   (`MAX_WAKE_CHAIN`) so self-perpetuating completion loops cannot spin.
3. **`wake_on_completion` is honest now**: it gates the idle-wake hook
   (default `true`; cron's bookkeeping spawns opt out). It no longer
   secretly requires `principal_root_session_key` to mean anything.
4. **The router is quiet by default.** Every builtin call is spawned as a
   background task, so an unguarded terminal push would flood the shared
   inboxes with one event per tool call. Router-spawned tasks carry
   `deliver_completion: false`; the router flips it via
   `deliver_on_completion` only when the call detaches past the timeout
   and the agent receives a `queued` receipt. Detached completions are
   keyed by the plain session id — the key the loop drains.

### 2.2 Ownership and resource control

- `AsyncToolConfig.principal_id` stamps the owning principal at every
  spawn path. `AsyncExecutorRuntime`'s `lookup`/`list`/`cancel`/
  `wait_for_completion` find-then-authorize: a task stamped with another
  principal is indistinguishable from a missing one. Unattributed tasks
  (`None` — tests, CLI one-shots) stay visible to all.
- `AsyncExecutor` holds a `Semaphore` (`DEFAULT_MAX_CONCURRENT_TASKS = 8`,
  `with_max_concurrent` to override). Permits are acquired inside the
  spawned future; queued tasks stay `Pending` and cancellable, and a
  cancel landing while queued skips execution entirely.
- The cancel-vs-completion TOCTOU (P1-5) is closed by performing the
  terminal status write under a single registry write lock (cancel takes
  the same lock).
- `SessionInbox::push` is synchronous and ordered (`std::sync::Mutex`;
  the `try_lock` + detached-spawn fallback is gone).
- Background `Bash` runs through `AsyncExecutor::execute_cancellable` and
  `Command::kill_on_drop(true)`, so `AsyncStop` actually kills the child
  process.

### 2.3 Deletions

- The IPC async-spawn path end to end: `RequestPacket::AsyncSpawn` /
  `AsyncCancel`, `ResponsePacket::AsyncReceipt`, the `DaemonClient`
  methods, the daemon-side handlers, `DaemonIpcTransport`,
  `UnavailableAsyncTransport`, the `DaemonTransport` projection,
  `ipc::create_transport`, and `cli/main.rs`'s `init_extension_core`.
- The legacy delivery stack: `AsyncResultQueueManager`, the three
  `ResultDelivery` impls + formatter registry, `AsyncTaskEventBus`,
  `pending_announcements`, `cleanup_old_subagents`,
  `ExtensionAsyncTool`, `TaskFileWriter::read`, and the
  `delivery_mode`/`delivery_target`/`DeliveryTarget`/
  `AsyncResultDeliveryMode`/`SessionMessageType` config surface
  (executor-side and the `async_control` mirrors).

## 3. Consequences

- **Wire change:** the three retired IPC packet variants are gone; a
  mismatched CLI/daemon pair errors on the unknown variant instead of
  silently enqueueing nothing. The variants had no producer since
  ADR-021, so no working deployment depended on them.
- `AsyncToolConfig` gains `principal_id` + `deliver_completion` (serde
  defaults keep old task files readable) and loses the delivery-target
  fields (task files are an ephemeral audit trail; the janitor reaps them
  at 24h regardless).
- A completion that used to wait silently now drives an LLM turn — that
  is the point, but it means background tasks have a token cost on
  completion. `wake_on_completion: false` is the opt-out for bookkeeping
  spawns.
- Follow-ups **deliberately not planned** (2026-09-27): durable task state
  across daemon restarts. Async tasks are ephemeral by design — a restart
  loses `running`-at-shutdown tasks and their registry entries, and the
  task files remain write-only audit artifacts reaped by the janitor at
  24h. This is a considered non-goal, not deferred work: unlike cron jobs
  (which are JSON-persisted on disk, reloaded at startup, and reconciled by
  the janitor's `reconcile_running_runs`), an async task is an
  *agent-initiated unit of work tied to a live conversation* — there is no
  meaningful way to resume a half-executed tool call whose parent session
  and LLM context are gone, and re-materializing the entry would only
  produce a task that cannot be re-run. Callers that need restart-safe
  work should use a cron job or an external workflow, not `AsyncSpawn`.

- Shipped in this ADR (previously listed as out of scope): **partial /
  progress output on cancel/timeout** (§4.1) — a per-task progress buffer
  shared with the executing closure, surfaced through `AsyncOutput` on
  still-running tasks and folded into the completion event when a task is
  cancelled or times out; background `Bash` populates it by streaming
  child output as it is produced. **P2-6** — the synthetic completion
  message now uses `Text` blocks instead of orphan `ToolResult` blocks
  (no `tool_use` to pair with, no provider 400 class), carrying the task
  id, tool name, and terminal status inline.
