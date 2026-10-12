# ADR-072: Only Humans and Cron Start Runs; Durable Cross-Runtime Delivery

**Status:** Accepted
**Date:** 2026-10-12
**Related:** [ADR-048](ADR-048-channel-native-cli-surface.md) (channels as the
conversation surface), [ADR-049](ADR-049-multi-party-group-channels.md)
(group channels — its wake policy is replaced by D2 here),
[ADR-057](ADR-057-single-attribution-identity.md) and
[ADR-058](ADR-058-origin-signed-messaging.md) (signed channel envelopes)

---

## 1. Context

Channels are a principal's only way in and out: every message to or from a
principal is a channel post, and `ChannelRead` / `ChannelSend` give the
principal full control over them. A user's DM must start a run at once. Three
problems remained:

1. **Principals woke each other.** A post from another principal on a DM
   channel started a turn, and that turn's answer was posted back, which
   could start a turn on the other side. Group channels woke every member
   principal on every user post. Two principals could talk to each other
   without end, and a busy group drove a turn per post.
2. **`ChannelSend` waited for answers.** A send to another principal blocked
   until the target answered. A principal (or a human behind one) can take
   days; the wait lived in memory (a restart lost the answer); a per-target
   lock queued every later send behind it; and waiting on a channel whose
   posts also start turns would deliver the answer twice.
3. **Delivery between runtimes was lossy.** Channel events and invites went
   source runtime → pekohub → recipient runtime with no acknowledgement. The
   source skipped the send when its tunnel was down; the hub dropped anything
   addressed to a runtime that was not connected.

## 2. Decision

### D1 — Only humans and cron start runs

A run starts for a user's DM (through ingress: `peko send`, Hub webchat, IPC
steer) and for a cron job — nothing else. A post by another principal never
starts a turn, on a DM or in a group. The DM channel responder is removed:
DM channels get a no-op responder, since their user posts already arrive
through ingress.

Principals therefore cannot set off a chain of automatic replies. A principal
that wants an answer reads for it (D3).

### D2 — Group channels wake on `@<name>` only

A group channel starts a turn in a member principal's group session only for
a **user's root post mentioning `@<principal name>`** (case-insensitive,
whole name: `@ada` is not mentioned by `@adam`). Mentions by principals never
wake anyone. The turn runs through the same driver as `peko send` (no time
limit, stoppable with `peko stop`).

### D3 — `ChannelSend` posts and returns; `ChannelRead` reads or watches

Every send returns at once. A same-runtime principal send records the message
on the caller's DM channel and delivers it on the target's, under one stable
`msg_id`. The result names the caller's `dm_channel`, where an answer — a
later `ChannelSend` from the target — appears.

`ChannelRead` gains `watch: true`: wait until someone else posts after a
cursor (default: the newest event at call time), optionally narrowed by
`author`, `query`, or `reply_to` (an event id the post must be threaded
onto). A watch has no time limit; past the 10-minute foreground cap it moves
to the background and its result arrives as a completion. It ignores the
caller's own posts and posts that start turns by themselves (a user's root
post on a DM, a user's root `@<name>` mention in a group), so no message is
handled twice.

### D4 — The session context lists unread posts

With no wakes, a principal learns about posts by reading. Each iteration's
`{{session_context}}` lists unread posts per channel (count + short previews),
tracked by per-(session, channel) read marks that rendering and `ChannelRead`
advance:

- the session's own channel (its DM or group): everything except the posts
  that started its turns;
- for sessions humans or cron drive (the trunk and the peer standing
  children), the principal's other channels: other principals' posts, and
  group posts no session was woken for. Spawned subagents see only their own
  channel.

A channel opened after the session was created is unread from its first post,
so a new peer's opening message is not lost to the "never dump history on
first look" rule.

### D5 — Posts have stable ids

A `posted` event carries `msg_id` (minted by the writing store, carried
verbatim to every mirror) and `reply_to` (the parent post's `msg_id`, derived
from `parent`). Line numbers differ between mirrors; these do not. Both are
inside the signed event bytes. They identify messages for delivery (D6); they
do not change who is woken.

### D6 — A runtime outbox; the hub stays a relay

The source runtime keeps each outbound channel envelope (event or invite) on
disk until its recipient runtime confirms it, resending on a per-entry
backoff (30 s doubling to 1 h), whenever the tunnel reconnects, and after a
restart. A resend reuses the envelope's `request_id` and is signed afresh
(signatures expire after five minutes). Entries older than 30 days are
dropped with a warning.

The recipient applies each `request_id` once (a persistent applied log) and
confirms every arrival with a receipt — `ChannelEvent::Received`, carried in
an ordinary `TunnelChannelEvent` envelope so the hub relays it unchanged.
Receipts are never logged, are not themselves confirmed, and confirm only
outbox entries addressed to their sender. An event that cannot be applied yet
(its invite has not arrived) gets no receipt and is resent.

`ChannelSend` waits up to three seconds for a remote receipt and reports
`delivered` or `queued` (the target runtime may be offline; the message is
kept and resent).

The in-memory replay cache that dropped a repeated `request_id` before
signature verification is removed: it would have silenced a legitimate resend
after a lost receipt. Replays are bounded by the signed `iat`/`exp` and
applied once by the persistent log.

## 3. Alternatives considered

- **Route replies to the asking session.** Threaded replies to a principal's
  post were queued in the asking session's inbox and woke it (briefly
  implemented). A reply that wakes its asker can be answered, which wakes
  the other side again; it needs DM channels to answer every request, and a
  claim set so each reply is delivered once. Dropped for D1.
- **A hop limit on principal-to-principal wakes.** Bounds a loop but still
  wakes on every message, handles a message both as a turn and as a reply,
  and cuts conversations off at an arbitrary depth.
- **A `watch` flag on `ChannelSend`.** Keeps a blocking send and makes the
  agent judge which channels are safe to wait on; a watch on `ChannelRead`
  separates posting from waiting.
- **Store-and-forward on the hub.** Delivers even when the two runtimes are
  never online together, but puts message contents on the hub. Deferred.
- **A new hub message type for receipts.** Cleaner on the wire, but needs a
  coordinated hub deploy; the receipt rides the existing relayed envelope
  instead.

## 4. Consequences

- No principal-to-principal loop can start on its own; every run traces back
  to a human message or a cron job.
- Principals cooperate by posting and reading: `ChannelSend`, then either a
  `ChannelRead` watch or the unread summary on a later turn.
- In a group, users address a principal with `@<name>`; unmentioned posts
  wait for its next turn there.
- A message reaches an offline runtime once both are online at the same
  time within 30 days.
- `ChannelEvent` gains a transport-only `received` kind; pekohub forwards it
  verbatim (its TypeScript event union can list it for documentation).
