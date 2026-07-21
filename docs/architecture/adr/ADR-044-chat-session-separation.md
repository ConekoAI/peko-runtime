# ADR-044: Chat-Session Separation

**Status:** Accepted
**Date:** 2026-07-20
**Author:** rlsn
**Related:** [ADR-042](ADR-042-no-external-session-concept.md) (no external `session` concept in CLI/IPC surface), [ADR-041](ADR-041-principal-as-container.md) (principal-as-container and session blackboxing), [ADR-039](ADR-039-principal-model.md) (principal type unification).

**Note:** This is a clean-slate pre-production design. Backward compatibility with Peko 0.1.0 is intentionally discarded so the codebase and UX remain coherent.

## 1. Context

A Principal's consumer-visible conversation (`peko log`,
`principal_log` IPC, desktop chat page) was, until this ADR, a
projection of the principal's latest agent-session JSONL. The
handler resolved a `SessionArtifact`, read
`<principal>/memory/sessions/<session>.jsonl`, and converted
internal `SessionEvent`s into UI events.

That coupling forced the consumer-facing chat surface to carry
every internal noun the session JSONL used: prompts, tool calls,
thinking blocks, compactions, model changes, branches, provider
roles. The runtime's internal session evolution (F30a atomic
append, F31a max-iterations, F31x hook seams, F32b JSON-schema
tool-arg validation) all had to flow through the same projection,
and every new internal event kind became a candidate consumer-
visible row.

The projection was a poor substitute for a real conversation:
- The session JSONL is mutable working memory. It can be
  compacted, branched, replayed, and rewritten; those are useful
  properties for the agent loop and harmful properties for a
  consumer-visible record of what was actually said.
- The session JSONL is principal-owned. Treating it as the
  consumer-visible store made it impossible to record principal-
  to-principal exchanges where both sides need their own
  authoritative view of the same exchange.
- Sessions fail to model channel semantics. A `Cron` tick and a
  `user:alice` message end up in the same internal store even
  though only one is a conversation the principal had with a peer.

ADR-042 already established that there must be no `peko session`
command and that `peko log` is the only user-facing read surface.
This ADR closes the remaining leak: the projection that bound
that read surface to internal session events.

## 2. Decision

- **Two distinct stores**, by ownership and lifecycle:
  - **Session JSONL** (`<principal>/sessions/<id>.jsonl`) is the
    principal-owned, mutable, internal working memory used by the
    agent loop. Its schema is free to evolve with the runtime.
  - **Chat log** (`<data-dir>/chat_logs/<blake3(principal_did)>/<blake3(peer)>.jsonl`)
    is the runtime-owned, append-only, consumer-visible record of
    the text messages an external participant actually exchanged.
- **The chat log is the only source for `peko log`**,
  **`principal_log` IPC**, and the desktop chat page. Session
  JSONL is invisible to those surfaces.
- **Recording happens at the principal boundary**, not in the
  agent loop. The runtime appends the external input before
  dispatch (a persistence failure on this append rejects the
  dispatch) and appends the authoritative final response before
  returning it to the caller (best-effort; a persistence fault
  surfaces a tracing warn and the response still goes out).
- **Channel filter**: only peer-chat channels are logged —
  `Cli`, `Http`, `Hub`, `A2a`/`P2p`, `Webhook`. `Cron` and
  `FileWatch` are automation inputs, not conversations.
- **Both sides of a principal-to-principal exchange record their
  own view.** The recipient records through
  `PrincipalManager::receive` with `ChannelKind::A2a`; the caller
  records through `PrincipalSendTool` (`tunnel::principal_send_tool.rs`).
  Each view is a shard keyed on its owner's DID. Deleting one
  principal removes only its own view.
- **IPC shape**: `RequestPacket::PrincipalLog` retains `name`,
  `peer`, `limit`, `since_secs` and gains `cursor: Option<String>`.
  `ResponsePacket::PrincipalLog` carries `name`, `peer`,
  `messages: Vec<ChatLogMessage>`, `next_cursor`, `has_more`.
  `events: Vec<HistoryEvent>`, `session_id`, and `truncated` are
  gone.
- **Pre-launch clean cutover**: no migration from session JSONL to
  chat log, no legacy fallback, no tombstones, no retention. Chat
  logs are untrusted UI surface and may be deleted with their
  principal; transient persistence faults must not deny the
  principal exchange.

## 3. Rationale

The split keeps each store's lifecycle aligned with its purpose.
The agent loop owns mutable working memory because the loop
mutates — compactions, branches, and replay all change the
record. The consumer-facing chat surface owns an append-only
event stream because conversations are append-only from the
peer's perspective. A peer who sent a message did not have it
rewritten a week later; the runtime has no business doing so.

The split also lets the runtime evolve the session JSONL freely.
F30a's atomic append, F31a's max-iterations, F31x's hook seams,
and F32b's tool-arg validation can all change the session-event
schema without forcing a consumer-visible wire change. The
inverse was true before: every internal `SessionEvent` variant
became a candidate `HistoryEvent` variant, and the desktop had
to update its projection for each one.

## 4. Storage shape

### Chat-log shards

- One shard per `(principal_did, peer)` pair, at
  `<root>/<blake3(principal_did)>/<blake3(peer_subject)>.jsonl`.
  Full BLAKE3 hashes avoid unsafe filenames and make each shard
  self-validating.
- A private thread header (`{"kind":"thread","schema_version":1,
  "principal":...,"peer":...}`) is the first line of every shard
  so a mismatched/corrupt shard fails
  `ChatLogStore::read_page` with `ChatLogError::ThreadMismatch`.
- Appends follow the F30a durability pattern (`O_APPEND`,
  `write_all`, `fsync`, `sync_dir`) under the cross-process
  `FileLock`. Torn final lines are filtered on read, same shape
  as `SessionStorage`.
- Sender validation rejects any sender that isn't one of the two
  participants of the thread (`Subject::Principal(key.principal)`
  or `key.peer`).

### Cursors

- Opaque, versioned, thread-bound base64-url strings carrying the
  thread fingerprint (`blake3(<principal>\0<peer>)`) and the byte
  offset before the oldest returned line.
- A cursor issued for thread A is rejected if reused against
  thread B (`ChatLogError::Cursor`), and a cursor pointing past
  EOF or mid-line is rejected
  (`ChatLogError::InvalidOffset`).
- Cursors remain valid across subsequent appends because appends
  only grow the file monotonically.

## 5. Consequences

- `peko log` no longer exposes session internals. Tool calls,
  thinking blocks, compactions, and session markers are not
  chat-log entries and never appear in `peko log` output.
- `PrincipalLog::event_to_history` (`src/session_service.rs`)
  remains only where internal session-history services still
  need it; the principal-log IPC no longer routes through it.
- `HistoryEvent` and `load_principal_session_events` are removed
  from the principal-log surface. Internal callers of
  `HistoryEvent` are unaffected.
- The desktop chat page projects `ChatLogMessage` directly onto
  chat bubbles; the previous
  `historyEventsToChatItems` projection that hid
  session-internal `kind` rows is replaced with a flat map.
- The desktop `PrincipalLog` page removes rendering for tool
  calls, thinking, compactions, and session markers, and gains
  "Load older" paging via `nextCursor`.
- A failed chat-log append for the input side rejects the
  dispatch. A failed chat-log append for the response side is
  best-effort — the principal still receives its response — and
  surfaces as a tracing warning.
- Removing a principal deletes that principal's own chat-log
  shards only. The counterpart views held by other principals
  remain because they are owned by the other principal.
- There is no `peko session` command and there will never be one
  (ADR-042, restated for clarity).

## 6. Alternatives Considered

- **Keep the projection, just stop exposing session events.** The
  projection logic had grown complex and error-prone (sender
  identity mapping, role-conditional rendering, compaction
  suppression). Removing the projection entirely is a smaller
  surface area than curating it forever.
- **Store chat messages inside the session JSONL under a
  dedicated `kind: "chat"` variant.** Keeps a single store but
  re-couples internal session evolution to consumer-visible
  schema. Rejected: the whole point of the split is that the two
  schemas evolve independently.
- **Add a per-peer in-memory ring buffer in `PrincipalManager`.**
  Avoids disk I/O but loses durability across restarts and makes
  pagination over a long conversation impossible. Rejected:
  durability is a hard requirement for any user-visible record.
- **Defer the cross-runtime caller-view recording.** The
  caller-side `principal_send` shard is the caller's
  authoritative record of what they sent and what came back.
  Recording on outbound accept (not on response) preserves the
  consumer-visible truth that the message left the caller's
  runtime even if the target never replied. Hub rejections and
  decode failures deliberately do not produce phantom reply
  lines.

## 7. Verification

- **Unit tests** for the chat-log domain: message/header serde
  round-trips, stable DID/Subject thread hashing, concurrent
  appenders producing non-interleaved lines, torn final lines
  filtered on read, latest-first reverse paging with stable
  cursors and `since_secs` cutoff, sender-participant validation,
  principal-removal cleanup.
- **Tunnel tests** (`tunnel::principal_send_tool::tests`): three
  new tests pin the caller-view contract — successful round-trip
  produces one request + one response with matching
  `correlation_id`; decode failure produces only a request line;
  hub rejection produces only a request line.
- **Integration test** (`tests/cli_log.rs`): rewritten for the
  new wire envelope. Asserts the JSON carries `messages`,
  `nextCursor`, `hasMore`, never leaks `kind`/`toolName`/
  `sessionId`/etc., sender identity matches the peer/principal
  pair, and `--cursor` walks older messages without overlap or
  gaps and remains chronologically ordered across page
  boundaries.
- **Desktop tests** (`src/__tests__/historyProjection.test.ts`):
  replaced the session-event projection tests with direct
  chat-message mapping tests and two paging-reconciliation tests
  pinning dedupe by message id.
- **Pre-launch cleanup invariant**: `PrincipalManager::remove`
  resolves the principal's stable DID before evicting in-memory
  state, then calls `ChatLogStore::remove_principal` as part of
  the lifecycle. Cleanup failures surface as
  `PrincipalManagerError::Io` rather than silently retaining
  removed data.