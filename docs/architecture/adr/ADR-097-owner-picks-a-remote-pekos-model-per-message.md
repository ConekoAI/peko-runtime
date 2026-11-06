# ADR-097: The owner picks a model for one message to their peko on another computer

**Status:** Accepted (2026-11-06)
**Date:** 2026-11-06
**Builds on:** [ADR-095](ADR-095-symmetric-dms-that-follow-devices-and-moves.md) (a person's DMs follow them to every runtime of their account), [ADR-091](ADR-091-remote-dm-history-from-the-host.md) (runtime-to-runtime requests through PekoHub, answered by the host), [ADR-090](ADR-090-local-user-dms-a-remote-peko-over-the-tunnel.md) (the person's post over the tunnel starts the peko's turn), [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (stamps and trust), [ADR-081](ADR-081-actor-identity-ownership-and-single-online-copy.md) (one owner per peko; the local user and their Hub account are one actor).
**Counterpart:** PekoHub (relay of two new tunnel messages; the `model` field on relayed posts). Peko desktop (the composer's model picker for the owner's own pekos elsewhere).

---

## 1. Context

The owner's pekos on their other computers now sit in the desktop's Chats like local ones (ADR-095). One difference is left: the composer's per-message model picker ("Peko's model" / a configured model for this message) shows only for pekos on this computer. Verified on `master` @ `3980554d`:

| # | Fact | Where |
|---|---|---|
| F1 | A local message picks its model with `principal_send_stream.override_model` (`peko send --model`); the turn runs it through `ExecutionConfig::model_override` and the spec gate, for that turn only. | `ipc/packet.rs` `PrincipalSendStream`; `principal/child_turns.rs` `drive_turn_streaming_with_media` |
| F2 | The picker lists this computer's configured models (`model_list`). A peko elsewhere runs on its host's models, which this computer cannot see. | desktop `useModels`; `daemon/state.rs` `list_catalog_models` |
| F3 | A message to a peko elsewhere is an ordinary `channel_post` in the DM copy here. It reaches the host as a signed `Posted` event, rebuilt from the stored event by the outbox until delivered, and the host starts the peko's turn from it (`human_dm_post` → `receive_channel_dm_post`). Nothing in it names a model. | `tunnel/channel_outbox.rs`; `daemon/state.rs` `human_dm_post`; `principal/manager.rs` |
| F4 | The host stamps that turn for the post's author; the peko's owner posting from their other computer is `Owner` (the local user and `user:hub:<account>` are one actor). | `security/stamps.rs` `trust_for`; ADR-081 |
| F5 | PekoHub relays a person's envelope only from a runtime registered to that person's account, and forwards channel events verbatim. | PekoHub `speaksForItsOwnPerson`, `handleChannelEventForward` |

## 2. Decision

### D1 — The host's models, asked for by the owner's other computer

Two tunnel messages, both runtime-signed (`sign_history`), between two runtimes of one account:

- `tunnel_peko_models_request { requestId, sourceRuntimeId, recipientRuntimeId, sourcePrincipalDid, principalDid, signature }` — the person (`sourcePrincipalDid = user:hub:<account>`) asks the runtime hosting the peko whose signing DID is `principalDid` which models it may run. Payload version `peko-peko-models-request/1`.
- `tunnel_peko_models { requestId, sourceRuntimeId, recipientRuntimeId, principalDid, models, signature }` — the host's answer: `models` is `[{ id, displayName }]`, the host's **enabled** configured models, in catalog order. Nothing else about a model crosses (no endpoint, wire id, headers, or credential). Payload version `peko-peko-models/1`; `models` is signed as `models_b64u`.

The host answers only when `principalDid` is a peko hosted there **whose owner is `sourcePrincipalDid`** (the same actor, F4); anything else gets an empty list, so a probe learns nothing. The asker accepts only an answer it is waiting for, from the runtime it asked, about the peko it asked about, and waits 15 s (an offline host: "did not answer").

On the asker, IPC `channel_peko_models { channel }` (local user only) names the DM with the remote peko and answers `channel_peko_models { models }`. It refuses a channel that is not this account's DM with a peko elsewhere.

### D2 — A post may ask for a model; the host honours it only for the owner

- `ChannelEvent::Posted` gains `model: Option<String>` — a configured model id the author asks the receiving peko to answer this one post with. Omitted when absent, so existing lines are byte-identical. It is part of the signed event (`event_b64u`) and of the stored record, which is what lets the outbox resend it (F3).
- IPC `channel_post` gains `model`; only the local user may set it.
- The host passes it to the turn the post starts (`receive_channel_dm_post`) as the turn's `override_model` (F1) **only when the turn's stamp is `Owner` and the id names an enabled model in the host's catalog**. Otherwise the peko answers with its own model, as if no model were asked. A post that reaches a busy peko steers the running turn and its model is not applied, as for a local message sent while the peko works.
- Nothing else reads `model`: group wakes, peer posts, history, and the desktop's thread ignore it.

### D3 — The Hub only relays

The Hub relays `tunnel_peko_models_request` only when the source runtime speaks for `sourcePrincipalDid` (F5) and the recipient runtime is registered to the same account, and `tunnel_peko_models` only between two runtimes registered to the same account. Requests share the history request rate limit. It stores nothing. `model` on a relayed post needs no Hub check (F5): the host decides.

### D4 — Desktop

For a peko elsewhere that follows the account (`fromAccount`), the composer lists the host's models from `channel_peko_models` and sends the pick as `channel_post.model`. Other people's pekos keep no picker: their host would not honour it (D2). The pick is remembered per peko, for local pekos too, across switching pekos and restarts (the desktop's local storage); a remembered id that is not listed shows and sends as the peko's own model.

## 3. Alternatives considered

- **The model on the envelope, not the event.** The outbox rebuilds envelopes from stored events until they are delivered (F3), so an envelope-only field would be lost on a retry; carrying it on the event also keeps it inside the existing signature.
- **The Hub keeps each runtime's model list.** Rejected: the Hub stays a relay, and a model list is the host's live configuration (models are added and disabled at any time).
- **Send the asker's own model configuration.** The host would have to run a configuration (and a credential) it never saw. Rejected.
- **Honour the model for anyone the peko trusts.** The model spends the owner's money and quota; only the owner chooses it, as locally.

## 4. Consequences

- A chat with the owner's own peko on another computer offers the same model picker as a local one, listing that computer's models.
- A post's `model` is visible to the DM's members in its record; it carries no authority beyond the owner's turn.
- An unknown or disabled model is ignored, not refused: the message is never lost, and the answer comes from the peko's own model.

## 5. Implementation

- **Protocol** (`peko-protocol`) — `ChannelEvent::Posted::model`.
- **Channel store** (`peko-channel`) — `PostMsg::model`, written onto the event.
- **Tunnel** — `TunnelPekoModelsRequest` / `TunnelPekoModels` with contract samples; `PekoModelsRequestPayload` / `PekoModelsPayload` and their verifiers; `tunnel/peko_models.rs`: the asker's `peko_models` with its pending table and `on_peko_models`; the dispatcher verifies a request and answers through the new `TunnelHost::peko_models_for_owner`.
- **Daemon** — `AppState::peko_models_for_owner` (enabled catalog models, only for the peko's owner); `human_dm_post` passes the post's `model` to `receive_channel_dm_post`, which applies it for an `Owner` stamp and an enabled model.
- **IPC** — `channel_peko_models` → `channel_peko_models` response; `channel_post.model`.
- **PekoHub** — the two relays and the `model` field on its `ChannelEvent` type.
- **Desktop** — `HubRemoteClient::models` / `send(…, model)`, the `remote_peko_models` command, `useRemotePekoModels`, and the composer's per-peko remembered pick (`lib/modelChoice.ts`).
