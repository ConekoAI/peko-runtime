# ADR-088: Attachments in direct messages — sent by path, kept in the peko's workspace, media to models that take them

**Status:** Accepted (runtime: `feat/dm-attachments`)
**Date:** 2026-10-30
**Counterpart:** peko-desktop — attach, drag-and-drop, and attachments shown in the chat.

**Preserves:** [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (file tools of non-owner runs stay inside the workspace — attachments live there), [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) (the DM channel is the conversation record).

---

## 1. Context

A message to a peko is text only. Verified on `master` @ `04c1abda`:

| # | Fact | Where |
|---|---|---|
| F1 | `principal_send` / `principal_send_stream` carry `message: String`. | `ipc/packet.rs` |
| F2 | The model layer already sends images: `ContentBlock::Image`, through every adapter (F28). Audio was reserved (`ModelSpec::audio_input`, a gate arm with no block); video had nothing. | `peko-message`, `providers/adapters`, `engine/spec_gate.rs` |
| F3 | The loop has a rich-input entry that takes a full user `LlmMessage`; the session keeps only its text. | `engine/agentic_loop.rs` `run_streaming_with_resume_rich` |
| F4 | A model's spec says whether it takes images; the spec gate refuses image blocks to a model that does not. | `providers::spec::ModelSpec::image_input`, `engine/spec_gate.rs` |
| F4b | Omni models take audio and video on the OpenAI chat-completions protocol: MiMo-V2.6 reads `input_audio` (OpenAI's raw base64 + `format`, or a data URL) and `video_url` (a data URL). Verified against `mimo-v2.6-flash` on 2026-10-30. | MiMo docs; manual check |
| F5 | IPC is a Unix datagram socket; a packet is bounded (SO_RCVBUF 256 KiB). An image does not fit inline. | `ipc/server.rs` |
| F6 | The desktop and the engine run on the same host; the desktop already talks to the engine as the local operator. | ADR-001 (desktop) |

## 2. Decision

### D1 — Attachments are sent by path, by the operator

`principal_send` and `principal_send_stream` gain `attachments: [path]` (absolute paths, default empty). The engine reads the files itself, so only the runtime's operator — the local user — may attach; any other caller is refused. Limits: at most 8 files, each at most 20 MB, regular files only. A request over a limit is refused before anything is posted.

Paths fit the packet limit (F5) and the shared host (F6). Remote callers (Hub, another runtime) have no upload path yet.

### D2 — The engine keeps a copy in the peko's workspace

Each file is copied to `<workspace>/attachments/<YYYY-MM-DD>/<8 hex>-<name>` (name reduced to a safe file name). The copy is the record: the original can move or disappear. The workspace is where the peko's file tools already reach — including non-owner runs (ADR-078) — and it travels with export.

### D3 — The DM post records them

The inbound DM post (`Posted`) gains `attachments: [{ name, path, mime, size }]`, `path` relative to the workspace, omitted when empty. `principal_log` pages and `principal_log_watch` rows carry them with an absolute `path`, so a client on the host can show them.

### D4 — The turn sees a note for every file, and the media its model takes

The turn's text is the message plus one line per attachment:

```
[attached: photo.png (image/png, 1.2 MB) at /…/attachments/2026-10-30/ab12cd34-photo.png]
```

Media also go to the model as content blocks — when the turn's model (after any per-message model override) declares that kind of input:

| Kind | Formats | Block limit | Spec flag | Chat Completions part |
|---|---|---|---|---|
| Image | PNG, JPEG, GIF, WebP | 5 MB | `image_input` | `image_url` (data URL) |
| Audio | WAV, MP3, FLAC, M4A, OGG | 20 MB | `audio_input` | `input_audio` `{data, format}` |
| Video | MP4, MOV, AVI, WMV, WebM | 20 MB | `video_input` (new) | `video_url` (data URL) |

`peko-message` gains `ContentBlock::Audio` and `ContentBlock::Video` (same source shape as images), `ModelSpec` gains `video_input`, and the spec gate refuses audio and video blocks to a model without the flag. For each kind the model does not take, the turn's text gains one line saying so (`[The current model cannot take audio or video; …]`). An API format with no such part (Anthropic Messages, OpenAI Responses) sends a text placeholder instead of the block. Other files are for the peko's own tools to open. The session keeps the text, notes included, so later turns can reopen a file by its path; media blocks are sent with their turn only.

### D6 — `Read` shows media to the model too (amended 2026-10-30)

An agent can look at an image, listen to audio, or watch video at any time, not only when it arrives attached to a message: `Read` on such a file returns a line describing it and a `media` reference (`{kind, mime, path}`) instead of base64 text. The engine attaches the file to that tool result as a media block when the run's model takes that kind (the D4 table and limits), else a note. Only the built-in `Read` may do this — its path already passed the run's file-access checks (ADR-078); another tool's output (an MCP server's) cannot point the engine at a file.

The session keeps the tool's text; the block rides the in-flight conversation for the rest of the run. Anthropic `tool_result` content carries images (audio and video become a placeholder); the OpenAI protocols take only text in tool outputs, so a turn's tool media goes in one `user` message right after its tool outputs.

### D7 — The peko sends files too: `Attach` (amended 2026-11-02)

Attachments were inbound only: a peko could make a chart or a report but could hand it over only as a path in its text. The `Attach` built-in takes `paths` (1–8) and an optional caption `text`. Each path passes the same floor as `Read` (ADR-078); the files are copied into `attachments/` under the D1 limits and D2 naming, and posted on the run's conversation channel — the stamp's compartment — as one message from the principal with the D3 attachment records. Clients read it like any post: `principal_log` rows carry absolute paths, and the desktop shows images (SVG included) inline, plays audio and video, and opens the rest.

A post is its own message beside the reply, not part of it: the reply projection stays text, and a file sent mid-run shows before the answer that refers to it. `Attach` is refused outside a conversation (cron and internal runs have no one to send to). ~~and in a conversation with any member on the Hub or another runtime, since a post carries paths on this host, not the bytes.~~ *Amended by [ADR-094](ADR-094-attachments-cross-runtimes.md):* in such a conversation the files also go into the channel's file store under their digest (10 MB a file, 25 MB a message), and the other runtimes pull them.

### D5 — A busy peko gets the notes only

A message sent while the peko is working is queued as steering, which is text: it carries the notes, not image blocks.

## 3. Alternatives considered

- **Inline bytes in the packet.** Needs chunking over the datagram socket and a reassembly protocol; a path is enough on one host.
- **Store attachments in the channel directory.** The channel is shared storage the peko's file tools do not reach in untrusted runs; the workspace is the peko's own.
- **Send media to every model.** The spec gate would fail the turn for models without the input; a note keeps the turn working.
- **Video as sampled frames.** Works with vision-only models but needs a decoder in the runtime; omni models take the file itself.
- **Groups now.** Group posts are written by several actors and mirrored across runtimes; DMs first.

## 4. Consequences

- You can hand a peko a screenshot, a voice memo, a clip, a PDF, or a spreadsheet; a model that takes images, audio, or video sees them directly, and the peko opens anything else with its tools.
- A model takes media only as its catalog spec says: `peko model add --spec '{"image_input":true,"audio_input":true,"video_input":true,…}'`.
- Attachments count toward the workspace's size and are exported with it.
- Remote clients and groups cannot attach yet. *(Runtimes can since [ADR-094](ADR-094-attachments-cross-runtimes.md); clients without a runtime still cannot.)*
- A peko sends files with `Attach` in local conversations (DMs and groups); files do not cross runtimes yet. *(They do since [ADR-094](ADR-094-attachments-cross-runtimes.md).)*

## 5. Implementation

1. **Wire.** `attachments` on both send requests; `Attachment` on `Posted` and on `PrincipalLogMessage`; DATA_MODEL.
2. **Ingress.** Validate (operator, count, size, regular file), copy into the workspace, post with attachments, write the notes.
3. **Turn.** `ExecutionConfig.media` → image / audio / video blocks for a model whose spec takes them → the loop's rich entry; adapters encode audio and video.
4. **CLI.** `peko send --attach <path>` (repeatable); `peko log` lists each message's attachments with the path of the peko's copy.
5. **Desktop.** Attach button and drag-and-drop; chips before sending; thumbnails and file chips in the thread.
6. **Tools (D6).** `Read` returns `media`; `peko_message::media::MediaKind` and `peko_engine::media` hold the shared kinds, limits, and loading; adapters carry media in tool results.
7. **Attach (D7).** `tools/builtin/channel/attach.rs`; the desktop renders the peko's attachment posts with the same chips and thumbnails.
