# ADR-090: The local user talks to a peko on another runtime over channels, not the Hub's chat proxy

**Status:** Accepted (2026-11-01)
**Date:** 2026-11-01
**Counterpart:** PekoHub ADR-007 (the desktop's OAuth token), the PekoHub Phase 1 gap map (`pekohub/docs/GAP_MAP_2026-11.md`: web chat removed; `POST /v1/public/pekos/:owner/:name/chat` kept for clients without a runtime). One Hub-side check is required (D3).

**Preserves:** [ADR-072](ADR-072-human-started-runs-and-durable-delivery.md) (only humans and cron start runs; the outbox is the durable delivery), [ADR-073](ADR-073-actor-addresses-and-conversation-bindings.md) (pair DM ids; human posts are attested by their runtime and its member row), [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (stamps, compartments, trust from the owner and grants), [ADR-081](ADR-081-actor-identity-ownership-and-single-online-copy.md) (one local user per runtime, registered with one Hub account; online links), [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) (channel kind, runtime-local bindings, a DM wakes on human posts), [ADR-089](ADR-089-runs-recover-from-disruptions.md) (a message to a parked session waits with it).

---

## 1. Context

Peko desktop always runs next to a local runtime, yet it chats with pekos hosted elsewhere through the Hub's HTTP chat proxy (`HubRemoteClient`). The conversation record then lives on the remote runtime only, the desktop keeps a copy of its own, and the proxy is the one piece of PekoHub that carries chat. Verified on `master` @ `a8e35953`:

| # | Fact | Where |
|---|---|---|
| F1 | `ChannelCreate` takes a loaded local principal as creator. The local user cannot open a DM with `principal:hub:<UUID>`; IPC DMs come only from `peko send` / ingress, which DM a *local* principal. | `ipc/handlers/channel.rs` (`ChannelCreate`), `principal/child_turns.rs` `ensure_child_ingress` |
| F2 | Cross-runtime channels exist between principals: a signed `tunnel_channel_invite` bootstraps a mirror on the invitee's runtime, events fan out once per recipient runtime, and an outbox resends each envelope until its recipient confirms it. | `tunnel/tunnel_channel_port.rs`, `tunnel/channel_outbox.rs`, `daemon/state.rs` `dm_channel_mirror_bootstrap` |
| F3 | The receiver accepts an invite only when its creator is a `did:key` with an author signature ("Principal invites require a signing key proof"). A human has no signing key. | `tunnel/dispatcher.rs` `handle_inbound_tunnel_channel_invite` |
| F4 | Events authored by a Hub human (`user:hub:<id>`) are already accepted: runtime-signed, no author signature, and only from the runtime the mirror's member row names for that human. | `tunnel/dispatcher.rs` `handle_inbound_tunnel_channel_event` (human branch) |
| F5 | The local user's registry key is `local@<runtime DID>`; registering the runtime adds its Hub account as an alias (`user:hub:<account>`). Outbound human events carry the alias. | `channel/src/store.rs` `ensure_local_user` / `link_local_user`, `tunnel_channel_port.rs` `event_envelope` |
| F6 | The Hub relay checks that an envelope's `source_runtime_id` is the connection's runtime, and that a `did:key` author is linked to it (ADR-081 D7). It does not check a `user:hub:<id>` author. | `pekohub/backend/src/services/tunnel-manager.ts` `handleChannelEventForward` / `handleChannelInviteForward` |
| F7 | On a runtime, a Hub-proxy chat from `user:hub:<id>` lands in the pair DM `dm_channel_id(principal key, key of user:hub:<id>)`; for an account the runtime does not host, that key is the address `user:hub:<id>` itself. | `principal/child_turns.rs` `ensure_child_ingress`, `store.rs` `register_actor` |
| F8 | A DM's subscriber is a no-op: a human's DM turn is driven by the ingress path that posted the message. Nothing drives a turn for a human post that arrives over the tunnel. | `daemon/channel_binding.rs` `select_responder` |
| F9 | Resolving a private peko by Hub ID needs the caller's Hub account; anonymous lookups resolve public pekos only. The runtime holds no account token. | `pekohub/backend/src/routes/api/principals.ts`, `tunnel/hub_directory.rs` |

## 2. Decision

### D1 — A DM between the local user and the remote principal

The conversation is an ordinary `dm` channel (ADR-082 D1) with two members: the runtime's local user and the remote principal.

- **Who.** On the user's runtime the members are `user:local` and `principal:hub:<UUID>`. On the wire, and on the peko's runtime, the user is `user:hub:<account>` — the Hub account the user's runtime is registered under (ADR-081 D3). An unregistered runtime cannot open one.
- **Pair id.** `dm_channel_id(principal's immutable actor key, "user:hub:<account>")`. Both runtimes compute it from what the invite carries. On the peko's runtime it is the same id F7 gives that account's Hub-proxy conversation, so the peko keeps one conversation with a person whether they come through the proxy or through their runtime.
- **The user's side** holds the conversation record: `user:local` reads and writes it with `channel_post`, `channel_peek` and `channel_events_watch`. It has no binding — no local principal takes part.

### D2 — Opening: `channel_open_remote_dm`

New IPC request `channel_open_remote_dm { hub_id, token? }` → `channel_remote_dm_opened { channel, peer, hub_id, name, created }`. Local callers only (it may carry the user's Hub token).

1. The runtime resolves `hub_id` through `GET /v1/principals/by-hub-id/:hub_id`. `token` is the desktop's PekoHub access token (PekoHub ADR-007), sent as `Bearer` for this request only and never stored; without it only public pekos resolve (F9).
2. It refuses a peko hosted on this runtime (talk to it directly), a `local_only` one, and an answer naming another Hub ID or no signing DID / actor key.
3. It records the peko in the actor registry, find-or-creates the channel (D1), records the peko's runtime as the channel's remote member, and queues the invite (D3). Calling it again is safe: it returns the same channel and re-sends the invite only when the peko's runtime changed (ADR-081 D7) or none was ever queued.
4. When the directory cannot be reached, a DM opened before is returned from the registry, unchanged; a new one fails.

### D3 — The invite is the user's, vouched for by their runtime and the Hub

A user-authored `tunnel_channel_invite` has `creator = creator_did = source_principal_did = "user:hub:<account>"`, `kind: dm`, two initial members (the user, on the source runtime; the invitee, addressed to the receiver), the runtime counter-signature, and no author signature. The receiver accepts a creator without a signing key only in exactly this shape; every other unsigned invite is still refused (F3).

A human has no key, so the human's identity rests on the source runtime (as ADR-073 already says of human posts) and on the Hub's binding of that runtime to one account. **The Hub relays an envelope whose `source_principal_did` is `user:hub:<id>` only from the runtime registered to account `<id>`** — the human counterpart of ADR-081 D7's check for principals (F6). This is a PekoHub change; until a Hub makes it, a registered runtime could claim another account's identity in envelopes it sends, which matters most where the owner trusts that account (D4).

### D4 — The receiving runtime: compartment and trust

On the invite the peko's runtime checks, before writing anything:

- the invitee is a principal hosted here, registered, and not `local_only`; the invite's invitee row carries its Hub ID and actor key;
- the pair id is D1's;
- a `private` peko has granted `Chat` to `user:hub:<account>` (its owner always may), as for principal invites.

It then binds a new session — the standing child `/user-<account>` — in the conversation's own compartment (`Compartment::Conversation { channel }`, ADR-078 D3), and creates the mirror. If the Hub-proxy conversation of D1 already exists, the mirror is that channel: the user's runtime is added as the human's route and the existing binding stays.

Trust is ADR-078's, with no new setting: `user:hub:<account>` is `Owner` when it is the peko's owner, `Trusted` when the owner granted it `Trusted`, and `Untrusted` otherwise. The trust floor, the policy reviewer, and owner approvals apply as for any conversation.

### D5 — Human posts wake the peko

A DM always wakes on human posts (ADR-082 D3). A human root post that arrives over the tunnel on a DM is ingress, like `peko send` or the Hub proxy: the runtime appends it, then starts a turn in the peko's bound session acting for the author (`Origin::Ingress`). One run per session — a post during a run becomes steering, and a post to a parked session waits with it (ADR-089 D2). The answer is posted to the channel and fans out to the user's runtime. Principal posts and replies never wake anyone (ADR-072 D1–D2).

### D6 — When either runtime is offline

Delivery is ADR-072 D6's outbox, unchanged: every envelope (the invite first, then posts) waits on its sender's runtime until the recipient's runtime confirms it, resent on a backoff and on every reconnect, for up to 30 days.

*Amended 2026-11-02:* when the Hub drops an envelope because its recipient is offline, it remembers that the sender waits on that runtime (ids only; it never keeps an envelope). When the recipient connects, the Hub sends each waiting sender that is still connected `peer_runtime_online { runtimeId }`, and the sender resends everything pending for that runtime at once instead of at its next backoff tick (up to an hour). A message still needs both runtimes online at some moment; the Hub holds no messages.

- **The peko's runtime is offline.** The user's posts are in their own record at once and are delivered when that runtime is back; the peko answers then. Posts that reach it before the invite are left unconfirmed and resent.
- **The user's runtime is offline.** The peko's answers wait in its runtime's outbox and arrive when the user's runtime reconnects.
- **The peko moved to another runtime.** Posts follow its online link (ADR-081 D7); reopening the DM re-sends the invite to the new runtime.
- **The Hub directory is unreachable.** D2 step 4.
- **The person has several runtimes** registered to one account. Each keeps its own record of the DM; on the peko's side it is one conversation, and every one of those runtimes that opened it receives the answers.
- **The user's runtime is unregistered.** Its local user loses the Hub alias, so posts to remote DMs fail to build ("Human sender has no Hub identity") until it is registered again.

### D7 — Not covered here

Attachments on this path (ADR-088 files stay on the sending runtime), visitors (no runtime), and switching the desktop over (`src-tauri/src/clients/hub_remote_client.rs`, a later desktop PR). The Hub proxy stays for clients without a runtime.

## 3. Alternatives considered

- **Keep the Hub proxy for desktop chat.** The record stays split, and the Hub keeps carrying chat it was meant to stop carrying.
- **A local principal as go-between** (a hidden peko that DMs the remote one). The remote peko would see a principal, not the person: the wrong trust (never `Owner`), and turns would not wake (ADR-072 D1).
- **A signing key for the local user.** It would let the receiver check the human without the Hub, but the receiver still could not tie the key to a Hub account without asking the Hub. The Hub relay check (D3) gets the same assurance with no new key.
- **Send the user's Hub token to the receiver.** It would leak a bearer credential to another runtime.

## 4. Consequences

- The desktop can message any peko it may reach with its runtime's channels; the conversation record is on the user's machine, and the peko's runtime keeps its own.
- A person keeps one conversation with a remote peko however they come in (D1).
- The tunnel now carries one kind of unsigned invite. It is narrow (a two-member DM from a Hub human), and the Hub check of D3 is what makes the human's identity trustworthy.

## 5. Implementation

- **Channel store** — `ChannelStore::ensure_human_dm` (the user's side of D1).
- **Directory** — `AgentDirectory::resolve_by_hub_id_for_user(hub_id, bearer)`.
- **Tunnel** — `TunnelChannelPort::open_hub_dm` (D2); outbox item `user_invite` and its envelope (D3); the dispatcher's human-invite branch, and a `TunnelHost::human_dm_post` hook after a human DM post is applied (D5).
- **Daemon** — the human branch of `dm_channel_mirror_bootstrap` (D4); `PrincipalManager::receive_channel_dm_post` drives the turn (D5).
- **IPC** — `channel_open_remote_dm` / `channel_remote_dm_opened`.
- **PekoHub** — the relay check of D3 (separate PR).
