# ADR-094: Attachments cross runtimes — named by digest, pulled in chunks through PekoHub, capped

**Status:** Accepted
**Date:** 2026-11-04
**Counterpart:** PekoHub — relays `tunnel_attachment_request` / `tunnel_attachment_chunk` within size and rate caps (`backend/src/services/tunnel-manager.ts`). peko-desktop — the composer's attach button and drag-and-drop for remote DMs and groups (`channel_post` with `attachments`); chips read the path the runtime fills in.

**Preserves:** [ADR-072](ADR-072-human-started-runs-and-durable-delivery.md) (the Hub holds no messages), [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (the peko's file tools reach its workspace; runtime state is the owner's), [ADR-088](ADR-088-attachments-in-direct-messages.md) (limits, workspace copies, notes and media for the turn), [ADR-090](ADR-090-local-user-dms-a-remote-peko-over-the-tunnel.md) (a person's DM with a remote peko), [ADR-091](ADR-091-remote-dm-history-from-the-host.md) (history from the host).

---

## 1. Context

A file sent in a local chat stays on the host it was sent from. Verified on `master` @ `d1c6b654`:

| # | Fact | Where |
|---|---|---|
| F1 | `channel_post` — how the desktop posts to a remote DM or a group — carries text only. | `ipc/packet.rs` `ChannelPost` |
| F2 | `Attach` refuses a conversation with anyone on the Hub or another runtime. | `tools/builtin/channel/attach.rs` |
| F3 | A post's `attachments` are `{name, path, mime, size}`; `path` is relative to the receiving peko's workspace. Cross-runtime envelopes carry the event as is; history pages blank `path`. No bytes leave the runtime. | `peko-protocol` `Attachment`, `tunnel/dm_history.rs` `served` |
| F4 | A person's post that arrives over the tunnel starts the peko's turn with its text only. | `principal/manager.rs` `receive_channel_dm_post` |
| F5 | The Hub relays tunnel JSON verbatim over WebSocket (16 MB frames), with per-runtime per-minute caps on events and history requests, and keeps no envelope. | `pekohub` `plugins/tunnel.ts`, `services/tunnel-manager.ts` |
| F6 | A channel event is signed as a compact JWS whose payload embeds the event; a runtime's outbox resends an envelope until it is confirmed, for up to 30 days. | `tunnel_channel_signature.rs`, `channel_outbox.rs` |

Inline bytes in the event (F6) would ride the JWS payload and the event both, and be resent with it. The Hub serves every runtime, so what it relays must be bounded.

## 2. Decision

### D1 — A post names each file by its digest; the channel keeps the bytes

A file that may cross runtimes is kept in the channel's **file store**, `<channel dir>/files/<sha256>` (lowercase hex; nothing else is ever named there), beside its events and removed with the channel. `Attachment` gains `sha256` (omitted when absent). On the wire a post's attachments carry no `path` — it names a place on the sender's disk — and keep `name`, `mime`, `size`, and `sha256`, all inside the signed event.

- **The local user** posts with `channel_post { attachments: [absolute path] }`. Only the local user may attach (the daemon reads the files). Each file goes to the file store; the post records it with no `path`.
- **A peko** posts with `Attach`. It keeps its workspace copy (ADR-088 D7); in a conversation with anyone on another runtime or the Hub, the copy also goes to the file store and the post gains the digest.

### D2 — Caps

A conversation with anyone on another runtime or the Hub takes at most **8 files, 10 MB each, 25 MB per message**. Elsewhere ADR-088's limits stay (8 files, 20 MB each). A request over a limit is refused before anything is posted. The receiving runtime does not fetch a post whose files break the limits.

### D3 — The receiver pulls the bytes, one chunk at a time

When a post with digests arrives over the tunnel, the receiving runtime asks the runtime that sent it for each file it lacks:

```json
{"type":"tunnel_attachment_request","requestId":"…","sourceRuntimeId":"did:key:…","recipientRuntimeId":"did:key:…","channelId":"dm_…","sha256":"<hex>","offset":0,"signature":"<JWS>"}
{"type":"tunnel_attachment_chunk","requestId":"…","sourceRuntimeId":"did:key:…","recipientRuntimeId":"did:key:…","channelId":"dm_…","sha256":"<hex>","offset":0,"size":1258291,"data":"<base64>"}
```

- A chunk is at most **256 KiB**. One request is in flight per file; the next waits for the answer (30 s), so a transfer never puts more than a chunk on the Hub at once.
- The request is signed by the asking runtime (`peko-attachment-request/1`, like history requests). The chunk is not: the receiver writes the chunks to `<sha256>.part`, checks the whole file against the digest the signed post carried, and only then names it by its digest. Bytes that do not match are discarded.
- The sender serves only a runtime that hosts a member of the channel, and only from that channel's store. Every refusal reads as "not here" (`size` 0, empty `data`), so a probe learns nothing.
- A chunk that does not come, or a file the sender does not have, is asked for again after 1, 3 and 10 seconds. A file still missing goes on the runtime's wanted list (`channel_delivery/wanted_files.json`) and is asked for again when the Hub says the sender is back (`peer_runtime_online`) and on every reconnect, for up to 30 days.

### D4 — A post applies once its files are here

The receiving runtime fetches a post's files before it appends the post, so a post never shows — or wakes a peko — without them. Posts of one channel apply in arrival order while a fetch runs; the fetch runs outside the tunnel's dispatch slots, since the chunks it waits for are dispatched too. A file that cannot be had applies the post anyway and stays wanted.

Catching up from a host (ADR-091) fetches the files its pages name, from the host.

### D5 — Who sees the files

- **The peko** whose DM wakes on a person's post gets its own copy of each file in its workspace (ADR-088 D2, under the name the sender gave it) and the turn's notes and media (ADR-088 D4). A file that did not arrive becomes a note saying so. A busy peko gets the notes only (ADR-088 D5).
- **Clients on the host** read a received file's absolute path in its stored copy: `channel_peek`, `channel_events_watch`, `channel_history_older`, and `principal_log` rows fill an empty `path` once the file is in the store; it stays empty while the file has not arrived.

### D6 — PekoHub relays within caps and keeps nothing

The Hub relays both messages like history envelopes: the source must be the connection's runtime (a mismatch closes it). It drops a chunk whose data is over 256 KiB or whose file is over 10 MB, caps each runtime at **50 MB of chunk data a minute** and **400 requests a minute**, and notes a request to an offline runtime as waiting (D3's retry). It never decodes, stores, or logs the bytes. Who may have a file is the sending runtime's decision (D3).

## 3. Alternatives considered

- **Bytes inside the post.** The signed JWS embeds the event, so the bytes would travel twice, be resent by the outbox until confirmed, and land on the Hub as one large frame.
- **A blob store on the Hub.** Works while the sender is offline, but the Hub would keep users' files: storage, retention, and moderation it does not have, against ADR-072's "the Hub holds no messages".
- **The sender pushes chunks after the post.** The outbox would have to keep and resend every chunk; a pull asks only for what is missing, when the receiver is ready.
- **Several chunks in flight.** Faster on a slow link, but lets one transfer put a burst on the Hub; the per-runtime byte cap would then drop chunks instead of pacing them.

## 4. Consequences

- A person can send a photo, a voice memo, or a document to a peko on another computer, and a peko can send files back, in DMs and groups.
- A file needs both runtimes online at some moment, like a message. A file over 10 MB stays local-only.
- A post with files shows on the receiving side a few seconds later than its text would have.
- Files take disk twice on a peko's host (the channel's store and the peko's workspace copy).
- Pekos woken in a group get a group post's text only, as before (ADR-088 §3); the files are in the channel's store and shown to clients.

## 5. Implementation

- **Wire.** `Attachment.sha256`; `ChannelPost.attachments`; `TunnelAttachmentRequest` / `TunnelAttachmentChunk` and `AttachmentRequestPayload`; contract fixture.
- **Store.** `ChannelStore::files_dir`, `ChannelPort::files_dir`; `principal::attachments` — `Limits`, `validate`, `check`, `ingest_into_channel`, `share_with_channel`, `locate`, `received_for_turn`, `crosses_runtimes`.
- **Transfer.** `tunnel/attachment_transfer.rs` (fetch, serve, wanted list, apply locks); the dispatcher's inbound apply step; outbound envelopes blank `path`; history catch-up fetches.
- **Turn.** `TunnelHost::human_dm_post` and `PrincipalManager::receive_channel_dm_post` take the post's attachments.
- **Clients.** `channel_peek` / `channel_events_watch` / `channel_history_older` / `principal_log` fill received paths. CLI: `peko send group:<slug> --attach`, `peko channel dm <peko> --attach`.
- **PekoHub.** `handleAttachmentForward` with `ATTACHMENT_*` caps and counters.
