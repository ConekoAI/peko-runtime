# ADR-072: Channel Replies Route to the Asker; Durable Cross-Runtime Delivery

**Status:** Accepted
**Date:** 2026-10-12
**Related:** [ADR-048](ADR-048-channel-native-cli-surface.md) (channels as the
conversation surface), [ADR-049](ADR-049-multi-party-group-channels.md)
(group channels), [ADR-057](ADR-057-single-attribution-identity.md) and
[ADR-058](ADR-058-origin-signed-messaging.md) (signed channel envelopes)

---

## 1. Context

Every message into or out of a principal is a channel post. Two problems
remained:

1. **Replies.** `ChannelSend` to another principal blocked until the target
   answered. A principal (or a human behind one) can take days; the wait
   lived in memory (a restart lost the reply); a per-target lock queued every
   later send behind it; and waiting for a reply on a channel whose posts
   also start conversation turns would deliver the answer twice.
2. **Delivery between runtimes.** Channel events and invites went source
   runtime → pekohub → recipient runtime with no acknowledgement. The source
   skipped the send when its tunnel was down; the hub dropped anything
   addressed to a runtime that was not connected. Messages to an offline
   principal were silently lost.

## 2. Decision

### D1 — Posts have stable ids

A `posted` event carries `msg_id` (minted by the writing store, carried
verbatim to every mirror) and `reply_to` (the parent post's `msg_id`, derived
from `parent`). Line numbers differ between mirrors; these do not. Both are
inside the signed event bytes.

### D2 — A reply goes to the session that asked

A post threaded onto a post written by a principal on this runtime is routed
to the session named in that post's `via`: queued in its inbox as a steering
message and woken through the completion-wake path (an idle session gets a
turn; a running one reads it at its next iteration). Root posts keep starting
conversation turns. Each post therefore has exactly one consumer.

The router wraps every channel subscriber's responder, and routes on behalf
of the post's author, not the subscriber: a same-runtime target answers on
its own DM channel, where the asker has no subscriber. A shared claim set
routes each reply once; same-runtime replies are mirrored onto the asker's DM
channel (same `msg_id`) for `peko log`.

Delivering a reply is not answering it — the asker answers only by calling
`ChannelSend` again — so agents cannot auto-reply in a loop.

### D3 — `ChannelSend` posts and returns

Every send returns at once. A same-runtime principal send records the request
on the caller's DM channel and delivers it on the target's, under one
`msg_id`. The channel responder answers every DM request (a failed turn as a
`⚠ Run failed:` reply), so an asker always hears back.

### D4 — A runtime outbox, the hub stays a relay

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

- **A `watch` flag on `ChannelSend`.** Keeps the blocking path for callers
  that ask for it, with all of its problems, and makes the agent judge which
  channels are safe to watch.
- **Store-and-forward on the hub.** Delivers even when the two runtimes are
  never online together, but puts message contents on the hub. Deferred.
- **A new hub message type for receipts.** Cleaner on the wire, but needs a
  coordinated hub deploy; the receipt rides the existing relayed envelope
  instead.

## 4. Consequences

- Replies arrive whenever they are written, including after restarts.
- A message reaches an offline runtime once both are online at the same
  time within 30 days.
- `ChannelEvent` gains a transport-only `received` kind; pekohub forwards it
  verbatim (its TypeScript event union can list it for documentation).
