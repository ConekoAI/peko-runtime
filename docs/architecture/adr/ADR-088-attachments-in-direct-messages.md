# ADR-088: Attachments in direct messages — sent by path, kept in the peko's workspace

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
| F2 | The model layer already sends images: `ContentBlock::Image`, through every adapter (F28). | `peko-message`, `providers/adapters` |
| F3 | The loop has a rich-input entry that takes a full user `LlmMessage`; the session keeps only its text. | `engine/agentic_loop.rs` `run_streaming_with_resume_rich` |
| F4 | A model's spec says whether it takes images; the spec gate refuses image blocks to a model that does not. | `providers::spec::ModelSpec::image_input`, `engine/spec_gate.rs` |
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

### D4 — The turn sees a note for every file, and images it can view

The turn's text is the message plus one line per attachment:

```
[attached: photo.png (image/png, 1.2 MB) at /…/attachments/2026-10-30/ab12cd34-photo.png]
```

Images (PNG, JPEG, GIF, WebP, up to 5 MB) also go to the model as image blocks — when the turn's model declares `image_input` (after any per-message model override). Otherwise the note says the current model cannot view images. Other files are for the peko's own tools to open. The session keeps the text, notes included, so later turns can reopen a file by its path.

### D5 — A busy peko gets the notes only

A message sent while the peko is working is queued as steering, which is text: it carries the notes, not image blocks.

## 3. Alternatives considered

- **Inline bytes in the packet.** Needs chunking over the datagram socket and a reassembly protocol; a path is enough on one host.
- **Store attachments in the channel directory.** The channel is shared storage the peko's file tools do not reach in untrusted runs; the workspace is the peko's own.
- **Send images to every model.** The spec gate would fail the turn for text-only models; a note keeps the turn working.
- **Groups now.** Group posts are written by several actors and mirrored across runtimes; DMs first.

## 4. Consequences

- You can hand a peko a screenshot, a PDF, or a spreadsheet; it sees images with a vision model and opens other files with its tools.
- Attachments count toward the workspace's size and are exported with it.
- Remote clients and groups cannot attach yet.

## 5. Implementation

1. **Wire.** `attachments` on both send requests; `Attachment` on `Posted` and on `PrincipalLogMessage`; DATA_MODEL.
2. **Ingress.** Validate (operator, count, size, regular file), copy into the workspace, post with attachments, write the notes.
3. **Turn.** `ExecutionConfig.attachments` → image blocks for a vision model → the loop's rich entry.
4. **CLI.** `peko send --attach <path>` (repeatable).
5. **Desktop.** Attach button and drag-and-drop; chips before sending; thumbnails and file chips in the thread.
