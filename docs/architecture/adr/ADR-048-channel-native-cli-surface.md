# ADR-048: Channel-Native CLI Surface (`send` / `stop` / `log`)

**Status:** Accepted
**Date:** 2026-08-27
**Author:** rlsn
**Related:** [ADR-042](ADR-042-no-external-session-concept.md) (no external `session` concept; the privacy rule `stop`/`log --watch` inherit), [ADR-044](ADR-044-chat-session-separation.md) (peer DM channels as the conversation's durable home), [ADR-041](ADR-041-principal-as-container.md) (principal-as-container), [ADR-049](ADR-049-multi-party-group-channels.md) (supersedes the group-channels section below).

**Note:** This is a clean-slate pre-production design. Backward compatibility with Peko 0.1.0 is intentionally discarded so the codebase and UX remain coherent.

---

## 1. Context

The pre-ADR CLI modeled the wire protocol, not the user's intent:

- `peko send` started a run and streamed it; `peko interrupt <request-id>`
  cancelled by a client-minted id; `--steer` injected a message
  mid-run. Three commands for what a user experiences as one: *I'm
  messaging a principal on a channel; I don't care whether it's
  mid-run.*
- The run registry (`AppState.streaming_runs`) was keyed by that
  client-minted `request_id`, and every CLI process starts its counter
  at 1 (`ipc/client.rs`). Two concurrent `peko send` processes
  overwrote each other's registry entry, and `peko interrupt 1`
  cancelled whichever run was inserted last. A real collision bug, not
  a cosmetic one.
- Steering-successor runs (messages landing on the final iteration,
  drained by the Gap-2 path) were never registered at all, so they
  were unstoppable.
- `peko interrupt` produced a confusing trail: the DM channel showed
  `⚠ Run failed: Subagent was cancelled` for what was a deliberate
  user stop.
- `peko log` was read-only paging; watching a live thread meant
  re-running it by hand.

Meanwhile the daemon already behaved channel-natively:
`drive_principal_ingress` posts the message to the peer's DM channel
first, then either acquires the per-session run permit (fresh run) or
queues a `SteeringMessage` the agentic loop drains at the top of every
iteration. The CLI was the only layer still pretending "steering" was
a separate concept.

## 2. Decision

Three commands address a thread by `(recipient, --peer)`; `--peer`
defaults to the caller's identity (the global `-U/--user`, default
`"local"`).

### `peko send <recipient> [msg]` — post to the thread

Always enqueues onto the `(principal, peer)` thread. If a run is in
flight, the message folds into it at the next agentic iteration; the
daemon answers the streaming request with `Done { success: false,
error: "[queued] …" }` (a bracketed-prefix convention, not a failure)
and the CLI prints a busy notice to stderr and exits 0. `--wait`
blocks for the principal's reply via the `principal_log_watch` stream
(10-minute cap, bounded `principal_log` poll as fallback). `--peer
user:<id>` overrides `-U`. Removed from the surface: `--stream`
(hidden no-op), `--no-stream` (streaming render is the only mode;
`--json` already implies buffered output), and the `request_id` stderr
banner.

### `peko stop <recipient> [--peer]` — deterministic halt + cleanup context

Replaces `peko interrupt <request-id> [--steer]` (deleted).

1. Resolve `(principal, peer)` → peer child session → run handle in
   the rekeyed registry. Not found → `Done { success: false, error:
   "no running turn…" }`; the CLI prints a friendly notice and exits
   0 (idempotent, scripting-friendly).
2. Fire the cancel token. This is a *soft* stop: the agentic loop
   exits at the next iteration boundary; spawned subagents observe
   their derived `child_token()` and stop at their own boundaries —
   the cascade is structural, not message-based.
3. Post a peer-authored `⏹ stopped by user` marker to the DM channel
   (visible in `peko log`; replaces the misleading `⚠ Run failed`
   row).
4. Push a stop-context note into the session inbox so the **next**
   run's first-iteration drain sees it: the LLM acknowledges what was
   interrupted and judges any rollback/cleanup.

The split is deliberate: the halt is control plane (deterministic,
token-based, never depends on inference); the cleanup judgment is data
plane (LLM context on the next turn). A stop must work even when the
model is degraded or slow.

### `peko log <recipient> [--watch]` — read or follow the thread

`--watch` opens the new `principal_log_watch` IPC: replay of `Posted`
rows newer than `--cursor`, then a live broadcast forward, with
heartbeat packets every 2s so a quiet thread never trips the CLI's
60s per-packet idle timeout. `--json --watch` emits NDJSON. `--limit`
is now a hard cap on a single page; `--all` opts into the multi-page
drain.

### IPC changes

- **Added:** `PrincipalStop { name, peer }` → `Done`; `PrincipalLogWatch
  { name, peer, since_cursor }` → stream of `PrincipalLogAppended {
  message }` + `Heartbeat`.
- **Removed (breaking, pre-launch cutover):** `PrincipalSendControl` +
  `PrincipalSendControlMode`. No known external consumer; peko-desktop
  depends only on the one-shot `PrincipalSend`/`PrincipalSent` shapes,
  which are untouched.
- **Registry rekey:** `streaming_runs` is keyed by the peer child
  session id (collision-free; one run per session is already enforced
  by the run permit). Successor runs register under the same key and
  are stoppable like any run. `PrincipalStop` resolves
  `(name, peer)` → session id before lookup; no request id crosses
  the wire for control anymore. Ctrl-C in `peko send` sends
  `PrincipalStop` for the thread.

### Why not expose `ChannelEventsWatch` for `log --watch`

The raw channel watch has no authorization check (any parseable
channel id is watchable) and no heartbeats (a quiet channel dies at
the client idle timeout). `PrincipalLogWatch` is its privacy-checked
sibling: the ADR-042 rule (`caller == peer || caller == owner` + the
`Chat` grant) runs before the DM channel is even resolved, and
heartbeats are interleaved. `ChannelEventsWatch` itself is unchanged
(peko-desktop consumes it).

### Group channels (`group:<slug>` recipients)

> **Superseded by [ADR-049](ADR-049-multi-party-group-channels.md).** The
> group model below ("principal-authored spaces, no user-authored post
> path") was the pre-ADR-049 state. ADR-049 redefines groups as
> multi-principal, multi-user channels with `Subject`-typed membership and
> authorship, a user write path via `peko send group:<slug>`, a loop-safe
> wake policy for user-authored posts, and membership-gated reads.

Historical record (pre-ADR-049 behavior at the time of this ADR):

- `peko send group:<slug> …` was **refused** with a pointer to
  `peko channel post group:<slug> <sender-principal> "<msg>"`.
  (`--wait`/`--model` got their own clear refusals. `--no-slash`
  was also refused at this point but has since been retired
  entirely along with the slash command subsystem; see
  [ADR-049 D7](#d7--cli-outcomes) for the current rule.)
- `peko stop group:<slug>` was refused ("groups have no bound run").
- `peko log group:<slug> [--watch]` read the channel directly via
  `ChannelPeek` (no principal privacy check — same posture as `peko
  channel peek`; membership gating was a known gap). `--watch` polled
  every 2s rather than using the heartbeat-less raw watch stream.
  Group `--json` rows were `{at, author, text}` (authors verbatim), not
  the `PrincipalLogMessage` shape.

The future change sketched here at acceptance time — an `author` field on
`ChannelPost` mapped to `post_attributed`, with a decision about which
member principal authorizes a human's write — is the path ADR-049 took,
with one amendment: ADR-049 D2 makes membership `Subject`-typed, so users
post as themselves and no vouching principal is needed.

## 3. Consequences

**Positive:**

- The CLI matches the daemon's actual (channel-native) behavior; one
  verb per intent. `request_id` disappears from the user surface
  entirely.
- The registry collision bug is gone structurally: session-id keys
  can't collide across CLI processes, and successor runs are
  first-class (registered, stoppable).
- Stops are honest in the transcript: `⏹ stopped by user` instead of
  a fake failure row, and the next turn inherits cleanup context.
- `log --watch` is safe to expose: privacy-checked and heartbeat-kept.

**Negative:**

- Wire-removal of `PrincipalSendControl` is a breaking protocol change.
  Acceptable pre-launch (no known external consumer); if peko-desktop
  privately used it, that surfaces at integration time.
- The busy path's `[queued]`-prefixed `Done { success: false }` and
  stop's "no running turn" `Done { success: false }` are
  string-convention signals, not typed packets — the CLI prefix-matches.
  Pre-launch pragmatism; a typed variant is a fine future cleanup.
- The watch replay/subscribe overlap is deduped by `(author, at, text)`
  tuple (broadcast events carry no line number); a same-author
  same-second duplicate post could be dropped. Accepted at DM-channel
  scale.
- Group log reads bypass membership checks (same as `peko channel
  peek`). A known gap at acceptance time — **closed by ADR-049 D6**
  (membership-gated reads).

## 4. References

- `peko-rs/cli/src/commands/{send,stop,log}.rs` — the merged CLI surface.
- `peko-rs/core/src/ipc/handlers/principal.rs` — `handle_principal_stop`,
  `handle_principal_log_watch`, the `[queued]` busy arm, the
  cancel-path wording fix.
- `peko-rs/core/src/daemon/state.rs` — the session-id-keyed
  `streaming_runs` registry.
- `peko-rs/core/src/ipc/packet.rs` — `PrincipalStop`,
  `PrincipalLogWatch`, `PrincipalLogAppended` wire shapes.
- `docs/user-guide/CLI_REFERENCE.md` §`send` / §`stop` / §`log`.
