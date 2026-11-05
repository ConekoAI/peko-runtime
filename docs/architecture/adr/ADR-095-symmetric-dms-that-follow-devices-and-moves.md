# ADR-095: Every DM is one conversation, kept whole by every runtime in it; it follows people to new devices and pekos to new runtimes; people DM people

**Status:** Accepted (2026-11-04); phases 1–4 built
**Date:** 2026-11-04
**Builds on:** [ADR-073](ADR-073-actor-addresses-and-conversation-bindings.md) (pair DM ids), [ADR-081](ADR-081-actor-identity-ownership-and-single-online-copy.md) (immutable keys, snapshots, the online link), [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) (DM kind, bindings), [ADR-090](ADR-090-local-user-dms-a-remote-peko-over-the-tunnel.md) (a person's runtime-vouched invite), [ADR-091](ADR-091-remote-dm-history-from-the-host.md) (history messages, merge by `msg_id`, changing hands), [ADR-092](ADR-092-groups-with-a-home-people-and-hub-ids.md) (pending invitations, per-account delivery).
**Amends:** ADR-091 D1 and D3 (no host/cache split for DMs; members other than the person may ask), ADR-081 D3 (registering rekeys the local user's DMs, D7) and D6 step 5 (DMs reattach with their history).
**Counterpart:** PekoHub: the relay check for person-to-person envelopes, delivery of a request to an account's runtimes.

---

## 1. Context

A DM should be one conversation however many machines take part: a person who signs in on a new computer, or a peko that moves to a new runtime, keeps writing in the same channel and sees what was said before. Verified on `master` @ `13b4875b`:

| # | Fact | Where |
|---|---|---|
| F1 | A DM's id hashes the two members' immutable keys (`dm_channel_id`), so every runtime computes the same id. A person on the tunnel is keyed by `user:hub:<account>`; a principal by its actor key. | `channel/src/port.rs` `dm_channel_id`; `tunnel/user_dm.rs` `human_dm_channel_id` |
| F2 | A peko-to-peko DM has no owner and no home. The inviter is recorded in `meta.creator` only. Each runtime keeps a full copy from the invite on, and binds a session for its own peko in its own compartment. | `daemon/state.rs` `dm_channel_mirror_bootstrap`; `channel/src/store.rs` `MetaJson` |
| F3 | Copies are kept in step only by live delivery (the outbox, ADR-072 D6). The history messages of ADR-091 answer only a person asking about their DM with a peko hosted on the answering runtime; a principal is never served. | ADR-091 D3; `tunnel/dm_history.rs` |
| F4 | ADR-091 makes the peko's host the record and the person's devices caches: scroll-back is fetched, not stored. | ADR-091 D1, D4 |
| F5 | A snapshot carries the peko's sessions and `local/channels.json` (each channel's id, kind, participants, and the peko's bound session), not the channel logs. Import does not replay `channels.json` into the channel store. A pair DM reattaches only when the moved peko next speaks in it; traffic from the peer first does not reattach. | `ipc/handlers/principal.rs` (export), `registry/packaging/principal_unpackager.rs`; ADR-081 D6 step 5 "Open" |
| F6 | A new device learns of a person's DM with a remote peko only when it is opened there (`channel_open_remote_dm`); nothing lists the person's past DMs. Groups have such a list (ADR-092 D4); DMs do not. | ADR-091 D4 |
| F7 | The local user's registry key is `local@<runtime DID>` and never changes when the runtime registers. Its DMs with pekos on its own runtime are keyed by that local key. | `channel/src/store.rs` `ensure_local_user`; ADR-081 D3 |
| F8 | A person can DM only a peko. The unsigned-invite exception is a person inviting a principal; nothing creates a DM between two people. | ADR-090 D3; `dispatcher::is_human_dm_invite` |

## 2. Decision

### D1 — A DM is symmetric: every runtime in it keeps the whole conversation

A DM has two members and no owner, home, or host. Every runtime that is a **route** for a member (the peko's linked runtime; each runtime registered to a person's account that has the DM) keeps a full copy and serves it. No copy is a cache.

- Copies merge by `msg_id` as in ADR-091 D1: a post this copy lacks is appended; the log stays append-only, so line cursors stay valid. Clients show posts ordered by `at`, then `msg_id`, not by line.
- Backfilled pages are **stored**, back to the conversation's first post. ADR-091's unstored scroll-back (`channel_history_older`) stays for a copy still filling, and reads from the copy when it has the page.
- `meta.creator` remains a record of who opened it, nothing more.

This amends ADR-091 D1: a person's copy of a DM with a peko is a full copy, and the peko's host is one copy among them.

### D2 — Who may ask a copy for history

ADR-091's two messages carry every DM. A runtime answers a `tunnel_channel_history_request` for a DM it holds only when **all** hold:

1. **The requester is one of the DM's two members**, proven as for its posts:
   - a person, `user:hub:<account>`, Hub-vouched from a runtime registered to that account (ADR-090 D3);
   - a peko, `principal:hub:<id>`, signed by its key, from the runtime its online link points at (ADR-081 D7).
2. **The DM is theirs.** `channelId` is the pair id of the requester and the other member, and its kind is `dm`.
3. **They may still talk.** As ADR-090 D4 for a person and a peko; for a person and a person, neither has blocked the other (D6).

Either member may ask, and so may the same member from another of its routes: a person's new device asks their old one; a moved peko asks its peer. Refusals stay empty pages. Pages are stripped as in ADR-091 D3.

### D3 — When a copy catches up, and from whom

Triggers: opening the DM; `tunnel_ready`; `peer_runtime_online` for a runtime that holds a copy; and finishing an import (D4). A runtime asks, in this order, until one answers:

1. the other member's routes;
2. the same member's other routes.

It asks for pages after the newest `msg_id` it holds, then fills backwards to the first post, at most 20 pages a pass, resuming on the next trigger. A DM with any copy online catches up; nothing is queued at the Hub.

A copy a peko reads catches up **forward only**. A peko's unread digest tracks the DM's lines, so older posts appended to its copy would read as new messages for it. Such a copy only ever lacks older posts after a move, and D4 brings those in the snapshot.

### D4 — A moved peko takes its DMs with it

- **Snapshots carry DM logs.** A snapshot includes the `events.jsonl` (all pages) of every DM the peko is a member of, beside `local/channels.json`. Groups are not included; they catch up from their home (ADR-092 D3).
- **Import recreates them.** For each DM in `channels.json`, import creates the channel at the same id with the snapshot's participants, merges the carried log by `msg_id` (a copy already here keeps its lines), and restores the binding to the imported session. Nothing is invited, nothing is woken.
- **After the move.** When the online link moves (ADR-081 D7), each peer's outbox already moves its route to the new runtime. The new runtime catches up each DM (D3) for what was said while the snapshot travelled. Traffic from a peer first now lands in a DM that exists. This closes ADR-081 D6's open item for DMs.
- A rotated import (`--rotate-keys`) is a new actor and carries no DMs, as today.
- The old runtime's copy stops receiving (ADR-081 D7) and stays as it is.

### D5 — A person's DMs follow them to every device

- **Which DMs.** A newly registered runtime asks the person's other runtimes: `account_dms_request`, addressed to the account, which PekoHub delivers to every runtime registered to it (as it delivers group invitations, ADR-092 phase 2). Each answers `account_dms { dms: [{ channel, peer }] }` with the account's DMs it holds. The new runtime opens each one and catches it up (D3). PekoHub keeps no list of who a person talks to.
- **Changing hands** stays ADR-091 D5: a runtime whose account changes deletes the previous account's DMs.
- **Phones** read through the Hub's proxy (ADR-091 D6), served by any copy.

### D6 — People DM people

A DM between two people is an ordinary `dm` channel with the two members `user:hub:<a>` and `user:hub:<b>`; it never gains members. To bring pekos into a conversation, start a group (ADR-092).

- **Opening.** IPC `channel_open_person_dm { hub_id }` → `channel_person_dm_opened { channel, peer, created }`. The pair id is `dm_channel_id("user:hub:<a>", "user:hub:<b>")` on every device of either person. An unregistered runtime cannot open one.
- **Invite.** A runtime-signed invite with `creator = user:hub:<a>`, `kind: dm`, no author signature, addressed to account `<b>`; PekoHub delivers it to every runtime registered to `<b>`, and relays it only from a runtime registered to `<a>`. This widens ADR-090 D3's exception from "a person invites a peko" to "a person invites a person", nothing more.
- **Consent.** On `<b>`'s runtimes the DM arrives as a **message request** that names only who asks, until `<b>` accepts on any device (`peko channel accept|decline|block`, desktop). Until then `<b>`'s runtimes say nothing to `<a>`'s: no route, no receipt, so `<a>` learns neither which runtimes `<b>` has nor that the request was seen. `<a>`'s posts stay in `<a>`'s own copy. Accepting makes that runtime a route and tells `<a>`'s runtime, then catches up `<a>`'s posts from it as history (D2–D3). Declining removes the request; **blocking** the account also refuses its invites, posts, and history requests on all of `<b>`'s runtimes (the block list travels with D5's answer).
- **Routes.** A runtime that opens or accepts the DM announces itself to every known route of both members (`tunnel_dm_route { channel, account }`, Hub-checked like a post). Posts fan out to every route; ADR-092 phase 4 already keeps a person's other runtimes as routes.
- **No peko in it.** No binding on either side; nothing wakes. A person-to-person DM belongs to no principal's compartment: `Channel`, `ChannelRead`, `Session`, `ToolLog`, and the file tools refuse it, owner runs included. `Bash` runs as the OS user and can read the files; the runtime says so rather than claiming otherwise (as ADR-079 does for the ledger).
- **Delivery** is the outbox's once both have a route: a post waits on the sender's runtime until a route of the recipient confirms it, for up to 30 days. Before `<b>` accepts there is no route, and `<a>`'s runtime resends the invite on `tunnel_ready` and when PekoHub reports one of `<b>`'s runtimes online. A message needs one runtime of each person online at the same moment; the Hub holds nothing.

### D7 — A registered runtime's person is keyed by their Hub account, so your own peko is one conversation on every device

Alice talks to peko P on its host L1 in `dm(P, local@L1)` (F7). From L2 she reaches P as `user:hub:alice`, which is `dm(P, user:hub:alice)`: without this decision, a second conversation.

- **While a runtime is registered**, its local user's DMs are keyed by its Hub address: the pair id is `dm_channel_id(<peer key>, "user:hub:<account>")`. The local user's registry key stays `local@<runtime DID>` (ADR-081 D3); only the address its DMs hash changes.
- **Registering rekeys.** For each DM of the local user keyed by `local@…`, the runtime moves the channel to the Hub-keyed id: the log (all pages), cursors, and bindings move with it; each bound session is rebound and its compartment set to the new channel (as `rebind_owner_dm` does on import); the participants record the Hub address. The old id is kept as a forward (`meta.moved_to`), so a channel id held in a peko's memory, plan, or cron job still resolves. A rekey is a Security audit event per channel; nothing is posted, nothing wakes.
- **Unregistering moves them back** to `local@…` ids, the same way, after ADR-091 D5 has deleted the remote DMs. A later registration under another account rekeys them to that account: the local user is still one person per runtime (ADR-081 D3), and people who share a computer use separate OS accounts (ADR-091 D5).
- **At the host**, `user:hub:<own account>` is the local user. An invite or post from Alice's other runtime lands in the existing DM, and that runtime becomes a route for her (ADR-090 D4's "existing conversation" case, now reached for the owner too).
- **On import**, the owner's DM is already Hub-keyed when the owner has a Hub account, so it keeps its id across runtimes and D4 carries it like any other DM. `rebind_owner_dm` remains for owners who are only a local user.
- **Peko-to-peko and person-to-person DMs** are unaffected: a peko's key never changes, and D6 is Hub-keyed from the start.

## 3. Alternatives considered

- **A host per DM** (ADR-091's model for every DM). Someone has to be the host; for two people neither is, and for two pekos it was never needed. A host also leaves history unavailable whenever its one machine sleeps, while symmetric copies answer from any member's machine.
- **Person-to-person conversations as two-person groups.** Reuses ADR-092's home, roster, and history, but a DM never changes members and has no owner; a home for a pair is the asymmetry this ADR removes.
- **Snapshots without channel logs, catching up from peers only.** Simpler snapshots, but a peko moved while its peers are offline would have no conversations, and nothing could recover a DM whose peer is gone.
- **PekoHub lists each account's DMs.** Lets a new device find DMs when the person's other runtimes are offline, at the cost of the Hub learning who talks to whom. Rejected; groups accept it (ADR-092 D6) because a group is a thing others join.
- **Keep the local key and map `user:hub:<own account>` to it at the host** (instead of D7). The other device cannot compute `local@<host DID>`, so the host would have to send the channel id in its answer, and DM ids would stop being derivable from the two members — every runtime would need a per-channel alias table.
- **PekoHub stores undelivered posts (encrypted).** Would deliver between two laptops that are never awake together. Not decided here; the Hub stays a relay that holds no messages.

## 4. Consequences

- A DM survives any one machine: a new device, a moved peko, a lost disk all catch up from the other member, or from the member's other machines.
- Neither member can unsay a message: once another copy has it, it is that copy's. Desktop text must not promise deletion for both.
- A copy can be asked for history by the other member. ADR-091 already exposed the host's record this way; now every copy is reachable, with the same checks.
- Snapshots grow by the peko's DM logs, and carry what its peers said to it, which its sessions already partly held.
- Clients order a DM by `at`, since backfill appends older posts after newer lines.
- Two people both offline-prone may see posts arrive late; the desktop shows "waiting for <name>'s computer" instead of pretending they were delivered.

## 5. Decided at review (2026-11-04)

1. **Person-to-person conversations are DMs**, not two-person groups (D6).
2. **Your own peko from another computer**: rekey on register (D7), not a mapping at the host. The split it fixes (F7) was read from the code, not reproduced live; phase 3 starts with a live check.
3. **A message request reveals nothing before it is accepted** (D6): the recipient's runtimes send no route and no receipt until the person accepts, and the sender's posts reach them afterwards as history.

## 6. Open

1. **Old copies after a move.** Keep (as D4 says), or delete once the link has moved and the new runtime has caught up.
2. **Size limits.** Whether snapshots cap carried DM logs, and whether backfill stops at an age.

## 7. Implementation outline (phases, each its own PRs)

1. **Symmetric history for peko DMs.** Generalize `answer_history` (D2) and catch-up (D3) to principal members; merge stays `append_synced_events`. Desktop and `peko log` order by `at`.
2. **Moves.** Pack DM logs in snapshots; import recreates DMs, merges, and rebinds (D4); catch up after the link moves.

   *Phase 2 notes (as built).* Export writes `local/dms/<pair id>.jsonl` (posts in history-page form) and a `remote` list per channel in `channels.json`. Import parks the logs in `local/dms.pending/`; once the principal is loaded (its actor key registered, so pair ids come out right) the import handler replays them through `ChannelPort::restore_dm_copy` and removes the folder. The other member is reached by its Hub ID; the source runtime's local user becomes this runtime's local user, so the owner's DM lands at the id `rebind_owner_dm` already moved its sessions to; a peko known only on the source runtime stays behind. The moved copy is then a peko-to-peko DM like any other and catches up forward on `tunnel_ready` (phase 1). Attachment files stay on the source runtime: posts keep their metadata, not their paths.
3. **Devices and rekeying.** First reproduce F7's split live (two runtimes on one account, one hosting the peko). Then D7: a channel-store move of a DM to a new pair id (log, cursors, bindings, `moved_to` forward — no such operation exists yet), rekey on register and back on unregister, the host treating its own account as the local user, and `resolve` following `moved_to`. Then D5's `account_dms_request` / `account_dms` and stored backfill (D1).

   *Phase 3a notes (as built).* F7 is reproduced by `your_own_peko_is_one_conversation_on_every_device`: without the change, the host keeps its DM at `dm(peko, local@host)` while the other computer opens `dm(peko, user:hub:account)`. `ActorInfo::dm_key` names the local user by its Hub address while it has one; every principal-to-peer pair id uses it. `ChannelStore::rekey_local_user_dms` moves each of the local user's DMs whose id no longer matches. Into a free id the directory moves and every event line takes the new id; into an existing DM posts merge by `msg_id`, and members, bindings, and a complete participant pair join. On the move back to the local key, person routes are dropped. `principal::dm_rekey` rebinds bound sessions and conversation compartments. It runs at boot, on `tunnel_ready`, and after unregister, because registration may run in the CLI process. The forward of D7 is kept in `<runtime>/channel_forwards.json` (old id → new) rather than `meta.moved_to` (the old directory is gone); `ChannelPort::forwarded` follows it, and the `Channel`, `ChannelRead`, and `ChannelSend` tools resolve an id through it, so an id a peko kept still reaches the DM. A move back replaces the reverse forward, so there are no cycles. The host needed no change: `user:hub:<own account>` already resolves to the local user, and the person's invite lands in the existing DM. D5 and stored backfill are phase 3b.

   *Phase 3b notes (as built).* On `tunnel_ready` a registered runtime sends `tunnel_account_dms_request` to its account. PekoHub delivers it to the account's other runtimes, only from one of them, and wakes the asker when an offline one connects. Each answers `tunnel_account_dms` with the person's DMs it holds: their DMs with pekos elsewhere, and with pekos hosted there (Hub-keyed by D7), each with the peko's Hub ID, signing DID, key, name, runtime, and exposure. The asker opens each DM it lacks whose id is the person's pair with that peko (so a private peko needs no directory token), queues the invite that makes it a route, and catches up from the host. Answers count only for a request of this runtime, for ten minutes. A person's copy fills backwards after catching up forward, at most 20 pages a pass, and stores what it gets (D1). The asker asks again on its next `tunnel_ready` and on every `peer_runtime_online` (2026-11-05: PekoHub's wake for an account runtime that was offline when asked; before, a device that signed in while the account's others slept found no DMs until it reconnected).
4. **People DM people.** `channel_open_person_dm`, the person-to-person invite, message requests, block list, route announcements, compartment refusals (D6); desktop. PekoHub: the relay check for `user:hub` → `user:hub` envelopes and account delivery for the new messages.

   *Phase 4 notes (as built).*
   - **Opening and requests.** `channel_open_person_dm { account }` (and `peko channel dm user:hub:<account>`) creates the DM at `dm(user:hub:a, user:hub:b)` under the opener's account, and sends `tunnel_person_dm_invite` to `<b>`'s account. PekoHub delivers it to every runtime of that account, and only from a runtime of `<a>`'s. Each of `<b>`'s runtimes keeps a `DmRequest` (`<runtime>/dm_requests/`) and sends nothing back. `<a>`'s runtime resends the invite on `tunnel_ready` and `peer_runtime_online` while no route of `<b>` is known.
   - **Accepting.** `channel_dm_request_answer` (and `peko channel accept|decline|block`) creates the DM, records the asker's runtime as a route, sends it `tunnel_dm_route`, and catches up `<a>`'s posts from it as history. A runtime that already holds the DM treats a later invite as a route.
   - **Routes.** `tunnel_dm_route` carries every route its sender knows. A receiver records the ones it lacks for the DM's two people only, and announces itself to each runtime it just learned of, and to the sender if the sender did not list it. Every route of both people comes to know every other.
   - **Other devices.** The account DM list (D5) gains `people` (each with its routes) and `blocked`. A person's other runtime adopts each DM it lacks, announces itself, catches up, and drops a request it was still holding.
   - **History.** A person DM's copy serves either of its two people (the other person, or the same person's other runtime). A requester asks the other person's runtime first.
   - **Blocking** is kept per account (`<runtime>/blocked_people.json`). It refuses invites, posts, routes, and history requests from that person. Every block and unblock is kept with its time and travels to the person's other runtimes in the DM list, where the latest decision per person wins.
   - **No peko in it.** The trust floor refuses a person DM's directory, `dm_requests/`, and `blocked_people.json` to every run, the owner's included. A `Glob`/`Grep` from a folder containing a person DM is refused too. Channel tools need membership, and no peko is a member. `Bash` remains the exception.
   - **Desktop** (peko-desktop#132): a People list in the Groups sidebar ("invited" until accepted, from `channel_person_dms`), message requests with Accept / Decline / Block, "Message a person" by account ID, a "waiting for <name> to accept" note in the DM, and Blocked people in Settings.
