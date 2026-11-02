# ADR-091: A person's history with a remote peko comes from its host, only to that person

**Status:** Accepted (2026-11-02)
**Date:** 2026-11-02
**Builds on:** [ADR-090](ADR-090-local-user-dms-a-remote-peko-over-the-tunnel.md) (the local user DMs a remote peko; one conversation per person on the peko's side; the Hub's D3 check), [ADR-081](ADR-081-actor-identity-ownership-and-single-online-copy.md) (one local user per runtime, registered with one Hub account), [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (compartments, trust), [ADR-072](ADR-072-human-started-runs-and-durable-delivery.md) (only humans and cron start runs).
**Counterpart:** PekoHub (relay of two new tunnel messages; a history route for clients without a runtime).

---

## 1. Context

A person who signs in on a new computer, or on their phone, sees none of their past conversation with a remote peko. And a runtime that changes hands keeps the previous person's conversations readable. Verified on `master` @ `c91c64b0`:

| # | Fact | Where |
|---|---|---|
| F1 | The person's side of a remote DM is a channel on their own runtime whose human member is `user:local`; it starts empty when that runtime first opens the DM. | `channel/src/store.rs` `ensure_human_dm` |
| F2 | The peko's runtime keeps one conversation per person: the pair DM `dm_channel_id(peko key, "user:hub:<account>")`, whichever device the person used. It is the complete record. | ADR-090 D1, D4 |
| F3 | Every `Posted` event carries a `msg_id` assigned once by the posting store and identical in every runtime's copy; line numbers differ. | `protocol/src/channel.rs` `ChannelEvent::Posted::msg_id` |
| F4 | Channel envelopes are push-only; the tunnel has no runtime-to-runtime request and response. The Hub relays an envelope for a person (`user:hub:<id>`) only from the runtime registered to that account. | `tunnel/channel_outbox.rs`; PekoHub `speaksForItsOwnPerson` (ADR-090 D3) |
| F5 | Unregistering a runtime drops the local user's Hub alias and nothing else. Registering under another account makes the new person the same `user:local`, a member of every remote DM the previous person opened, readable through `channel_peek`, `channel_events_watch`, `peko log`, and the desktop. | `store.rs` `unlink_local_user` / `link_local_user`; `tunnel/registration.rs` |
| F6 | A Hub-proxied chat (no runtime: a phone) lands in the same pair DM as F2 and returns no history. | `tunnel/dispatcher.rs` `handle_proxied_request` |

## 2. Decision

### D1 — The host's conversation is the record; a device's copy is a cache

The peko's runtime (the *host*) holds the conversation of record (F2). A person's runtime keeps a copy that it fills from the host and may drop at any time. Copies merge by `msg_id` (F3): a synced post this copy lacks is appended, oldest first by `at`; an event without a `msg_id` is never synced. The log stays append-only, so line cursors held by readers stay valid.

### D2 — Asking the host: two tunnel messages

- `tunnel_channel_history_request { requestId, sourceRuntimeId, recipientRuntimeId, sourcePrincipalDid, channelId, before?, after?, limit, signature }`: the person's runtime asks for one page. `sourcePrincipalDid` is `user:hub:<account>`; `before` / `after` are `msg_id` cursors (newest page when both are absent); `limit` is at most 100.
- `tunnel_channel_history { requestId, sourceRuntimeId, recipientRuntimeId, channelId, events, hasMore, signature }`: the host's answer. `events` are `Posted` events, oldest first.

Both are runtime-signed (domain tags `channel-history-request:v1`, `channel-history:v1`). The person's runtime accepts an answer only for a request it has pending, from the runtime it asked.

### D3 — The host serves a conversation only to the person in it

Before reading anything, the host checks:

1. **Who asks.** The requester is `user:hub:<account>`, counter-signed by the source runtime, and the Hub relayed it — which it does for a person only from the runtime registered to that account (F4). No other requester kind is served: not a principal, not a visitor, not `user:local` of another runtime.
2. **What is asked.** `channelId` is exactly `dm_channel_id(<a peko hosted here>, "user:hub:<account>")`, its kind is `dm`, and the channel exists. Group channels and every other DM are refused without saying whether they exist.
3. **May they.** The person may still open this DM, by ADR-090 D4's own check: the peko is registered and not `local_only`, and a `private` one needs a `Chat` grant for the person (its owner always has one). An owner who revokes access ends history too.

It then serves only `Posted` events, stripped of `via` (the peko's internal session path) and `parent` (a line number in the host's own log; `reply_to` carries the reply across runtimes). Attachments come as their metadata only; the files stay on the host (ADR-088). History requests never start a run, are never fanned out, and get no receipt. A refusal is an empty page with `hasMore: false`, so a probe learns nothing.

### D4 — When a device syncs

- **Catching up** (stored). On `channel_open_remote_dm`, on `tunnel_ready`, and on `peer_runtime_online` for the host, a runtime asks for what its copy lacks: with no post yet, the newest page; otherwise every page after the newest `msg_id` it holds (at most 20 pages a pass). Messages sent from the person's other devices arrive this way too.
- **Scroll-back** (not stored). `channel_history_older { channel, before }` fetches the page before a `msg_id` from the host and hands it to the client; the device keeps only what it caught up on, so its log never has to be rewritten.
- **Host offline.** The request goes unanswered (the Hub drops it, F4); the wait ends after 15 s, and the next trigger asks again. Nothing is queued at the Hub.

Synced events are appended as verified remote events: they wake nothing (ADR-072), are not fanned out, and get no receipt.

### D5 — A runtime that changes hands forgets the previous person

- A remote DM's `meta.json` records `account`: the Hub account it was opened under.
- When the local user's Hub alias is removed or changes (unregister; a registration under another account), the runtime **deletes every remote DM whose `account` is not the new one**, with its outbox entries; a DM opened before `account` was recorded counts as the previous account's when the local user created it and a remote runtime hosts its peer. The host still holds the record (D1); if that person registers here again, their history syncs back.
- Undelivered posts would be lost, so unregistering refuses while any wait in those DMs: `peko runtime unregister --discard-unsent` (IPC `runtime_unregister { discard_unsent }`) goes on anyway, after the desktop or CLI has said what is lost.
- Defence in depth: `channel_peek`, `channel_events_watch`, `channel_post`, and the desktop's reads refuse a remote DM whose `account` is not the runtime's current one.

The runtime gates its own interfaces. Anyone using the same OS account can read the files; people who share a computer should use separate OS accounts, each with its own runtime.

### D6 — Clients without a runtime

PekoHub's `GET /v1/public/pekos/:owner/:name/history?before=&after=` (signed-in callers only, never visitors) is relayed as a proxied request whose body is `{ "kind": "history", before?, after?, limit? }`, with the caller's bridge token. The host runs the proxy's usual exposure and `Chat` check, then serves only the pair DM of the token's account (F6), answering `{ status: 200, body: { events, hasMore } }`; a visitor or a missing conversation gets an empty page.

### D7 — The Hub only relays

The Hub checks D3's first point (as for every person-authored envelope), relays both messages, rate-limits history requests per runtime, and never stores or logs their content. It sees history in transit, as it sees live messages today; end-to-end encryption is a separate decision.

## 3. Alternatives considered

- **The Hub stores conversations.** Rejected: the Hub stays a relay that holds no messages.
- **Devices sync with each other.** Needs two of the person's devices online together, and neither has the whole record; the host does.
- **Keep per-device history.** The status quo: a new device starts empty and a changed-hands runtime leaks (F5).
- **Hide, not delete, the previous person's DMs.** Any bug in the hiding exposes them; the host has the record, so deleting loses nothing but undelivered posts, which D5 asks about first.

## 4. Consequences

- A person sees their whole conversation with a remote peko on any runtime registered to their account, and on a phone, whenever the host is online.
- The host's record becomes reachable from outside, but only to the person in it, only through the runtime the Hub binds to their account, and only while the owner lets them talk to the peko.
- The Hub is trusted for the runtime ↔ account binding, as ADR-090 D3 already trusts it; a dishonest Hub could request a person's history. Signing people's requests with a key of their own would remove that trust and is left for later.
- Group history for new members, attachment bytes, and search across synced history are not covered.

## 5. Implementation

- **Channel store** (`peko-channel`) — `meta.account`; `ensure_human_dm(…, account)`; `remote_dm_account`, `remote_dms`, `append_synced_events` (merge by `msg_id`), `history_page` (cursor paging), `purge_remote_dms`.
- **Tunnel** — `TunnelChannelHistoryRequest` / `TunnelChannelHistory` with contract samples; `sign_history` / `verify_history_request` / `verify_history` (version tags `peko-channel-history-request/1`, `peko-channel-history/1`); `tunnel/dm_history.rs`: `sync_history`, `history_older`, `sync_remote_dms`, the pending-request table, and the host's `answer_history`; the dispatcher's `handle_history_request` (D3) and `proxied_history_page` (D6); catch-up on `tunnel_ready` and `peer_runtime_online`. The outbox drops entries whose DM was deleted.
- **Registration** — `register_runtime` deletes other accounts' DMs; `unregister_runtime(…, discard_unsent)` refuses while posts are unsent, then deletes them all.
- **IPC** — `channel_history_older` → `channel_history_page`; `runtime_unregister.discard_unsent`; `channel_open_remote_dm` starts a catch-up; `channel_post` / `channel_peek` / `channel_events_watch` / `channel_history_older` refuse another account's DM.
- **PekoHub** — relays both messages (the person check on the request), rate-limits requests per runtime, and serves the D6 route.
