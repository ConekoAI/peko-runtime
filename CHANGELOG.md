# Peko Changelog

All notable changes to Peko.

## [Unreleased]

### Capabilities as workspace files + per-turn prompt catalog (2026-08-30, ADR-050)

Removes the CLI/IPC sugar around workspace capabilities and makes the
system prompt's agents/skills sections live views of the workspace.

#### Removed
- **`peko principal agent|persona|tool|hook|skill|mcp`** — the entire
  per-category extension-management tree (list/install/remove/set/show)
  is gone, along with the `principal_workspace` CLI module. Capabilities
  are plain files in the principal workspace
  (`agents/`, `skills/`, `tools/`, `mcp/`, `hooks/`); manage them by
  editing files directly (e.g. add a skill: create
  `<workspace>/skills/<name>/SKILL.md`).
- **Persona drafting IPC** — `RequestPacket::PersonaDraft` /
  `ResponsePacket::PersonaDrafted`, the `ipc/handlers/persona.rs`
  handler, and the client method. The `persona` config field on
  principals stays.

#### Changed
- **Per-turn workspace capability catalog** — the `{{agents}}` and
  `{{skills}}` system-prompt sections moved from the cache-stable prefix
  to the per-turn volatile suffix
  (`peko-rs/engine/src/prompt/renderer.rs::render_per_turn`), rendered by
  two new scanning hook handlers (`WorkspaceAgentsPromptHandler`,
  `WorkspaceSkillsPromptHandler`) that scan the workspace `agents/` and
  `skills/` directories at prompt-build time with an mtime-keyed cache.
  Dropping a new `agents/foo.md` or `skills/foo/SKILL.md` into the
  workspace makes it visible in the system prompt on the next iteration
  — no restart, no re-registration.
- **Presence = visibility** — a capability is in the catalog iff its
  file is in the workspace; there is no capability/active-extension
  filter on the prompt sections. The catalog is name + description lines
  only (progressive disclosure: the model reads files with fs tools and
  invokes skills via the `Skill` tool). The skills catalog caps at 8 KB,
  then truncates with a pointer to list the directory.

### Channel-native CLI surface: `send` / `stop` / `log` (2026-08-27, ADR-048)

Reshapes the user-facing surface around one mental model — "I'm
messaging a principal on a channel; I don't care whether it's
mid-run" — and fixes a real run-registry collision bug underneath it.

#### Changed
- **`peko send <principal> [msg]`** always posts to the `(principal,
  peer)` thread. If a run is in flight, the message is queued onto the
  session inbox and folds into the running turn at the next agentic
  iteration; the daemon answers the streaming request with
  `Done { success: false, error: "[queued] …" }` and the CLI prints a
  busy notice to stderr and exits 0. New flags: `--wait` (block for
  the reply via the log-watch stream; 10-minute cap, poll fallback)
  and `--peer user:<id>` (overrides `-U/--user` for both the send and
  the Ctrl-C stop target).
- **`peko stop <principal> [--peer]`** replaces `peko interrupt`:
  deterministic soft-stop at the next agentic boundary with the
  structural subagent cascade, a peer-authored `⏹ stopped by user`
  marker on the thread, and a stop-context note in the session inbox
  that the *next* turn drains as cleanup context. Idempotent — no
  running turn prints a friendly notice and exits 0.
- **`peko log`**: new `--watch` (replay newer than `--cursor`, then
  live rows over the privacy-checked `principal_log_watch` stream with
  2s heartbeats; `--json` → NDJSON). `--limit` is now a hard cap on a
  single page; `--all` opts into the multi-page drain.
- **Group recipients** (`group:<slug>`): `peko log` reads the channel
  directly via `ChannelPeek` (client-side `--limit`/`--since`, 2s poll
  for `--watch`, `{at, author, text}` JSON rows). `peko send` and
  `peko stop` refuse groups with a pointer to `peko channel post` —
  the channel IPC authorizes writes against member principals and has
  no user-authored post path (known limitation, see ADR-048).
- **Ctrl-C in `peko send`** sends `PrincipalStop` for the thread
  instead of a request-id-addressed control packet.
- **Cancel-path transcript**: a user stop no longer posts
  `⚠ Run failed: Subagent was cancelled` to the thread; the send
  stream's error reads `stopped by user`.
- **Run registry rekeyed**: `streaming_runs` is keyed by the peer
  child session id instead of the client-minted `request_id` — every
  CLI process starts request ids at 1, so concurrent `peko send`
  processes used to overwrite each other's entry and
  `peko interrupt 1` cancelled the wrong run. Steering-successor runs
  (Gap-2 drain) are now registered under the same key and are
  stoppable.
- **E2E scripts**: `--no-stream` swept from all flows under
  `scripts/e2e/flows/` (the flag no longer exists).

#### Removed
- **`peko interrupt <request-id> [--steer]`** — replaced by
  `peko stop`; steering as a separate concept is gone (send always
  queues onto a busy thread).
- **`peko send --stream` / `--no-stream`** — streaming render is the
  only mode; the `request_id` stderr banner is gone (`peko stop` needs
  no id).
- **IPC `PrincipalSendControl` + `PrincipalSendControlMode`** —
  breaking protocol removal (pre-launch cutover; no known external
  consumer). Replaced by `PrincipalStop`.
- **`IngressMode::SteerOnly`** — its only caller was the deleted Steer
  arm.

#### Added (IPC)
- `PrincipalStop { name, peer }` → `Done { success, error }`.
- `PrincipalLogWatch { name, peer, since_cursor }` → stream of
  `PrincipalLogAppended { message }` + `Heartbeat` — the
  privacy-checked sibling of `ChannelEventsWatch` (which stays
  unchanged for peko-desktop).

#### Compatibility
- peko-desktop: the one-shot `PrincipalSend`/`PrincipalSent` shapes it
  depends on are untouched, including the legacy "Queued…" content on
  the one-shot busy path (only the streaming variant gained the
  `[queued]` signal).

#### Docs
- **ADR-048** (channel-native CLI surface) added.
- CLI_REFERENCE rewritten for `send` / `stop` / `log`; USERS_GUIDE
  privacy table now covers `stop` and `--watch`; README command list
  updated.

### Sprint 9 — Retire chat-gateway adapter; converge ingress paths (2026-08-22)

Removes the last holdout that bridged out-of-process children to the
legacy `StatelessAgentService` ingress path. Under the new
agent-session paradigm (Phase 7 of sprint 2), all external ingress
already lands in per-peer standing children via
`Principal::Manager::receive_streaming` — `peko send` IPC,
tunnel/A2A, and bound channels were on this path; the chat-gateway
adapter was the only remaining legacy producer.

#### Removed
- **Chat-gateway adapter framework** (`peko-rs/core/src/extensions/gateway/`,
  ~2000 LOC + archived HTTP-basics fixture). The framework never shipped
  a concrete integration; it only bridged to `StatelessAgentService`.
- **`StatelessAgentService`** + the `PrincipalMessageService` trait
  port (~1568 LOC). Their only production caller was the deleted
  gateway router.
- **Legacy IO hook points**: `HookPoint::ChannelInput`,
  `HookPoint::ChannelOutput`, `HookPoint::MessagePreSend`,
  `HookPoint::MessagePostReceive` — all four had exactly one production
  caller each, both now deleted.
- **Platform `ChannelType` variants**: `Discord`, `Telegram`,
  `WhatsApp`, `Slack`, `Signal`, `Matrix`. Pure dead arms — no
  production code ever constructed them, no production JSONL
  contained them, no sibling repo depended on them.
- **`AgentConfig::agent_did` field**: last reader was
  `StatelessAgentService::load_config_fresh`. Old TOML files with
  `agent_did = "..."` silently drop the key on deserialization
  (forward-only migration).
- **`discord_session_key` helper** + the chat-platform match arm in
  `scope_from_key`. Per-channel keys now go through the generic
  `derive_session_key` + `SessionKeyContext { channel: "cli", .. }`
  path.
- **`GatewayAdapter` + `GatewayRuntimeStarter` registrations** in
  daemon, extension management service, and `peko ext init gateway`
  scaffold.

#### Changed
- `peko ext install` no longer accepts `extension_type: "gateway"`
  manifests; `is_valid_type` returns `false`.
- `BuiltInAdapters::adapters()` count dropped from 7 to 6.
- `StarterContext` no longer carries `principal_service` or
  `gateway_router`; `ExtensionServices` no longer carries a
  `principal_message_service` slot.
- `ChannelType::supports_threads` now returns `false` (no active
  thread-capable channel remains).

#### Docs
- **ADR-025 marked Superseded** with pointer to the Agent Session
  Paradigm doc. Sprint 9 closes the chat-gateway architecture
  described there.
- **README** replaces Discord/Slack references with the current
  ingress surface (CLI + tunnel + bound channels).

### PEKO sprint 3: DM-over-channel unification (2026-08-18)

Re-founds all peer DM communication on the channel primitive: a peer
DM is a 1:1 channel with a passive binding to the peer's standing
child; group channels stay active (cron-read). Phase by phase.

#### Phase 10 — DM channel auto-provisioning + push-wake

##### Added
- **Peer DM channel auto-provisioning** (`principal/peer_dm.rs`):
  every external ingress path (`peko send` IPC, tunnel A2A
  `receive`/`receive_streaming`, Hub webchat, IPC Steer) funnels
  through `PeerChildTurns::ensure_child`, which now also
  find-or-creates the peer's DM channel — `dm-<peer_child_slug>` with
  `passive_binding = "/<slug>"` and the principal as creator/member.
  Find matches on the *binding* (semantic identity), not the display
  name; find-or-create is serialized per principal by the manager's
  `session_creation_lock` (concurrent first-contacts converge on one
  channel). The port is threaded explicitly
  `AppState → PrincipalManager::with_channel_port → PeerChildTurns`;
  `None` (standalone/test) skips provisioning with session behavior
  unchanged. Remote (`principal:<did>`) peers get the LOCAL channel
  only — cross-runtime invite/`join_remote` fan-out is Phase 12.
- **Live subscriber on create**: a freshly provisioned DM channel
  fires the `dm_subscriber_hook` the daemon installs
  post-supervisor-build (`PrincipalManager::set_dm_subscriber_hook` →
  `ChannelBindingSupervisor::ensure_subscriber`), so it gets its
  `PassiveBindingResponder` subscriber without a daemon restart.
- **Push-woken bound channels**: `ChannelStore::append_event` is now
  the single disk-append chokepoint and fires the per-channel
  broadcast on EVERY durable append (local posts, membership events,
  and cross-runtime mirror appends; `TunnelChannelPort` delegates to
  the same store). `ChannelSubscriber::spawn` `select!`s on that
  broadcast plus a backstop tick (`SubscriptionConfig` default raised
  5s → 30s; a `Closed` broadcast degrades to pure ticking). No
  `ChannelPort` trait signature changed (`subscribe_events` already
  existed with a no-op default).

#### Phase 11 — local ingress re-routed onto DM channels

`peko send` (IPC), Hub webchat, and IPC steer now write both
directions of the conversation to the peer's DM channel; the chat-log
projections on these paths are deleted.

##### Added
- **Attributed channel posts**: `ChannelPort::post_attributed`
  (default degrades to `post`) with `ChannelStore` +
  `TunnelChannelPort` overrides — membership/permission checked
  against the `sender` principal, `author` written verbatim. Ingress
  convention: inbound posts use `sender = principal.id`,
  `author = peer.to_string()` (the Subject wire form: `user:alice`,
  `principal:<did>`); replies keep plain `post()` (author = the
  principal's `prin_<uuid>`).
- **`PeerChildTurns::ensure_child_ingress`** returns
  `PeerChildIngress { child_id, dm_channel }`; `peer_dm` gained
  `find_peer_dm_channel` (find-only by binding) +
  `post_peer_dm_inbound` / `post_peer_dm_reply` helpers.

##### Changed (breaking)
- **Ingress posts to the DM channel, drives the turn itself.** The
  inbound message is posted before the run permit is acquired (a post
  failure rejects dispatch — the persist-before-dispatch invariant);
  the assistant reply, run-failure traces, and steering-successor
  replies are posted back as the principal. Turn ownership is
  partitioned by author: `PassiveBindingResponder::response_trigger`
  now skips posts whose author parses as a Subject wire form
  (ingress-handler-owned), and drives only raw-principal-id posts
  from OTHER principals (local cross-principal `ChannelSend`; Phase
  12 A2A fan-out). No double-turns.
- **`peko log` reads the DM channel**, not the chat log: same
  `PrincipalLog` packet + `ChatLogMessage` rows, backed by
  `find_peer_dm_channel` + `peek_with_ids` with line-number cursors.
- **Behavior changes:** manager-path slash responses and "Queued…"
  notices are no longer persisted anywhere (transport UX, not
  conversation); pre-Phase-11 chat-log peer history stays on disk but
  is no longer read (Phase 13 owns the crate's retirement);
  port-less standalone/test contexts skip DM posts.
- **Dead surface deleted** (prelaunch): `record_input` /
  `record_response` / `record_chat_input` / `record_chat_response` /
  `is_peer_chat_channel` on `PrincipalManager`,
  `PrincipalHost::chat_log_store`,
  `PeerChildTurns::ensure_child` /
  `PrincipalManager::ensure_peer_child_session` (all call sites use
  the `_ingress` forms). `record_cron_input` stays — cron projection
  is unchanged and keeps `peko-chat-log` alive until Phase 13.

#### Phase 12a — cross-runtime DM channel plumbing + anti-loop rule

Additive only: the new machinery exists and is tested, but nothing
user-facing switches over yet (12b rewires `send_peer`).

##### Added
- **DM-aware channel invites**: `TunnelChannelInvite` gains
  `creator_did` + `passive_binding` (both join the signed pre-image).
  The binding value is only a "this is a DM channel" marker — each
  side's binding names its OWN child for the other principal, and
  slug-collision suffixes are runtime-local, so the receiver derives
  its own `/​<slug>` binding from its own session tree.
- **`ChannelStore::join_remote` re-partition**: member rows arrive
  keyed to the SOURCE runtime's view; the mirror maps the invitee row
  to the receiver's LOCAL `PrincipalId` (so the receiver can post and
  the mirror shows up in `list_for_principal`) and files the creator
  + runtime-stamped rows as `remote_members` (so the receiver's posts
  fan back out). `passive_binding` is persisted on the mirror.
- **Fan-out actually wired**: `TunnelChannelPort::fanout_dm_invite`
  records the invitee as a `RemoteMember` (unwiring the previously
  dead `add_remote_member` — before this, `remote_members` stayed
  empty and `fanout_event` no-oped) before the tunnel-handle check,
  so routing state survives a transient disconnect.
- **Inbound mirror bootstrap**: new `TunnelHost::
  dm_channel_mirror_bootstrap` (AppState impl) — resolves the invited
  local principal by DID, ensures its peer child for
  `principal:<creator_did>` (child-only; no second local channel),
  derives the binding from the child's real slug, `join_remote`s with
  it, and ensures the `ChannelBindingSupervisor` subscriber — closing
  the Phase 10 gap where mirrored channels got no responder until
  next boot.
- **Anti-loop rule**: responder replies are posted with
  `PostMsg::reply(event_id, …)` (`RespondCtx` now carries the
  triggering event's line id) and `response_trigger` only fires on
  ROOT posts (`parent.is_none()`). Cross-runtime ping-pong is
  structurally impossible: no responder ever reacts to a reply.
  Trade-off: threaded human replies (`PostMsg::reply` via
  `ChannelSend`) no longer wake a bound session — root posts only.

#### Phase 12b — `send_peer` + messenger over channels; A2A RPC retired

Principal-to-principal DM now runs over the mirrored DM channel; the
bespoke signed-envelope RPC stack is deleted.

##### Changed (breaking)
- **`send_peer` principal branch is channel-based.** Remote targets:
  ensure the caller's DM channel for the peer (first contact fires
  `fanout_dm_invite` with the caller's real DID), post a root message
  (author = caller's raw id — self-skipped locally, fires the remote
  responder), await the peer's reply on the channel broadcast with
  the 1-minute `response_timeout` (reply = first parent-bearing post
  after the send line not authored by the caller; per-target mutex
  serializes overlapping awaits). Local targets use a two-channel
  design: the exchange runs on the TARGET's DM channel (exact
  `author == target` matching) and is mirrored onto the caller's own
  DM channel for `peko log` (outbound as a self-authored root; the
  reply via `post_attributed` with `parent` set so the caller's
  responder skips it).
- **`send_peer` `session_id` arg dropped** — channel continuity
  replaces it (the child session is implied by the peer pair);
  `PrincipalSendResult.session_id` now returns the caller's
  standing-child id.
- **Peer notes (`send_peer` user branch, cron `Notify`/`Send`
  outcomes) post to the DM channel** instead of projecting a chat-log
  row. `deliver_note` keeps the find-only child-JSONL append (the
  agent's working memory of what was said on its behalf — notes have
  no turn) and the trunk `[notify]` self-view. After this the ONLY
  chat-log writer left is the cron `Send` input projection
  (`record_cron_input`).
- **Per-target `Permission::Chat` check dropped** with the retired
  RPC path — directory resolution + exposure gates are the boundary
  now. Flagged for a product decision.
- **Accepted gap:** cross-runtime channels are push-only — a post
  fanned out while the peer runtime is offline is not re-delivered on
  reconnect (the caller's copy is durable; the await times out with a
  structured error). Replay-on-reconnect is a follow-up.

##### Removed (prelaunch, no migration)
- `TunnelMessage::PrincipalToPrincipal{Request,Response}` envelopes,
  their dispatcher handlers, and the pending-response machinery
  (`tunnel/a2a_pending.rs`, `tunnel/a2a_audit.rs`; from
  `a2a_signature.rs` only the request/response signing — the
  pre-image helpers stay for channel signatures and invite tokens).
- The whole `tunnel/direct/` transport stack (client, server,
  manager, routing, handshake — it existed only for the
  principal-send RPC); `tls.rs` relocated to `tunnel/tls.rs` (still
  used by the tunnel client). `DirectHealth`, the direct-server
  startup, and `network.direct` runtime wiring removed from AppState
  (the hub-directory metadata fields stay).
- `PrincipalManager::receive` (its only production callers were the
  two retired paths); test callers migrated to `receive_streaming` /
  `receive_trunk`.
- `CrossRuntimeA2aCtx` trimmed to `{ directory, caller_runtime_id,
  principal_manager, channel_port (concrete TunnelChannelPort),
  response_timeout }`.
- Integration tests `direct_connection.rs` +
  `direct_transport_policy.rs` (with their `[[test]]` entries).

#### Phase 13 — `peko-chat-log` retired (2026-08-19)

With DM channels as the durable record of every peer conversation
(Phases 10–12b), the chat-log crate had one writer left (the cron
`Send` fired-prompt projection) and no readers. It is removed from
the workspace (22 → 21 members).

##### Changed
- **`peko log` row type moved + renamed**: `ChatLogMessage` →
  `peko_core::ipc::packet::PrincipalLogMessage` (+
  `PRINCIPAL_LOG_SCHEMA_VERSION`), serde wire shape byte-identical.
- **Cron `Send` no longer projects the fired prompt** to a
  consumer-visible log — it lives in the trunk session JSONL and the
  outcome note lands on the owner's DM channel (Phase 12b).
  `PrincipalManager::{record_cron_input, with_chat_log_store,
  chat_log_store}`, the AppState store construction, and
  `PathResolver::chat_logs_dir` are gone.
- On-disk `chat_logs/` shards from before this sprint are orphaned
  in place (prelaunch; delete them by hand if you care).

##### Removed
- The `peko-chat-log` crate (`ChatLogStore`, `ChatThreadKey`,
  `ChatLogPage`), its workspace membership, the core + CLI dep
  edges, and 17 forbidden-edge rules in
  `check_workspace_deps.py`.

With this phase the reviewer's finding is fully closed: channels are
the external-I/O primitive for BOTH directions of every peer
conversation — a peer DM is a 1:1 passive-bound channel, group
channels stay active (cron-read via `ChannelRead`, post via
`ChannelSend`), and no parallel per-peer record exists.

### Channel tool reachability fixes (reviewer findings, 2026-08-18)

The `ChannelRead` / `ChannelSend` built-in tools were wired in but
unreachable and inert in production; two layered bugs, both fixed.

#### Fixed
- **Missing starter grants** (same shape as the F351 session-tool bug,
  PR #351): `Capabilities::starter_bundle()` now grants
  `tool:ChannelRead` / `tool:ChannelSend`. Without them the capability
  filter (`is_tool_enabled`) dropped the tools from every
  default-created principal's toolset even though they were registered
  globally by `ToolRuntime::register_builtins`. Pinned by
  `starter_bundle_includes_channel_tools`.
- **`NoopChannelPort` clobber**: the daemon registered the real
  file-backed channel port at startup, but the first
  `PrincipalContext::core()` call re-registered the channel tools with
  a `NoopChannelPort`, and `BuiltinToolAdapter::register_tool`
  unconditionally overwrites the name-keyed instance side-table — so
  the real adapter was replaced and the tools were inert in
  production. The real port is now installed process-wide via
  `peko_channel::set_global_channel_port` (new global registry in
  `peko-channel`, mirroring the `CronRuntime`/`PeerMessenger` port
  pattern); `PrincipalContext::core()` resolves it via
  `peko_channel::global_channel_port()` with `NoopChannelPort` only as
  the test/standalone fallback.

### PEKO sprint 4: `send_peer` consolidated into `ChannelSend` (2026-08-19)

Sprint 4 closes the loop on the sprint 3 design promise: `send_peer` and
`ChannelSend` now share one delivery mechanism — there is one tool
(`ChannelSend`) whose `channel` parameter's wire form selects the
dispatch branch. The LLM picks the prefix; the runtime owns the
await / timeout / mirror / exposure-gate / originating-user-gate
policies.

#### Added
- **Typed `ChannelId` prefixes** (`peko-protocol`): `chan_<8 base36>`
  (Bare), `principal:<did>` (Principal), `user:<id>` (User),
  `group:<slug>` (Group). Bare form unchanged for backward compat;
  typed forms carry the routing identity on the wire (no separate
  indirection table for `peko log`).
- **`CreateOpts::id`** (`peko-channel`): callers can pin the channel id
  on creation. Used by `peer_dm` for principal peers so the routing
  ChannelId is `principal:<did>` and `peko log` consumers and
  `ChannelSend`'s principal-branch agree without translation.
- **On-disk path normalizer** (`peko-channel/fs.rs`): typed-prefix ids
  gain colons that are invalid in directory names on Windows /
  classic Unix filesystems; `channel_dir_name` replaces `:` with
  `.3A.` for the storage path only. Wire form unchanged.

#### Changed (breaking)
- **`ChannelSend` is now per-agent** with the caller's principal DID
  bound at construction (mirrors pre-PR `send_peer` shape). The F37
  funnel's `ToolContext` does NOT carry the caller DID, so the tool
  needs it bound at registration. Bare / group / user branches work
  in local-only mode (no cross-runtime ctx); principal branch returns
  a structured error. Global registration in `engine/tool_runtime.rs`
  is removed; `ExtensionServices` gains `set_channel_port` /
  `channel_port()` so the per-agent constructor can find the file-backed
  `ChannelPort`.
- **`peko-rs/core/src/tunnel/principal_send_tool.rs` deleted** (1775
  lines). The consolidated `ChannelSend` absorbs the executor code;
  `SendPeerArgs` / `PrincipalSendResult` are renamed to
  `ChannelSendArgs` / `ChannelSendResult`. The new dispatch on
  `ChannelId::kind()` selects Bare / Group (bare post), Principal
  (`execute_local` / `execute_remote`, await reply up to 1 minute,
  mirror), or User (peer messenger note, originating-user gate).
- **`peer_dm` routing id**: principal peers now route on
  `ChannelId::for_principal(did)`; display name stays `dm-<slug>` for
  `peko log` continuity. User / public peers keep the slug-based
  routing id (the user-branch dispatch is messenger-port).

#### Removed (prelaunch, no migration)
- **`tool:send_peer` capability**: retired outright, no compatibility
  alias. Principals with the legacy grant lose it post-cutover.
  `Capabilities::starter_bundle()` no longer includes the grant;
  `starter_bundle_does_not_grant_send_peer` pins the absence. Use
  `tool:ChannelSend` with a typed channel id (`chan_*` / `principal:<did>`
  / `user:<id>` / `group:<slug>`) to dispatch.

### PEKO sprint 5: slug-path addressing on the LLM-facing surface (2026-08-20)

Collapses the LLM-facing session-addressing surface to slug paths.
Three commits land in order: caller-relative slug resolution +
raw-id rejection (commit 1, `96d3e55a`), `name` required at Agent
spawn + tool-surface grammar update (commit 2, `89ca46d9`), and the
doc sweep (this commit).

#### Added

- **`peko_session::path::resolve_reference`** — single LLM-facing
  resolver. Three forms accepted: `/a/b/c` (absolute slug path,
  caller-anchored), `agent-c` (caller-relative slug, BFS by depth),
  and raw session ids **refused** with a structured error pointing
  the model at the `path` field in `session list` output.
- **`peko_session::path::resolve_id_or_path`** — engine-internal
  resolver. Same dispatch, but raw ids pass through verbatim. Used
  by `resume_preflight`, `request_compaction`, and
  `validate_context_parent` in `peko-rs/core/src/agents/subagent_executor.rs`
  for ids the runtime itself produces.
- **`peko_session::path::resolve_relative`** — BFS-descent resolver
  for the caller-relative branch. Direct children first (slugs are
  unique-per-parent so this is unambiguous at depth 0), then
  breadth-first descent. Multiple matches at the same depth → a
  structured error listing all candidate paths.
- **`peko_session::path::looks_like_session_id`** — shape heuristic
  for the raw-id refusal branch. `true` when the reference contains
  `:` (tree-root shape `root:<dim>:<name>`, runtime-extension
  prefixes `spawn:<uuid>:` / `channel:<id>:`) or when the value is a
  32+ char all-hex/dash blob (UUID-shaped).
- **`validate_slug` rejects `:`** — extends the existing slug
  validator to refuse `:` so the LLM-facing grammar is unambiguous
  by construction (slugs cannot look like raw ids). Confirmed safe:
  `peer_children.rs:84-92` already strips `:` from DID fragments
  via `c.is_ascii_alphanumeric()`, so standing-child slugs do not
  contain `:`.

#### Changed (breaking)

- **Raw session ids are REFUSED** at the LLM-facing tool layer
  (`Agent` `session_key`, all `session` tool `session_key` /
  `new_parent` / `target` parameters). Every "pass a session id"
  error now cites the slug-path grammar (`/a/b/c` or `agent-c`)
  instead. Engine-internal call sites are unaffected — they keep
  using raw ids via `resolve_id_or_path`.
- **`Agent` `name` (slug) is REQUIRED** at `action = "new"`.
  `validate_slug` runs early so the model sees an actionable error
  before any state touches the runtime. The four worked examples in
  the Agent tool description now include `"name": "writer-1"` (and
  similar) so the model copies the right shape.
- **Channel binding resolver (`peko-rs/core/src/daemon/channel_binding.rs:339-387`)**
  keeps its raw-id passthrough on purpose. Bindings are
  config-authored, not LLM-authored; the resolver sits closer to the
  `resolve_id_or_path` surface than the `resolve_reference` one.
  Doc comment now states the deliberate divergence so the next
  reader doesn't "fix" it.

#### Notes

- `peko_session::path` gained 280 tests (one new
  `resolve_id_or_path_accepts_raw_ids_and_resolves_paths` covers
  the engine-internal contract; `resolve_reference_rejects_raw_ids`
  uses `root:cron:alice` instead of `root:user:alice` so the
  self-reference shortcut doesn't fire).
- `peko-rs/core/src/session/session_runtime_impl.rs` gained
  `relative_slug_resolves_end_to_end`,
  `relative_slug_ambiguous_lists_all_paths`, and
  `raw_id_refused_with_actionable_message`.
- Full DTO pruning — `SessionInfo` dropping `session_key` /
  `session_id`, `ListScope` + `SessionRuntime::list_sessions`
  signature change, `SessionCache` path-resolution seam, spawn
  response replacing `child_session_key` with `path` + `slug` — is
  deferred to a follow-up commit. The user-facing behavior change
  (raw-id refusal, `name` required) is in commits 1 and 2.
- The bound-channel binding (`passive_binding`) is preserved as a
  `/`-rooted path through `SessionStoreBindingResolver`; the channel
  binding tests (`peko-rs/core/src/daemon/channel_binding.rs:1483-1541`)
  confirm the resolver still maps `/user-a` → canonical child id.

### PEKO sprint 6: opaque UUID session ids, peer is a channel concern (2026-08-20)

Collapses the engine-internal session id to an opaque UUID and
moves peer identity out of the session id entirely. The session
layer gives up one job — peer routing — and takes back one: a clean
storage key. Three commits land in order: `SessionId` newtype +
`find_trunk_session` + `resolve_peer_via_parent_walk` (commit 1,
`6bcfa9fa`), path resolver heuristic collapse (commit 2,
`2294f13e`), and the doc sweep (this commit).

Three layers, three shapes:

| Layer | Id shape | Job |
|---|---|---|
| LLM surface (Agent + session tools) | slug path (`/a/b/c`) | Address by human-meaningful name |
| Engine-internal session | opaque UUID | Storage key + parent-chain walk |
| Peer / routing | channel id (`principal:<did>`, `user:<id>`, `chan_<8>`, `group:<slug>`) | Conversation surface + cross-runtime |

#### Added

- **`peko_session::SessionId`** — newtype around `Uuid`,
  `#[serde(transparent)]`, `Copy`. `SessionMetadata.session_id` and
  `parent_session_id: Option<SessionId>` carry `SessionId` end to
  end. `SessionId::from(s)` falls back to a v5 UUID for non-UUID
  inputs so fixture literals and CLI logs round-trip without
  breakage. `SessionId::new()` mints v4 for new sessions.
- **`peko_session::ownership::find_trunk_session(metas)`** — returns
  `Option<SessionId>` of the session with `parent_session_id = None`.
  Replaces the `trunk_session_id()` magic-string helper at the 13
  production call sites (engine-managed guards, `ensure_peer_child`,
  `ensure_declared_children`, `receive_trunk`, `peer_children`,
  `child_turns`, `messenger`, `cron_engine::run_send_job`,
  `routers::root::root_session_id_for_channel`). Anchor for the
  trunk-short-circuit arms and the engine-managed guard compare.
- **`peko_session::ownership::resolve_peer_via_parent_walk(metas, session_id)`**
  — walks the `parent_session_id` chain reading stamped `peer_type` /
  `peer_id` on each ancestor; the first session with both fields
  set returns `Subject::from_str(&format!("{peer_type}:{peer_id}"))`.
  The walk terminates at the trunk (which has `parent_session_id =
  None` and no peer stamped) — returns `None`. The placeholder
  subjects (`Principal("standing_<slug>")` and
  `Principal("spawn_<uuid>")`) are skipped so the resolver keeps
  walking past standing / spawned intermediates. Replaces
  `peer_from_session_key` at both production call sites in
  `principal::messenger::originating_peer`.
- **`peko_session::path::resolve_reference` (collapsed)** — three-form
  grammar (sprint 5) → two-form grammar. `/`-prefixed paths
  resolve via `resolve_path`; the caller's own UUID is accepted via
  the self-reference shortcut. Every other shape is REFUSED with a
  structured error pointing the model at the `path` field in
  `session list` output. The caller-relative slug arm (`agent-c`) is
  retired; absolute paths are unambiguous and the tool surface
  already emits them.

#### Changed (breaking)

- **`SessionMetadata.session_id` and `parent_session_id` are
  `SessionId`**, not `String`. Production sites that need the
  string form call `.as_str()` (UUIDs are filesystem-safe on POSIX
  and Windows, so JSONL filenames and IPC payload keys are
  unchanged). Documentation and tests updated to use UUID fixtures
  with parent-chain metadata; the `root:self` / `root:cron:owner` /
  `root:user:alice` literal shapes are gone.
- **`SubagentMetadata.child_session_id` is `SessionId`** (was
  `child_session_key: String`). `SessionInfo` / `SessionStatusResult`
  / `BranchOutcome` / `DeleteOutcome` / `CompactRequestOutcome`
  remain stringly typed for now (DTO pruning is deferred — see
  sprint 5 "Notes"). The fields that did migrate are
  `serde(transparent)` so the wire shape is still a string.
- **`SessionStoreBindingResolver` (channel binding) anchors on
  trunk UUID lookup** — `find_trunk_session` over the
  `principal.toml` `[children]` materialization, not a hardcoded
  `"root:self"` literal. The raw-id passthrough is now a
  `SessionId::as_str()` pass-through under the hood; the deliberate
  divergence from the LLM-facing `resolve_reference` is documented
  in `daemon/channel_binding.rs`.

#### Removed (prelaunch, no migration)

- **`trunk_session_id()`** at `peko-rs/core/src/principal/routers/root.rs:67-69`
  — replaced by `find_trunk_session(metas)`. The literal
  `"root:self"` magic string is gone.
- **`peer_from_session_key`** at `peko-rs/core/src/principal/messenger.rs:57-78`
  — replaced by `resolve_peer_via_parent_walk`.
- **`parse_session_key_v2`**, **`parse_session_key` (v1)**,
  **`base_key_from_overlay`** at `peko-rs/session/src/key.rs` —
  the `agent:{a}:peer:{type}:{id}[:subagent:{uuid}][:overlay:{type}:{id}]`
  shape is gone. Peer identity comes from stamped metadata; agent
  identity from `meta.agent_name`; subagent / overlay identity from
  `parent_session_id` linking in metadata.
- **`peko_session::path::looks_like_session_id`** — engine-internal
  ids are opaque UUIDs, so the `:`-branch is dead and the 32+ hex
  arm matches every input. The LLM-facing refusal now keys on the
  strict two-form grammar instead.
- **`peko_session::path::resolve_id_or_path`** — engine-internal
  callers in `agents::subagent_executor.rs` (`resume_preflight`,
  `request_compaction`, `validate_context_parent`) now canonicalize
  via `SessionId::from` directly. With all engine ids being UUIDs
  the split between LLM-facing and engine-internal resolvers was
  redundant; the engine-internal sites were handing around shapes
  the LLM-facing resolver already handles via the self-reference
  shortcut.
- **`peko_session::path::resolve_relative`** — the BFS-descent
  caller-relative slug resolver; retired alongside the
  caller-relative slug arm.

#### Notes

- `peko_session::path` lost `looks_like_session_id_heuristic` and
  `resolve_id_or_path_accepts_raw_ids_and_resolves_paths` (sprint 5
  tests for the engine-internal surface); gained
  `resolve_reference_accepts_engine_uuid_via_self_reference` to
  cover the canonical engine-internal usage path.
- `peko-rs/core/src/session/session_runtime_impl.rs` lost
  `deep_tree_harness`, `relative_slug_resolves_end_to_end`, and
  `relative_slug_ambiguous_lists_all_paths` (sprint 5 callers of
  the now-retired `resolve_relative`); updated
  `raw_id_refused_with_actionable_message` to assert the new
  message wording ("raw session ids are not accepted").
- `peko-rs/core/src/agents/tests/subagent_integration_tests.rs`
  lost `resume_and_compact_accept_path_targets` (66 lines) — the
  engine-internal API no longer accepts path targets.
- `find_trunk_session`'s `Option` return is `.expect(...)` at sites
  that load the principal (trunk guaranteed to exist post-`ensure_trunk`-boot)
  and `.unwrap_or_default()` at sites that handle the no-trunk case.
- `live_root_id` integration test helper in
  `peko-rs/core/tests/cli_session_manage.rs:121-128` was rewritten
  to walk `sessions.json` looking for the entry with
  `parent_session_id = None`. Single helper, ~10 lines.
- Sprint 6 stays prelaunch — no on-disk sessions exist, so no
  migration. The sprint 5 doc sweep's "Deferred to follow-ups" list
  shrinks by three items (`resolve_id_or_path`, `resolve_relative`,
  `looks_like_session_id`); the DTO pruning items remain.
- `safe_filename_component` is a thin no-op for UUIDs (kept as a
  Windows-compat shim). UUIDs are filesystem-safe on POSIX and
  Windows; the function is a no-op for the canonical path and
  defensively safe for any non-UUID callers.

### PEKO sprint 2: external ingress off the root (2026-08-17)

External traffic (CLI `peko send`, tunnel A2A, Hub webchat) moves off the
root agent into per-peer standing children of the trunk (`/local-user`,
`/user-x`, `/principal-{did}`); the trunk `root:self` is cron-only.
Per-peer `root:{peer}` / `root:cron:{owner}` sessions are retired
(prelaunch breaking change — no migration).

#### Changed (breaking)
- **External ingress lands in per-peer children** (Phase 7). `peko send`,
  tunnel A2A, and Hub webchat spawn-or-continue the peer's standing child
  (`/local-user` for the owner — privileged, whole-store; `/user-x`;
  `/principal-{did}`) and stream the turn from there via
  `SubagentExecutor::resume_streaming`. Per-peer root sessions
  (`root:{peer}`, `root:cron:{owner}`) and their routing are deleted;
  `RootRouter` is trunk-only; `root_session_id` is gone. The root-family
  guards (delete/archive/move refusal, prune exemption) now protect
  exactly `root:self`.
- **Cron `Send` defaults to the trunk** (`target: "trunk"` is accepted
  but redundant); the 60s `Every` floor covers all trunk sends. Send
  outcome notes land in the owner's child; `[notify]` self-view lines
  land in the trunk; `SpawnTool` wakes steer the trunk inbox (Phase 3b).
  `deliver_note` appends to the peer's child (find-only, no create).
- `peer_from_session_key` no longer parses `root:`/`root:cron:` keys;
  `originating_peer` resolves via stamped `peer_type`/`peer_id` + the
  parent walk.

#### Added
- **Peer-child provisioning** (`principal/peer_children.rs`):
  `peer_child_slug` (peer → slug: `user:local` → `local-user`,
  `user:{id}` → `user-{id}`, `principal:{did}` → `principal-{fragment}`)
  and `ensure_peer_child` — find-or-create the peer's standing child
  (parent = trunk, `trigger="spawn"`, real peer stamped, idempotent,
  slug-collision suffixing).
- **`privileged` session flag**: the owner's child (`/local-user`) gets
  whole-store reach in the ownership guards (like the root agent had);
  strangers' children stay subtree-scoped. Privilege affects guard reach
  only — the session keeps its parent and stays in the trunk's tree.
- **Streaming child-turn driver** (`principal/child_turns.rs` +
  `SubagentExecutor::resume_streaming`): drives a turn in a peer child
  session with the full resume guard stack, live `AgenticEvent`
  streaming (same event shape as the root path — the IPC packet mapping
  is unchanged), cancellation, and registry registration (run-active
  guards see it). One shared builder (`PeerChildTurns::build`) now
  constructs both this driver and the channel-binding driver.
- **Persona inheritance**: peer children run the principal's root agent
  prompt (workspace `agents/root.md` → compiled-in default), fixing the
  blank-prompt fallback in daemon-driven child turns.
- **`record_peer_recall`**: the per-peer memory artifact now points at
  the peer-child session id (write side; the read side is peer-keyed and
  unchanged).

#### Fixed
- **Flaky `concurrent_messages_serialize_turns` test** (was failing ~6/8
  full-suite runs): the FIFO assertion raced task spawn order against
  mutex acquisition; now waits for the first turn to start before issuing
  the second message.
- **`create_session` peer clobbering**: the peer stamp on the index entry
  was silently lost when a post-create `set_*` (`set_slug`, `set_standing`,
  …) rewrote the entry through the metadata cache. The peer is now stamped
  on the metadata before caching.

### Agent–session paradigm sprint (2026-08-15)

Sprint branch `feat/agent-session-paradigm` closing the gaps mapped in
`docs/architecture/AGENT_SESSION_PARADIGM.md`.

#### Fixed
- **Maintenance prune no longer destroys protected transcripts.** The
  30-day idle prune now exempts the `root:*` family, `archived`
  sessions, and sessions carrying the new `standing` flag — previously
  it deleted the JSONL transcript of any session idle past the cutoff
  with no exemptions (`peko-rs/session/src/index.rs`).
- **`session list` reports subagent liveness.** `run_active` now also
  consults the unified `AsyncTaskRegistry`; subagent runs never hold
  `InboxRegistry` permits, so live subagent sessions previously showed
  `run_active: false`.
- **Channel subscribers resume from persisted cursors at daemon boot.**
  Previously every restart re-observed each channel's full event
  history.

#### Added
- **`standing` flag** on `SessionMetadata`/`SessionEntry`
  (serde-default false): marks a session as a durable, standing entity —
  exempt from idle pruning. Groundwork for standing named children.
- **`session move` (reparent).** The session tool gains a 10th action,
  `move` (`session_key` + `new_parent`), reparenting a session with its
  subtree. Guards mirror `delete` (not-self, not-ancestor, subtree scope
  on both endpoints, `root:*` source refused, live-run refusal) plus a
  **cycle guard** — moving a session under itself or one of its
  descendants is refused. The reparent is recorded as a `System` event in
  the session's JSONL; the `session.created` header's parent field stays
  stale-by-design (the index is the source of truth for parentage).
- **Session path addressing (slugs).** Sessions gain an optional `slug`
  (per-parent-unique path segment, serde-default). Any `session_key` tool
  param now accepts `/a/b/c` paths — anchored at the caller's tree root,
  walked by slug, resolved before ownership guards. Set points: `session
  rename --slug`, Agent tool `new` with `name`, `branch` (derives
  `<source>-branch`), `move` (uniqueness re-checked at destination).
  `session list` shows `slug` + computed `path`. Ids remain the canonical
  key; paths are a computed view (`peko-rs/session/src/path.rs`).
- **Standing named children.** `principal.toml` gains a `[children]`
  table (`name → { subagent_type, description? }`). Declared children are
  ensured to exist at root-agent run setup — created as sessions flagged
  `standing`/`trigger="spawn"`/slug=name, parented at the owner root, NO
  LLM turn (`peko-rs/core/src/principal/children.rs`); the declaration is
  recorded as a `System` event in the child JSONL. The Agent tool's `new`
  with a `name` matching an existing standing child in the caller's
  subtree **attaches** (resumes that session) instead of minting a fresh
  UUID — "spawn once, resume by name" is one operation. Standing sessions
  are exempt from idle pruning (Phase 0 flag).
- **Principal trunk session `root:self`.** A peer-less, forever-continuous
  self session — the `/` of the principal's session tree
  (`trunk_session_id()` in `principal/routers/root.rs`; inherits the
  root-family delete/archive/move guards via the `root:` prefix).
  `CronJobAction::Send` gains `target: "trunk"` (CLI: `--target`): the
  turn lands in `root:self` via `PrincipalManager::receive_trunk`
  (owner-as-proxy permissions, no chat-log projection, no note
  cross-post). Default cron behavior (`root:cron:{owner}` + note) is
  unchanged. This is the principal's heartbeat: a trunk-targeted cron job
  on a fixed cadence keeps the principal an active actor, with
  `budget_per_cycle` / `cost_per_call_max` as the wake budget.
- **SpawnTool wake + keepalive floor (Phase 3b).** Per PEKO.md §K, cron
  `SpawnTool` completion wakes (`wake_on_completion`) now steer the trunk
  inbox (`root:self`) instead of the owner's conversational root — one
  PEKO, one root. Trunk-targeted `Send` jobs with an `Every` schedule are
  refused below a 60s floor (`TRUNK_MIN_INTERVAL_MS`) — a faster
  self-targeted keepalive is a token-burn anti-pattern.
- **Channel passive binding (Phase 4).** `peko channel create --bind
  <session-id|/path>` marks a DM-tier channel: an inbound post from
  another member wakes the bound session via the subagent resume path
  (`SubagentExecutor::resume_and_execute`) and the reply is posted back
  as the principal (`peko-rs/core/src/daemon/channel_binding.rs`).
  Anti-loop: the responder never processes its own posts (author match).
  Turns serialize per channel (FIFO) and coordinate with Agent-tool runs
  via the shared registry key. No chat-log projection — the channel's
  event log is the record of both directions. Unbound channels are
  unchanged (active polling only). Restart-safe via persisted cursors;
  at-most-once delivery by design.

### Round 7: chapters deleted, stable-id paging, session/Agent tool surface (2026-08-13)

The chapter concept was a category error — a session is one agent.
Session ids are now stable for life; transcript growth is handled by
storage-internal paging, and the tool surface is split honestly:
`session` does pure storage reads/writes, `Agent` drives anything
that causes LLM work.

#### Changed
- **Stable-id transcript paging.** Session ids never change. When a
  JSONL exceeds `rotate_bytes` it pages in place: `<id>.jsonl` →
  `<id>.N.jsonl` (N chronological, 1 = oldest) and appends continue
  into a fresh `<id>.jsonl`. `load_events`/`load_normalized` stitch
  pages 1..N + the current page transparently, so history, context
  build, compaction, and transcript search all see one continuous
  transcript. `delete` removes all pages + sidecars; `branch` copies
  them. Legacy `#`-suffixed JSONLs stay inert on disk.
- **`session` tool: 9 storage actions.** `status` / `list` /
  `history` / `search` / `rename` / `delete` / `branch` / `archive` /
  `unarchive` — no LLM involvement. The root session (`root:*`)
  refuses delete/archive ("continuous, managed by the engine");
  self-mutation is refused; `branch` makes a non-running copy (resume
  it via the `Agent` tool); `archive` hides from `list` unless
  `include_archived:true`. `limit` schema default is now 100
  (matching the handler); an invalid IANA timezone is a structured
  error instead of a silent local-time fallback;
  `SessionInfo.is_active` was dropped (`run_active` carries the real
  signal).
- **`Agent` tool: 3 LLM-driving actions.** New `action` param
  (default `"new"`): `new` spawns (prompt + subagent_type), `resume`
  re-attaches a run to an existing spawned session (session_key +
  prompt + subagent_type), `compact` flags a session for
  engine-driven summarization at the target's next run (session_key
  only; returns immediately, no completion signal). `cleanup` is
  validated — `keep`/`delete`, structured error otherwise.
- **`trigger` field retained.** The spawn depth check in
  `agents/subagent_executor.rs` branches on `trigger == "spawn"`;
  dropping it is deferred to a follow-up.

#### Removed
- **Chapters.** `peko_session::chapters` (`ChapterRequest`,
  `chapters.json`, `chapter_id`), `SessionRuntime::{new_chapter,
  resume_chapter}` + `ChapterChangeOutcome`,
  `SessionManager::rename_session_id`, and the `Session`-id re-keying
  in `append_to_storage` are gone. `ownership::is_live_base_id` /
  `chapter_family` deleted — "live base" is now
  `id.starts_with("root:")`.
- **`Agent` tool `resume_session` param** (breaking for tool
  callers). Use `action:"resume"` + `session_key` instead.

### Agent-owned session management — the unified session/run framework (2026-08-09)

The "coin model": a **session** is a persisted conversation, a **run**
is a live agentic process attached to exactly one session. The `Agent`
tool is the generate side; the `session` tool is the persist side. One
ownership/guard module, one run-permit protocol, and one guarded delete
path now govern both.

#### Added
- **`session` tool mutation actions.** Beyond `status`/`list`/`history`:
  `search` (case-insensitive transcript scan), `branch`, `rename`,
  `archive`/`unarchive`, `delete` (subtree-aware, `recursive:true` for
  descendants, children-first), `compact` (persisted request flag), and
  the chapter pair `new`/`resume`.
- **Chapter rotation.** `session new`/`resume` write a durable pending
  change to `<sessions_dir>/chapters.json`; `agent_runner` consumes it
  at the next run start (before open/create, under
  `session_creation_lock`) and rotates the deterministic live id
  (`root:{peer}`) to `root:{peer}#<YYYYMMDD-HHMMSS>` via
  `SessionManager::rename_session_id`. The live id is reused, so
  InboxRegistry mappings, queued steering, and completion announcements
  are untouched; a daemon restart loses nothing.
- **Ownership tree + guards.** `peko-rs/core/src/session/ownership.rs`
  classifies the caller from its session's `parent_session_id` chain:
  base-session callers manage the whole store, spawned callers only
  their own subtree (reads are filtered, out-of-tree mutations and
  `history`/`status` refuse). Self/ancestor deletion, live-`root:*`-id
  delete/archive, archived compact/resume, and cross-family resume all
  produce structured, LLM-actionable refusals. Shared by the session
  tool adapter and the `Agent` tool path.
- **Run-permit delete protocol (D3).** `session delete`/`archive`
  acquire `InboxRegistry` run permits for the target and every
  descendant and hold them across the operation; a busy session
  produces a refusal naming it. `list` surfaces `run_active`.
- **`Agent.resume_session` (persistent subagents).** Re-attaches a new
  task run to an existing spawned session with its full prior history.
  Guards: target must exist, be `trigger=="spawn"`, not be the caller's
  own session or an ancestor, be in-subtree for spawned callers, not be
  archived, and not have an active run — detected via the unified
  AsyncTaskRegistry (`child_session_id` on `SubagentMetadata`), because
  subagent runs do not hold InboxRegistry permits. Mutually exclusive
  with `isolated`. Explicit `parent_session_key` (context seeding) is
  ownership-validated. `cleanup:"delete"` on both spawn and resume
  routes through the guarded delete.
- **Forced compaction (D2).** `compact_requested` on
  `SessionEntry`/`SessionMetadata` (serde-default `false`, alongside
  `archived`). The orchestrator ORs `SessionView::peek_compact_request`
  — read through to disk via `SessionIndex::get_uncached` so a flag set
  mid-run by the session tool is seen at the next iteration — into the
  threshold decision and clears it only when compaction genuinely
  starts; the "Summarizing…" event gets forced-case wording.
- **Integrity fixes.** `SessionStorage::delete_session`/`copy_session`
  now hold the append `FileLock` (no orphan-recreate race, consistent
  branch snapshots); `SessionHandle::exists()` no longer returns true
  for missing sessions; deleting a session scrubs its id from
  `PeerInfo.session_ids` and `clear_active_for_peer` drops only the
  active pointer (`PeerInfo.active_session_id` is now
  `Option<String>`; other sessions of the peer stay routable);
  `branch_session_by_id` copies `peer_type`/`peer_id` onto the branch;
  `session status` on an unknown session returns the real error instead
  of fabricated zeros; the prompt's `SessionSnapshot` carries the real
  session id (`TurnPromptContext.session_id`).

### `send_peer` unification, cron `Notify` delivery, and `delay` (2026-08-08)

From the round-4 live-verification observations (addendum in
`scripts/e2e/reports/2026-08-07-non-technical-user-subagent-cron-principal.md`):

#### Added
- **`CronCreate` `delay` arg.** Relative one-shot delays (`"90s"`,
  `"5m"`, `"1h"`) resolved to an absolute `at` at registration time —
  the model no longer does RFC3339/timezone arithmetic (the top
  remaining turn-cost driver: two failed `at` attempts burned ~44k
  input tokens on one reminder turn). Mutually exclusive with explicit
  schedule fields. `parse_duration_ms` moved from the CLI into
  `peko-cron` (`peko_cron::tools::parse_duration_ms`) and is shared by
  both.
- **`send_peer` tool — `principal_send` renamed and unified.** One
  tool for messaging any `Subject`: `user:<id>` delivers a
  fire-and-forget note (`📨 [<label>] …`, `MessageSource::Agent`) to
  that user's conversational session; a Principal DID runs the legacy
  synchronous RPC (wire envelope, signing, transport selection all
  unchanged; result gains `kind: "principal" | "user"`). The user
  branch is gated to the user who originated the current run —
  resolved from the calling session id (`root:{peer}` / v2 keys /
  subagent-suffix stripping / spawn-overlay `parent_session_id`
  walk), never from the never-populated `ToolContext.peer_id`. The
  tool registers whenever a caller principal DID is bound — no longer
  gated on the cross-runtime context — so it works in tunnel-less
  daemons and for **subagents** (caller DID propagates down the spawn
  tree via a `OnceLock` on `SubagentExecutor`, set by
  `Agent::with_caller_principal_did`). The starter capability bundle
  gains `tool:send_peer` (existing principals: `peko capability grant
  --principal <name> tool:send_peer`).
- **`PeerMessenger` port** (`peko-rs/core/src/principal/messenger.rs`):
  trait + `PrincipalPeerMessenger` impl + global registry (same
  pattern as `CronRuntime`), installed by the daemon at startup.
  Shared by the `send_peer` user branch and the cron engine's note
  delivery.
- **`CronJobAction::Notify`** — pure delivery: the message text lands
  in the owner's conversational session as a labeled `⏰` note with NO
  agent turn (0 tokens, instant). The `CronCreate` tool's `message`
  arg now builds these. `Send` keeps its deferred-`peko send` turn
  semantics for CLI compatibility.
- **`MessageSource::Agent`** for agent-pushed notes.

#### Changed
- `PrincipalSendTool` → `SendPeerTool`; args `target_principal` →
  `target` (+ optional `label`). No alias — pre-launch.

### Cron/session structural fixes (2026-08-07 round-3 field-test findings F1–F5, P1–P2, U1)

From `scripts/e2e/reports/2026-08-07-non-technical-user-subagent-cron-principal.md`:

#### Fixed
- **IPC receiver no longer dies after 60s of silence (F1, critical).**
  `ipc::stream::spawn_receiver` treated the read timeout as fatal and
  exited its loop, leaving every long-lived `DaemonClient` deaf while
  `send_request` kept writing into the void — the in-daemon cron
  runtime adapter went deaf this way, so conversational
  CronCreate/CronList added jobs but their responses were lost (60s
  hangs, duplicate jobs, retry storms). Idle timeouts now `continue`;
  only genuine socket errors terminate the loop (with an error fan-out
  to pending streams), and `DaemonClient::send_request` fast-fails
  with a reconnect hint if the receiver task is dead.
- **Cron runtime adapter is in-process, not an IPC loopback (F1).**
  `DaemonCronAdapter` now holds `Arc<daemon::cron_ops::CronOps>` — the
  owner-cap gate + schedule/history writes extracted verbatim from the
  IPC handler — instead of a `DaemonClient` connected to its own
  daemon. The conversational CronCreate path no longer depends on the
  IPC round trip at all; the IPC handler is a thin packet wrapper over
  the same `CronOps`.
- **Cron `Send` jobs no longer hijack the user's conversational turn
  (F2).** Root session routing is channel-aware: automation traffic
  (`ChannelKind::Cron`) runs in `root:cron:{peer}` instead of the
  human's `root:{peer}` session. Previously a cron message draining at
  an iteration boundary was indistinguishable from human input, and
  the model answered the cron tick instead of the user's pending
  request. The fired job's outcome is appended to the conversational
  session as a labeled `⏰ [cron job '<name>' fired] …` note
  (`MessageSource::Cron`, user-role on purpose: Anthropic-style
  adapters map system-role messages to the top-level system parameter,
  last-one-wins) so the user still sees the result.
- **One run row per cron run, and one-shot jobs always reap (F3).**
  `execute_job` opened a run row at fire and `record_run`'d a SECOND
  row (same id) at completion — duplicate rows, and SpawnTool runs
  stuck "running" forever. Completion now closes the start row in
  place (`finalize_run`, or `attach_run_output` for the still-open
  SpawnTool fire). One-shot (`delete_after_run`) reaping keys on the
  fire, not `status == "success"` — the old gate parked failed/spawned
  one-shots on the 100-year sentinel forever.
- **`CronCreate` has a `message` arg for reminders (F4).** The tool
  previously built only SpawnTool jobs, so "remind me …" requests ran
  an Agent turn whose output nobody saw. `message` builds a `Send` job
  (delivered as the labeled note above); the schema description steers
  the model: `message` for reminders/notifications, `prompt`/`tool`
  for background work, and states plainly that `prompt` produces no
  user-visible output. One-shot derivation was also fixed:
  `recurring: false` was previously accepted and silently ignored, and
  `at` jobs (which can only ever fire once) now default to
  delete-after-run when the caller passes no recurrence hint — a fired
  `at` job no longer parks on the 100-year sentinel (round-4 live
  verification finding).
- **Spawn-session JSONL header matches the index (F5).**
  `session.created` events for spawn overlays now record
  `trigger: "spawn"` (was `"user"` while the index said `"spawn"`) via
  `SessionTrigger::from_label` threaded through
  `SessionManager::create_session`.
- **Root prompt honesty (P1).** The root agent prompt no longer claims
  a roster of "specialist agents" — the catalog it is given is the
  COMPLETE list, and it must not invent named specialists. Added a
  tool-use rule: never retry an identical failing call more than once.
- **Identical-failure circuit breaker (P2).** `peko-engine`'s
  `ToolExecutor` short-circuits a tool call that repeats the exact
  same `name + arguments` after 2 consecutive identical failures,
  returning a stop-retrying tool error instead of burning tokens on an
  unbounded retry loop.
- **`peko cron list` masks the 100-year sentinel (U1).** One-shot jobs
  whose `next_run` is the far-future sentinel now render `—` instead
  of a raw year-2126 timestamp.

#### Internal
- `peko-session`: `add_user_with_source` on the unified session +
  handle; doctest paths fixed (`peko_core::…` → `peko_session::…`,
  stale from the workspace migration).

### Session-integrity + cron hardening (2026-08-07 findings N1–N3)

Follow-up to the field-test fix pack below, from its verification pass:

#### Fixed
- **Concurrent writers can no longer brick a session, and already
  bricked sessions recover.** Two halves (finding N1):
  - *Prevention:* the daemon's `PrincipalManager` now shares
    `AppState`'s `InboxRegistry` instead of keeping a private one.
    The per-session run permit now actually serializes cron/tunnel
    turns against in-flight CLI turns — previously the two registries
    were independent permit spaces, so a cron turn could append a user
    message between an assistant `tool_call` and its `tool_result`,
    after which Anthropic-style providers reject every request ("tool
    call result does not follow tool call"). Steering messages drained
    by a live loop are now also persisted to the session JSONL at the
    iteration boundary (they were in-memory only, leaving replies
    without questions on reload).
  - *Hardened (same day):* the shared registry is now a **required
    constructor parameter** of `PrincipalManager::{new,
    with_path_resolver}`, `AsyncExecutor::{new, with_registries}`, and
    `ExtensionAsyncAdapter::new` — the private defaults and the
    `with_inbox_registry` builder overrides are gone, so a future
    construction path cannot silently reintroduce a split permit
    space; the compiler forces an explicit choice. Components that
    genuinely own a private registry (placeholders, per-call scopes,
    tests) use the explicitly-named
    `extensions::framework::async_exec::executor::standalone_inbox_registry()`.
  - *Repair:* new `peko_message::repair::repair_history` — a pure,
    idempotent normaliser applied at the engine's history intake. It
    re-pairs tool_calls with their (possibly displaced) tool_results,
    backfills synthetic error results for interrupted calls, drops
    orphan results, and merges consecutive same-role messages. Storage
    stays faithful; repair is consumption-side only.
- **Agents now have a wall clock.** New volatile `{{current_time}}`
  prompt placeholder ("Current time: <local> / <UTC>") in the per-turn
  suffix — relative-time requests ("remind me in 2 minutes") no longer
  require the model to guess the date (finding N2a). Kept out of the
  cache-stable prefix.
- **Past one-shot `at` times are rejected at creation.**
  `CronScheduler::add_job` (the single chokepoint for the CronCreate
  tool, CLI, and IPC paths) now errors on `at <= now` instead of
  accepting the job and parking it on the 100-year sentinel where it
  showed "active" but never fired (finding N2b). **Behavior change:**
  `peko cron at` with a past timestamp previously fired immediately on
  the next poll; it is now rejected. The `CronCreate` tool's `at`
  schema description now tells the model the timestamp must be in the
  future and points at the system-prompt clock.
- **`peko cron at` prints the resolved timestamp** in its confirmation
  instead of echoing the raw `--at` input (finding N3).

### Field-test fix pack (2026-08-07 cron/session/IPC findings)

Fixes from `scripts/e2e/reports/2026-08-07-non-technical-user-cron-session.md`:

#### Fixed
- **Long tool calls no longer kill streaming turns.** The daemon now
  emits `ResponsePacket::Heartbeat` every `HEARTBEAT_INTERVAL_SECS`
  while draining a `PrincipalSendStream` run. Previously any tool call
  silent for >60s tripped the CLI's per-packet idle timeout
  (`CLI_TIMEOUT_SECS`), and the daemon then aborted the healthy run
  with `Connection refused` when the next event couldn't reach the
  gone CLI (Finding 3). The packet variant and the CLI-side ignore
  paths predated this emitter, so old clients are wire-compatible.
- **Failed runs leave a trace in `peko log`.** Both failure exits of
  the streaming handler (route error; sink-write abort) now record a
  `⚠ Run failed: …` entry via `record_chat_response` instead of
  leaving the user's message unanswered in the chat log (Finding 8).
- **Cron tools granted to new principals.** `Capabilities::starter_bundle()`
  now includes `tool:CronCreate` / `tool:CronList` / `tool:CronDelete`.
  The tools were always registered (`ToolRuntime::register_builtins`)
  and the `principal:write_cron` capability was granted, but the
  missing `tool:` grants let `is_tool_enabled` filter them out of the
  LLM's toolset — principals honestly reported "I have no scheduling
  tool" (Finding 2). Pre-existing principals need a one-time
  `peko capability grant <principal> tool:CronCreate tool:CronList
  tool:CronDelete` (or equivalent) to pick the tools up.
- **One-shot cron history survives auto-delete.** `CronScheduler::delete_job`
  no longer purges run records, and the IPC cron handler resolves a
  job's principal via preserved run records when the job itself is
  gone — `peko cron history <id>` now works for fired one-shots
  (Finding 4). Run growth stays bounded by the existing 1000-run cap.
- **Interval cron jobs no longer drift.** `Every` jobs anchor their
  next fire to the *scheduled* time (new
  `peko_cron::calculate_next_interval_anchored`) instead of the actual
  finish time, so poll-tick quantisation no longer accumulates (a 60s
  job fired every ~75s; Finding 6). Missed slots after downtime are
  skipped without bursting.
- **Session index integrity.** `create_session` now copies
  `parent_session_id` / `trigger` into the `sessions.json` index entry
  (previously hardcoded to `null` / `"user"`); spawn (subagent)
  sessions are stamped with their parent session id and
  `trigger: "spawn"`; `turn_count` is maintained (bumped per user-role
  message) instead of always reading 0 (Finding 7).

#### Added
- **Cron CLI ergonomics** (Finding 5): `peko cron every --interval`
  accepts human durations (`30s`, `5m`, `1h`, `1d`) alongside
  `--interval-ms`; `peko cron at --at` accepts relative delays
  (`in 10m`) alongside RFC3339; `cron remove` / `run` / `history`
  accept `--name <job-name>` (exact match, errors on ambiguity) as an
  alternative to the hex job ID.

### Multi-model subagents (PR #346)

All four phases merged as one squashed commit (`ee4895a6`).

#### Added
- **Per-spawn model choice.** `AgentTool`'s `model: Option<String>`
  parameter is no longer a no-op — the parent's choice is threaded
  through `SpawnRequest` → `ExecutionConfig.model_override` →
  `SubagentRuntime::resolve_agent_config` →
  `SubagentExecutor::execute_subagent_task`. Pre-flight
  `SpecGate::check` runs against the override so a parent picking
  Opus for a tool-using subagent with an opus-without-vision spec
  is refused **before** any LLM traffic. Refusals surface as
  `SpawnError::SpecGateFailed { model_id, reason }`.
- **F39 production quota wiring.** Every `SubagentExecutor::new`
  site chains `.with_quota_meter()` / `.with_peer_meter()` so
  subagent LLM calls are attributed to the spawning principal
  (previously fell open to `unlimited()`).
- **`model_list` builtin tool.** Parent agents can discover their
  principal's catalog in-band. Filter args: `filter`
  (`vision | tools | thinking | priced | json_mode`) AND-combined
  with `contains <NEEDLE>` (matches `id` case-sensitive,
  `display_name` and `note` case-insensitive).
- **Per-model user notes.** `ModelConfig.note: Option<String>` (≤500
  chars, validated by `upsert()`). CLI: `peko model add --note`,
  `peko model edit --note` (empty clears), `peko model show` prints
  the block, `peko model list --detailed` truncates to 80 chars.
- **Two-gate cost ceiling.**
  `QuotaConfig.cost_per_call_max` runs at spawn time
  (`SubagentExecutor::spawn_and_execute`); `budget_per_cycle`
  runs mid-stream (`QuotaMeter::try_charge_with_cost`). Refusals
  surface as `SpawnError::CostCeilingExceeded { estimated,
  ceiling, model_id }`. `QuotaState.cost_usd` persists cycle
  spend across restarts.
- **First-use-per-model audit warnings.** `peko-engine` gained an
  `AuditSink` trait (typed `AuditEventView`; orphan-rule
  workaround via free `severity_into_obs`). The engine emits a
  `model.selected` audit event at `Warning` severity the first
  time `(principal, model)` fires, `Info` thereafter. First-use
  state persists to `<workspace>/seen_models.json` (atomic write,
  corrupt-file tolerant).

### Chat-session separation (PRs forthcoming)

The runtime now keeps two distinct stores of conversation data,
matching the data-model split:

- **Session JSONL** (`<principal>/sessions/<id>.jsonl`) is the
  principal-owned, mutable, internal working memory used by the
  agent loop. Its schema is free to evolve with the runtime.
- **Chat log** (`<data-dir>/chat_logs/<blake3(principal_did)>/<blake3(peer)>.jsonl`)
  is the runtime-owned, append-only, consumer-visible record of
  what was actually exchanged. It powers `peko log`, the
  `principal_log` IPC, and the desktop's chat page.

#### Added

- `src/chat_log/` — runtime-owned chat-log domain: `ChatLogStore`,
  `ChatThreadKey`, `ChatLogMessage`, `ChatLogPage`, opaque
  thread-bound cursors, sender-participant validation.
- `src/common/persistence/` — extracted `FileLock` and
  `append_bytes_durable` for shared use by session and chat-log
  stores (compat re-export shim at `src/session/lock.rs`).
- `PathResolver::chat_logs_dir()` — `{data_dir}/chat_logs`,
  auto-created in `ensure_dirs`.
- Principal-boundary recording (`PrincipalManager::receive` /
  `receive_streaming`) — appends the external input before dispatch
  and the authoritative final response before returning. Persistence
  failures on the input side reject dispatch; persistence failures
  on the response side are best-effort.
- Caller-side `principal_send` recording
  (`tunnel::principal_send_tool.rs`) — appends request and response
  lines for both the local same-runtime shortcut and the
  cross-runtime path; transport failures, hub rejections, and
  decode failures do not create phantom reply lines.
- `PrincipalManager::remove` — removes the principal's chat-log
  shard directory as part of the principal-lifecycle invariant.
- Channel filter: peer-chat channels (`Cli`, `Http`, `Hub`, `A2a`
  covering `A2a` and `P2p`, `Webhook`) are recorded;
  automation (`Cron`, `FileWatch`) is deliberately excluded.

#### Changed

- `RequestPacket::PrincipalLog` and `ResponsePacket::PrincipalLog`
  IPC shape. The legacy `events: Vec<HistoryEvent>` array,
  `session_id`, and `truncated` fields were removed. The new
  response carries `messages: Vec<ChatLogMessage>`, `next_cursor`,
  and `has_more`. The `RequestPacket::PrincipalLog` request gains
  `cursor: Option<String>`.
- `peko log` CLI accepts `--cursor` and walks pages until exhaustion
  (or a 25-page hard cap). JSON output reflects the new envelope.
- `peko-runtime/src/commands/log.rs` and
  `src/ipc/handlers/principal.rs` consume the new wire shape
  directly; `load_principal_session_events` and the session→history
  projection path are removed from the principal-log read surface.
- Desktop (sibling repo): `src/types/index.ts` exposes
  `ChatLogMessage` / `ChatLogPage`; `src/lib/api.ts` and
  `src/hooks/usePrincipals.ts` thread `cursor`; `src/pages/Chat.tsx`
  maps persisted chat lines directly onto chat bubbles (no
  session-event projection); `src/pages/PrincipalLog.tsx` adds
  "Load older" paging that walks pages via `nextCursor` and
  dedupes by message id.

#### Notes

- Pre-launch clean cutover: there is **no** migration path from
  session JSONL to chat log, **no** legacy fallback, and **no**
  retention. Removing a principal deletes that principal's own
  chat-log shards only; counterpart views held by other
  principals remain because they are owned by the other principal.

### Claude Code core tool parity (in progress)

A multi-phase program to align peko's built-in tool names and schemas with
Claude Code's core tool surface, while preserving peko's daemon-first
execution, A2A protocol, and extension system.

#### Added

- `docs/architecture/builtin-tools.md` — canonical catalog of built-in tools,
  schemas, and Claude parity status.
- `tests/core_tools.rs` — integration-test harness for golden-transcript parity
  fixtures.
- Configuration gates `enable_async_tools` and `enable_task_tools` on
  `ToolFactoryConfig` and `BuiltinToolRegistrarConfig`.

#### Upcoming (tracked on `tool-parity-core-subset` branch)

- (none — all scoped tools are now in place)

### Architecture cleanup (PRs #153–#156)

#### Removed

- Dead code across the engine, IPC, and extension layers (PR #153).
- The write-only tool-latency metrics/tracing toolkit that was recorded but
  never read; the `observability` module is now `pub(crate)` (PR #155).
- `test-utils` removed from the default feature set, so test plumbing
  (`pub mod test_utils`, the `peko::daemon` internal re-export, and the
  resolver/hub-directory test hooks) no longer ships in the production binary.
  `tests/tunnel_e2e` is gated behind the feature — run it with
  `cargo test --features test-utils --test tunnel_e2e`.

#### Changed

- Tidied stale section headers in `src/lib.rs` (PR #154).
- Marked ADR-027 (unified packaging), ADR-031 (agent team membership), and
  ADR-037 (agent extension bundling) as superseded by ADR-039 / ADR-041, and
  refreshed capability-grant wording (PR #156).

### Added

- Planning-todo family `TaskCreate`, `TaskGet`, `TaskList`, and `TaskUpdate`.
  Todos are stored in a per-session JSONL sidecar
  (`{session_key}.todos.jsonl`) alongside the main session JSONL, using the
  same atomic-write durability strategy as session storage. `TaskCreate` takes
  `subject`, `description`, and `activeForm`; `TaskGet` and `TaskUpdate` take
  `taskId`; `TaskList` accepts an optional `status_filter`. These tools are
  registered per-agent so they resolve to the agent's own session directory.

### Changed

- **BREAKING**: Split built-in tool `task` into `AsyncSpawn`, `AsyncOutput`,
  `AsyncStop`, `AsyncStatus`, and `AsyncList`. `AsyncSpawn` runs any built-in
  tool in the background (`tool`, `params`, optional `label`); `AsyncOutput`
  reads a task's result with optional blocking (`block`, `timeout`,
  `tail_lines`); `AsyncStop` cancels a running task; `AsyncStatus` returns the
  current status; `AsyncList` lists tasks with optional filters. The previous
  `task` tool and `sub_command`-based schema are removed. Legacy
  `disabled_tools` entries `"task"` and `"async"` disable the entire Async*
  family. Update agent configs, whitelists, and prompts that referenced the old
  name.
- **BREAKING**: Renamed built-in tool `read_file` to `Read`. The tool now
  reports its canonical name as `Read` and its schema uses `file_path`
  (with `offset`, `limit`, and `pages` support). Update agent configs,
  whitelists, and prompts that referenced the old name.
- **BREAKING**: Renamed built-in tool `write_file` to `Write`. The schema now
  uses `file_path` instead of `path`; `mode` and `encoding` extensions are
  unchanged. Update agent configs, whitelists, and prompts that referenced
  the old name.
- **BREAKING**: Renamed built-in tool `str_replace_file` to `Edit`. The schema
  now uses `file_path`, `old_string`, `new_string`, and `replace_all` (default
  false); the previous `path` + `edit` object shape is removed. Update agent
  configs, whitelists, and prompts that referenced the old name.
- **BREAKING**: Renamed built-in tool `shell` to `Bash`. The schema now uses
  `command`, `description`, `cwd`, `run_in_background`, and `timeout`. Blocking
  execution returns `{ exit_code, stdout, stderr, success }`; background
  execution returns an async task receipt discoverable by the future `Async*`
  family. Update agent configs, whitelists, and prompts that referenced the old
  name.
- **BREAKING**: Split built-in tool `cron` into `CronCreate`, `CronDelete`, and
  `CronList`. `CronCreate` uses `prompt` plus one schedule source (`cron`, `at`,
  `interval_ms`, `idle_ms`, or `event_topic`) and `recurring`/`durable` flags;
  `CronDelete` takes `id`; `CronList` takes no required arguments. The previous
  `sub_command`-based schema is removed. Update agent configs, whitelists, and
  prompts that referenced the old name.
- **BREAKING**: Renamed built-in tool `agent_spawn` to `Agent`. The schema now
  uses `prompt`, `subagent_type`, `description` (renamed from `label`), and
  `model`; `isolated`, `cleanup`, and `parent_session_key` are unchanged.
  `subagent_type` resolves to `~/.peko/agents/<subagent_type>/config.toml` via
  `AgentService`. Update agent configs, whitelists, and prompts that referenced
  the old name.
- **BREAKING**: Renamed the Rust crate from `pekobot` to `peko`. Update all
  `use pekobot::...` imports to `use peko::...`.
- **BREAKING**: Renamed the public Rust type
  `peko::common::types::config::PekobotConfig` to `PekoConfig`.
- **BREAKING**: Renamed the OS keychain service namespace from
  `pekobot-runtime` to `peko-runtime`. Existing keychain entries are not
  migrated; users will need to re-onboard (negligible at 0.1.0).
- Renamed the project name in all prose, documentation, and config defaults
  from "Pekobot" to "Peko". Binary name (`peko`), package layout on disk
  (`config.toml` `app_name = "peko"`), and Python SDK defaults are unaffected.

### Notes

- Phase 6 cleanup completed: updated `DATA_MODEL.md` with a new "Planning
  Todo Sidecar" section, refreshed `docs/architecture/builtin-tools.md` to
  include `TaskList`'s optional `status_filter`, and updated help text and
  `AGENTS.md` for the renamed `Async*` and `Agent` tools.
- Removed the one-release `label` alias from the `Agent` tool; `description`
  is now the only subagent tracking field.
- The MCP example server display name was renamed from
  `pekobot-memory-server` to `peko-memory-server`.
- The Python SDK package was renamed from `pekobot-tool` / `pekobot_tool`
  to `peko-tool` / `peko_tool`.
- Historical CHANGELOG entries below are preserved verbatim.

### Cleanup

- **Legacy `a2a_send` references scrubbed from doc comments and docs.** The
  tool itself was fully superseded by `principal_send` (ADR-023 + ADR-039);
  the `a2a_audit`, `a2a_signature`, `a2a_pending` modules and the on-wire
  event kinds `a2a.sent` / `a2a.received` / signature domain `a2a:v1` are
  unchanged. No behavior or wire changes; doc-only.

### Provider catalog & agent decoupling (v3)

This release decouples agent configs from provider/model/API-key
wiring. Pulled registry agents now work on any host with at least one
configured provider; secrets never touch plaintext disk.

#### New: runtime-owned provider catalog (`~/.peko/providers.toml`)

- `src/providers/catalog.rs` — `ProviderCatalog`, `ProviderCatalogEntry`,
  `ModelInfo`, `ApiFormat`. Loaded on startup, shared via
  `Arc<RwLock<…>>`, persisted atomically.
- `src/providers/templates.rs` — `BUILT_IN_TEMPLATES` (15 providers:
  openai, anthropic, ollama, groq, together, fireworks, moonshot,
  deepseek, cohere, openrouter, perplexity, xai, kimi, minimax,
  azure-openai) with curated model lists and known context lengths.
- `src/providers/resolver.rs` — `LlmResolver`. Resolution precedence:
  caller override > session-pinned > agent preference > runtime
  default > first enabled catalog entry. Optional `--bootstrap-env-keys`
  for headless / CI deployments.
- CLI: `peko provider {list, templates, add, remove, set-default,
  get-default, fetch-models}`.

#### New: secure secret store

- `src/common/secret_store.rs` — `SecretStore` trait with two impls:
  - `OsKeychainSecretStore` (production) using the `keyring` crate.
    Service name `"peko"`, account = `provider_id`. Same namespace as
    `peko-desktop`'s `vault/mod.rs`, so desktop-entered keys are
    visible to the runtime after this release.
  - `InMemorySecretStore` (tests only — explicit opt-in, never used in
    production).
- CLI: `peko credential {set, delete, list, test}` with a hidden
  terminal prompt via `rpassword`.
- The legacy plaintext `~/.peko/credentials.json` is no longer
  written. The migration (`migrate_adr_provider_catalog_v3`) reads it
  once on first run, moves every entry into the keychain, and deletes
  the file.

#### New: agent ↔ provider decoupling

- `AgentConfig.version` is bumped to `"3.0"`.
- The embedded `provider: ProviderConfig` field is gone (deleted in
  v3-cleanup, PR #44). Agents carry only `preferred_provider_id` and
  `preferred_model_id` soft hints; the runtime resolves these via
  `LlmResolver` against the catalog (`~/.peko/providers.toml`) and
  the OS keychain at request time. There is no hard binding between
  an agent and a provider.
- New constructors: `Agent::new_with_resolver(config, resolver)` and
  `Agent::new_with_session_manager_and_resolver(...)`. The original
  `Agent::new(config)` continues to work — it falls back to the legacy
  `provider` field so pre-v3 fixtures still compile.

#### Adapter signature refactor (per-call `model_id`)

- `ApiAdapter::build_request` now takes `model_id: &str` as the first
  argument. The `model` field is removed from `OpenAiAdapter`,
  `AnthropicAdapter`, and `OpenAiCompatibleAdapter` — adapters are
  model-agnostic. The model id is threaded per call.
- `ApiAdapter::parse_response(model_id, response)` and
  `parse_sse_event(model_id, data)` carry the model id into the parsed
  `ChatResponse` / `StreamEvent::Start` events.
- `Provider::chat_with_tools(model_id, …)` and
  `stream_with_tools(model_id, …)` thread `model_id` into the adapter.
- `MockAdapter::new()` (no model argument) — model is set per call.

#### Migration (`src/runtime/migration_v3.rs`)

- `migrate_adr_provider_catalog_v3(resolver)`:
  - Walks `~/.peko/agents/*/config.toml`.
  - For any non-default `[provider]` block, creates a matching
    `ProviderCatalogEntry` (if absent) and seeds soft hints.
  - Moves any literal `api_key` into the OS keychain under
    `provider_id`.
  - Bumps `version` to `"3.0"` and atomic-writes the config.
  - Idempotent — already-v3 files are skipped.
- `migrate_legacy_data` now calls the v3 migration after ADR-032/033.
- Verified by `legacy_agent_config_gets_v3_and_hints` and
  `empty_state_reports_zero_migrations`.

#### Registry round-trip hardening

- `AgentConfig` no longer carries a `provider` field at all (deleted in
  v3-cleanup, PR #44), so the OCI config blob embedded by
  `agent_to_registry_manifest` cannot carry a literal `api_key`.
- Legacy `.agent` packages still in flight are stripped
  defensively: re-hydration reads the v3-clean TOML.

#### Desktop (`peko-desktop`)

- `src-tauri/src/ipc/mod.rs` gains `credential_get`, `credential_set`,
  `credential_delete`, `credential_list` IPC clients that proxy to the
  runtime's secret store.
- `src-tauri/src/commands/settings.rs` `credential_*` commands now
  route through IPC rather than reading/writing the desktop's local
  `vault/mod.rs`. The OS keychain is the single source of truth.
- `peko-desktop/src-tauri/src/vault/mod.rs` remains in place for the
  PekoHub-token callers (`pekohub.rs`, `registry.rs`) — they will
  follow in a subsequent change.

#### Mid-session model switching

- Between turns only (per the v3 plan). New CLI flag: `peko send
  --provider X --model Y` (or equivalent SDK/IPC parameter). The
  resolved pair is captured in `SessionState` and reused for every
  LLM call within that turn. In-turn provider swap remains out of
  scope (documented as a future ADR).

### Fixed (issue #26) — Add typed `Principal` caller field to `AuditEvent`

The audit event carried caller identity as a free-form `Option<String>`
(`caller_id`, added by #17), so per-user, per-key, and per-agent audit
queries had to string-parse the legacy `user:{sub}` convention with no
way to distinguish `"user:alice"` from `"apikey:foo"` from
`"agent:helper"`. This change replaces `caller_id` with
`caller: Option<Principal>` — the canonical actor type from ADR-039,
serialized as `{kind, id}` so query code can index on the kind tag.

- **`AuditEvent.caller: Option<Principal>`** replaces
  `caller_id: Option<String>`. Wire format: `{kind, id}` (or
  `{kind: "public"}` for the unit variant). `skip_serializing_if` keeps
  legacy events compact.
- **`Observability::audit_with_caller`** now takes
  `Option<&Principal>` instead of `Option<&str>`. The plain `audit(...)`
  helper is unchanged.
- **`Observability::audit_security_with_caller`** is a new sibling of
  `audit_with_caller` that stamps the caller as a typed `Principal`
  AND marks the event as `AuditSeverity::Security`. Security events
  are the ones operators query by user when investigating an incident
  — leaving them unattributed would defeat the per-user audit query
  use case this PR headlines (issue #26 review feedback).
- **`Principal::from_bridge_user(sub)`** centralizes the tunnel-bridge
  caller projection — `user:` prefix + `"anonymous" → Public`
  special-case — next to the type's other constructors, so the
  `user:` shape can't drift between the dispatcher and `CallerContext::subject()`.
- **Tunnel dispatcher** uses `Principal::from_bridge_user(&caller_user)`
  at the audit emission site (the projection logic is no longer inlined).
- **Cron engine** uses `CallerContext::local().subject()` directly at
  the two `cron.execute` / `cron.result` audit sites (matches the
  canonical `Identity::Local` projection at `caller.rs:114`; the
  intermediate helper has been removed).
- **Test coverage** — new `audit_event_caller_principal_serialization`
  asserts the canonical `{kind, id}` shape round-trips through serde
  for `User`, `Agent`, and `Public` variants, and that `None` callers
  are omitted. New `audit_security_with_caller_records_caller_and_severity`
  pins the security-side migration. New `Principal::from_bridge_user`
  tests cover the `user:` prefix and the `"anonymous" → Public`
  special-case. Existing audit + observability tests updated for the
  field rename.

`PermissionGrant.granted_by` and audit queries on PekoHub itself are
out of scope (parallel PekoHub issue to follow).

### Added (issue #28) — Per-agent persistent `agent_did` over the tunnel

`Principal::Agent(agent.name)` (from #24 / ADR-039) keys agents by the
local name — fine in a single-runtime world, but ambiguous across
runtimes and fragile when an agent is recreated with a new local name.
This change promotes the existing per-agent `Identity` (already
generated and persisted under `KeyStorage` at `peko_home/identities/`)
to a first-class `agent_did` that flows through the tunnel, the
`a2a_send` wire, and `PermissionGrant` lookups.

- **`AgentConfig.agent_did: Option<String>`** — the per-agent stable
  identifier (`did:peko:local:<keyhash>`), persisted in `config.toml`.
  Back-filled on first `Agent::new()` via a new
  `Agent::backfill_agent_did` helper. Two agents with the same `name`
  on two different `peko_home` roots now have different `agent_did`
  values; the unit test `test_two_runtimes_same_name_different_did`
  pins this invariant. New helper `AgentConfig::wire_agent_id()` is a
  thin shim over `Principal::agent_wire_id` (the single source of
  truth for the resolution, including the empty-DID defense).
- **`InstanceAnnouncePayload.agent_did: Option<String>`** — PekoHub
  now stores the per-agent DID on the instance row so it can serve
  as the canonical primary key for `Principal::Agent(...)` callers.
  Wire field is `agentDid` (camelCase) and is omitted when `None` so
  pre-#28 PekoHub instances accept the payload without modification.
- **`A2aSendTool::with_caller_did(name, did)`** — when the calling
  agent has a `agent_did`, the `a2a_send` tool now projects that DID
  into `Principal::Agent(...)` on the wire (instead of the local
  name) so the receiving agent's session is keyed by a stable,
  runtime-independent identifier. The `caller_agent` annotation stays
  as the human-readable name regardless. The legacy `with_caller(name)`
  is preserved and used as the fallback for agents predating #28.
- **`load_or_create_identity` bug fix** — the previous lookup key
  was the agent name, but `KeyStorage::store` names files after the
  DID, so a fresh keypair was being generated on **every** agent
  start. The fix looks up by `config.agent_did` first, falls back
  to the name-keyed legacy path, and only then generates a new
  identity.
- **`Principal::agent_wire_id(Option<&str>, &str) -> String`** — the
  single source of truth for the DID-or-name resolution, with the
  empty-DID guard. `A2aSendTool` and `AgentConfig::wire_agent_id`
  both route through it (review of #34 concern #3).
- **Targeted backfill, not a full overwrite** (review of #34
  concern #1) — `backfill_agent_did` reads the existing
  `config.toml`, sets just the `agent_did` key on the parsed
  `toml::Value`, and re-serializes. Preserves hand-edited comments,
  key ordering, and any fields the parser doesn't know about.
- **Loud DID rotation logging** (review of #34 concern #4) — when
  `load_or_create_identity` recovers from a missing identity file
  (e.g. a backup restore that lost `peko_home/identities/`), the
  caller logs a `warn!` naming **both** the old DID (from
  `config.agent_did`) and the new one (from the freshly generated
  `Identity`) so the operator can correlate audit / grant breakage
  to the event. Cross-runtime grants to the old DID are orphaned
  by design until the follow-up DID-rotation ADR lands.
- **Test-safety guard for subagent path** (review of #34 concern
  #5) — `new_with_shared_executor` skips the on-disk backfill when
  the resolved config path is under `std::env::temp_dir()`, so
  tests that bypass `new_for_test` don't mutate the developer's
  real `~/.peko`. The in-memory identity is still valid; the next
  production-path call does the real backfill.

### Notes for reviewers

- **Name-fallback semantics (deliberate, see issue #28 comment):**
  when an agent has no `agent_did` yet (legacy config, first run
  before the back-fill completes), `a2a_send` falls back to the
  local name. Within a single runtime, the name and DID are
  interchangeable. **Cross-runtime references require a live
  `agent_did` — the name fallback is forgeable across runtimes by
  design.** This is consistent with the `agent_did` field being
  `Option<String>` and the `agentDid` wire field being skipped when
  `None`. DID rotation / key-compromise recovery is a follow-up ADR.
- This PR lands ahead of `ConekoAI/pekohub#11` (PekoHub agent
  enforcement); PekoHub will need to thread the new `agentDid`
  field through to its `instances` row. Pre-#28 PekoHub instances
  that ignore the new field will continue to work; PekoHub upgrades
  that want to enforce per-agent matching must include the field.

### Fixed (issue #17) — Plumb hub-attested user identity through the tunnel path

Pre-#17, the tunnel dispatcher hard-coded the user attribution to the
literal string `"web"` and `MessageRequest::new` defaulted to
`"default"` — so the audit trail, the rate limiter, and per-user tool
permissions all operated on a placeholder. With this change, every
proxied request carries the resolved pekohub user identity from end to
end, with **cryptographic verification** when a JWT is present:

- **Dispatcher** — `resolve_bridge_caller()` reads
  `Authorization: Bearer <jwt>` from the bridge payload first. When a
  `JwtValidator` is configured (via `auth_config.enable_pekohub_jwt`)
  the JWT is signature-verified (RS256 / EdDSA), audience-checked
  against the runtime DID, and expiry-checked, and the validated
  `sub` claim becomes the caller. The validated sub is cross-checked
  against `x-pekohub-user-id` and a mismatch is logged as a possible
  tamper attempt. Falls back to the unverified header only when no
  JWT is present or validation fails. Returns `"anonymous"` only
  when both are absent.
- **Hook layer** — `HookInput::ToolCall` gains a `caller_id: Option<String>`
  field, plumbed through `execute_tool_via_core_with_context` →
  `ToolExecutor::execute` → `HookInput::ToolCall` so every tool
  invocation inside the agentic loop carries the resolved caller.
- **Agentic loop** — `AgenticLoop` carries a `caller_id`, set via
  `with_caller_id()` by `Agent::execute_streaming_with_session`. The
  caller is `Some(user)` for real pekohub users, `None` for local CLI
  invocations and the dispatcher's `"anonymous"` fallback.
- **Audit log** — `AuditEvent` gains a `caller_id: Option<String>` field
  (serialized with `skip_serializing_if = "Option::is_none"` to keep
  legacy events compact). New `Observability::audit_with_caller()`
  helper stamps the resolved caller on every audit event that flows
  through the request path. The tunnel dispatcher now emits a
  `tunnel_proxied_request` audit event tagged with the caller on every
  proxied request.
- **Request defaults** — `MessageRequest::new`, `ExecutionRequest::new`,
  and `SessionManager::new` no longer default `user` to `"default"`.
  The default is now `String::new()`, with a doc comment that
  production callers must set it explicitly via `.with_user()`. The
  two legacy-data fallbacks in `SessionManager::get_or_load_session`
  (peer info missing in the index) and `unified::Session::from_entries`
  (no peer provided) also drop the `"default"` literal — empty
  `sender_id` is the new fallback, distinguishable from a real resolved
  caller.
- **Agentic-loop `run` method** — `engine/agentic_loop.rs:243`'s
  hardcoded `Peer::User("default".to_string())` now uses
  `self.caller_id` (set via `with_caller_id` from the agent service),
  falling back to `Peer::User("local")` for the no-caller local-CLI
  case. The session's `sender_id` is now the resolved caller, not the
  placeholder.

**Why this matters**: unblocks per-user rate limiting
([`src/auth/rate_limit.rs`](src/auth/rate_limit.rs) is already keyed
off `CallerContext`), per-user session scoping
([`src/session/key.rs:97`](src/session/key.rs#L97) keys by `sender_id`),
per-user extension permissions
([`src/extension/core/registry.rs:194-202`](src/extension/core/registry.rs#L194)),
and any future PekoHub→runtime feature that needs to know *which*
user is asking. The JWT wiring closes the "self-asserted header"
security gap called out in issue #17.

**Test plan**:
- All 1413 lib tests pass (3 ignored, 0 failed)
- All 6 `extension_packaging` integration tests pass
- 5 new dispatcher tests for `resolve_bridge_caller` (missing / empty /
  whitespace / non-string / happy)
- 5 new JWT-wiring tests for `resolve_bridge_caller` (signed /
  tampered / no-validator / header-only / case-insensitive header)
- 2 new observability tests for `audit_with_caller`
- 1 new audit serialization test (skip_serializing_if for `None`)
- 1 new `hook_io` test for `HookInput::ToolCall::caller_id`
- `JwtValidator`'s existing 9 unit tests (positive + tampered) still pass

**Note on `src/session/key.rs:201`**: the `"web"` string there is the
*channel* segment of the session key format
(`agent:{agent}:{channel}:{sender_id}`), not user attribution. The
user's identity is keyed via `sender_id`, which is now correctly
plumbed. No change needed.

### Fixed (issue #25) — Collapse IPC `(subject_id, subject_type)` into `subject: Principal`

The IPC `RequestPacket` variants for grant/revoke
(`agent_grant_permission`, `agent_revoke_permission`,
`team_grant_permission`, `team_revoke_permission`) now carry a single
`subject: Principal` field (ADR-039). The legacy two-field shape
(`subject_id: String` + `subject_type: SubjectType`) is accepted on
the wire for one release, with a `warn!` logged once per process per
variant-kind on the legacy path so operators can monitor the
deprecation window. New CLIs only emit `subject`.

**Why this matters**: pre-#25, the `AgentRevokePermission` /
`TeamRevokePermission` packets carried only `subject_id: String` with
no `subject_type`. The server handler hardcoded
`principal_from_string_with_default_user(&subject_id)`, which always
returned `Principal::User(...)`. Since on-disk `PermissionGrant`
stores `subject: Principal` with the proper kind
(e.g. `Principal::Agent("helper")` for an Agent-issued grant), and
the service-layer revoke matches via `g.subject == *subject`,
**revoking any Agent / Team / Public grant via the IPC layer was a
silent no-op** — pinned by three regression tests in
`tests/principal_back_compat.rs`. The fix closes this hole by
collapsing the wire to the canonical `Principal` and routing both
shapes through a single `RequestPacket::resolved_subject()` helper.

- New `RequestPacket::resolved_subject()` helper in
  [`src/ipc/packet.rs`](src/ipc/packet.rs) collapses the canonical
  `subject: Principal` and the legacy `(subject_id, subject_type)`
  pair into a single `Result<Principal, Error>`. Returns an explicit
  `Error` (surfaced as `ResponsePacket::Error` with message
  "missing subject: ...") when neither field is set — strictly
  better than the previous silent no-op.
- All four grant/revoke IPC variants now carry the new `subject`
  field; legacy fields are kept as `Option<...>` with
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so new
  CLI wire bytes stay clean (no `subject_id`/`subject_type` keys
  emitted).
- Server handlers in [`src/ipc/server.rs`](src/ipc/server.rs) no
  longer call `principal_from_string_with_default_user` or
  `principal_from_wire` directly — they call
  `RequestPacket::resolved_subject()` and surface `Err` as
  `ResponsePacket::Error`.
- CLI handlers in
  [`src/commands/agent/handlers.rs`](src/commands/agent/handlers.rs)
  and [`src/commands/team.rs`](src/commands/team.rs) emit the new
  `subject: Some(principal)` shape. CLI UX is unchanged
  (still `--subject <string>` with `"public"` sentinel); the
  `--subject-kind` flag is a follow-up.
- `SubjectType` and `principal_from_wire` are marked
  `#[deprecated]`. Both are still exported for the deprecation
  window and will be removed in the next release after the warning
  logs show no legacy traffic.
- New `tests/scenarios/s6_revoke_principal_collapse_e2e.rs`
  exercises the bug repro end-to-end: an Agent-issued grant +
  revoke via IPC removes the on-disk grant; same for Team grants;
  the legacy wire shape still works; missing-subject returns a
  clean error.
- The three `test_revoke_string_form_*` regression tests in
  [`tests/principal_back_compat.rs`](tests/principal_back_compat.rs)
  are rewritten from "pin the limitation" to "pin the fix": they
  now assert that the new wire resolution correctly matches
  Agent / Team / Public grants and removes them, and that the
  cross-kind guard still holds. Two new tests
  (`test_resolved_subject_missing_subject_errors` and
  `test_resolved_subject_legacy_wire_shape_serde_round_trip`) cover
  the error path and the JSON round-trip.

### Fixed (issue #16) — `peko agent permit` / `pevoke` propagate to PekoHub within ~1s

`peko agent permit <agent> <user> chat` and `peko agent revoke <agent>
<user> chat` now push a fresh `exposure_update` to PekoHub immediately,
instead of silently waiting for the daemon to restart (or for the
agent to be re-created / the tunnel to reconnect). Previously the
grant was persisted to `~/.peko/agents/<name>/config.toml`, but
PekoHub's `canChat` ACL — and the runtime's defense-in-depth
`instance_state.allowed_users` cache — both read from the last
`instance_announce`, so a granted user was denied (or a revoked user
could keep chatting) until the daemon restarted. The revoke path
was the more dangerous half: a *security* failure disguised as a
feature.

- New `TunnelDispatcher::refresh_instance_allowed_users(agent_name)`
  in `src/tunnel/dispatcher.rs` re-derives `allowed_user_ids` from
  the live `AgentConfig.permissions` and re-announces the instance
  to PekoHub, but only if the agent's current exposure is `Private`
  (Public/Unexposed don't carry an `allowed_users` list, and we must
  not silently flip the exposure as a side effect of a permit call).
  No-op if the agent has no cached `instance_state` (tunnel not
  connected) — the next `announce_instances` after `TunnelReady` will
  pick up the latest config.
- `AgentGrantPermission` and `AgentRevokePermission` IPC handlers
  in `src/ipc/server.rs` call `refresh_instance_allowed_users`
  after a successful local config write. The call is best-effort
  and never fails the permit itself; a tunnel outage produces a
  `warn!` log and the next `TunnelReady` round-trip carries the
  new `allowed_users`.
- `TunnelDispatcher::set_instance_exposure` was refactored to
  delegate its tunnel-send step to a new private
  `send_exposure_update` helper, which `refresh_instance_allowed_users`
  also calls — the local state mutation stays in
  `set_instance_exposure` only.
- New `tests/scenarios/s5_live_permit_propagation.rs` regression
  test: starts the daemon, asserts a non-owner user is denied
  (empty `allowedUsers`), runs `peko agent permit` via subprocess,
  asserts the user is allowed within ~1s, runs `peko agent revoke`
  and asserts denial within ~1s, then re-permits and asserts
  re-allowance — all without restarting the daemon. PekoHub's
  `instance.allowedUsers` is also asserted to contain the grantee.
- `peko agent permit --help` and `peko agent revoke --help` help
  text now state the propagation behaviour explicitly.
- The "known production gap" note in
  `tests/scenarios/s4_publish_running_agent_with_permission.rs:68-82`
  is removed and replaced with a pointer to s5 + the issue.

### Fixed (issue #14) — manifest signature verification on import

**`.agent` signature is now verified on unpack.** The packager has always
signed the manifest with the agent's ed25519 DID key on write
(`Packager::sign_manifest` at `src/portable/packager.rs:492`), but the
unpackager never called any verify function on read. A tampered `.agent`
from a registry or mirror would import successfully and the runtime's
per-author trust assumption would be silently broken — the headline
"secure portable agent" claim was false. This change closes the gap.

- New `src/portable/signature.rs` module with
  `verify_manifest_signature(manifest_bytes, did_doc_bytes, allow_unsigned)`.
  Verifies the ed25519 signature in `signatures.manifest` against the
  public key embedded in the package's `identity/did.json`, using the
  same canonical byte reconstruction the packager signs
  (manifest with `signatures.manifest = ""` and `signatures.algorithm = "ed25519"`,
  re-serialized via `to_toml`).
- `Unpackager::import_from_files` now calls signature verification
  *unconditionally* — before `validate_package` — and returns the
  stable error code `[signature_verification_failed]` (with the
  `SignatureError` reason in the message) on failure.
- `--force` no longer bypasses signature verification. Signature is a
  security guarantee, not a format check, and was previously lumped in
  with `validation.is_valid()` under the same `--force` umbrella.
- New `--allow-unsigned-agent` opt-in flag (default `false`) on
  `peko agent import` and `peko agent pull` for users pulling from a
  source they don't fully trust. An *unsigned* package is permitted
  only with this flag; a *badly signed* package is always rejected.
  The flag is `allow_unsigned: bool` on `ImportOptions` /
  `TeamImportOptions` / `AgentImportOptions` and is also threaded
  through the daemon IPC `RequestPacket::AgentImport { allow_unsigned }`.
- The `InvalidSignature` and `DidResolutionFailed` variants in
  `src/portable/validation.rs` are no longer dead code paths conceptually,
  though the unpackager returns a `SignatureError` directly for richer
  error reasons rather than going through `ValidationError`.

**Surfaces two related determinism bugs** (both real, both caught
by the new tests; both fixed in the same change so the signature
gate is actually usable end-to-end):

- `packaging.checksums` was `HashMap<String, String>`. HashMap
  iteration order is randomized per instance, so the packager and
  a round-tripped manifest could serialize the checksums table in
  different orders, producing different bytes for the same manifest
  and breaking signature verification spuriously. Both
  `AgentManifest::PackagingMetadata` and `TeamPackagingMetadata`
  are now `BTreeMap<String, String>` (sorted by key) so the
  canonical signed bytes are stable across the serde round-trip.
  On-disk wire format is unchanged.

- `packaging.files` (a `Vec<String>`) was being appended to in
  insertion order by `AgentManifest::add_file` (called by
  `Packager::export_identity`, `export_config`, `export_skills`,
  `export_workspace`, `export_sessions`). On the round-trip through
  the registry, `AgentRegistry::export_package` re-builds the file
  list from the layer storage and `.sort()`s it. The two paths
  produced different bytes — the packager's signed bytes had the
  file list in insertion order, the registry's re-serialized bytes
  had it sorted — and signature verification failed after any
  push→pull cycle. `add_file` now keeps `packaging.files` sorted
  at all times via `binary_search` + `insert`, so both paths
  produce identical bytes. New regression test
  `manifest_round_trip_produces_identical_bytes` exercises the
  full serde round-trip and asserts byte equality.

- New tests in `tests/cli_agent_signature.rs` (7 tests, all passing):
  - green: signed manifest imports successfully
  - red: tampered manifest byte fails with `signature_verification_failed`
  - red: stripped signature fails (no silent fallback to "unsigned")
  - red: wrong-key signature fails (signed by A, DID doc claims B's key)
  - red: `--force` does NOT bypass signature
  - green: `--allow-unsigned-agent` permits unsigned import
  - byte-stability regression guard pinning `created_at`

### Fixed (issue #8)

**Tunnel reconnect cap and degraded-state surfacing.** Previously, when
PekoHub was permanently unreachable (DNS, network, decommissioned), the
runtime's tunnel client retried forever, producing unbounded log spam and
no operator signal that the relay was down.

- `TunnelClient` now caps consecutive reconnect attempts via
  `max_reconnect_attempts` (default `50`, ≈ 28 min with default backoff).
  After the cap, the client stops retrying and emits a one-shot
  `TunnelStatusUpdate::Degraded` callback.
- New `TunnelStatusUpdate` enum (`Connected` / `Disconnected` / `Degraded`)
  wired into `AppState::start_tunnel`, which now takes a
  `max_reconnect_attempts` parameter and tracks per-attempt state
  (`tunnel_attempts`, `tunnel_last_error`, `tunnel_degraded`).
- New `AppState::tunnel_health() -> TunnelHealth` enum with four states
  (`disabled` / `connected` / `disconnected` / `degraded`).
- New `peko daemon start --max-reconnect-attempts <N>` CLI flag (default 50).
  Pass `4294967295` (u32::MAX) to effectively disable the cap.
- New IPC `RequestPacket::Status` / `ResponsePacket::Status` packet
  returning tunnel health. `peko daemon status --json` now emits
  `tunnel: { state, reconnect_attempts, last_error, degraded }`.
  `stop_tunnel()` clears the degraded flag and per-attempt state.

## [1.0.0-rc1] - Phase 1 Completion - 2026-05-14

Phase 1 of the Pekobot runtime is complete. All P0 success criteria for the agent runtime, unified packaging, registry integration, and CLI have been implemented and verified.

### Phase 1 Summary

**Runtime Engine:**
- Turn-based agentic loop with streaming (`StreamOrchestrator`), tool execution, and session persistence
- 15+ LLM providers via metadata registry (OpenAI, Anthropic, Kimi, Minimax, Ollama, Azure, Cohere, DeepSeek, Fireworks, Groq, OpenRouter, Perplexity, Together, xAI)
- Configurable timeout per LLM request (default 60s, max 3600s)
- Max 10 iterations per turn, gracefully handles API failures and tool timeouts
- 7 integration tests covering RT-001 through RT-006 in `engine::agentic_loop::tests`

**Packaging (ADR-027):**
- Unified `.agent` format: gzip tar with TOML manifest, SHA-256 checksums, content-addressable layers
- `.team` format: checksum-validated, `team.toml` roundtrip, registry layer deduplication
- `.ext` format: extension bundles for offline distribution
- `AgentRegistry` local content-addressable storage in `~/.pekobot/registry/`

**Registry:**
- `pekobot agent push <local> <remote>` / `pekobot agent pull <registry-ref>`
- OCI-inspired protocol with bearer/basic auth, layer existence checks (HEAD), digest verification
- Python FastAPI mock registry server for integration testing

**Extension Framework:**
- 22 hook points across agent lifecycle (`PromptSystemSection` through `AgentIteration`)
- 6 extension types: builtin, skill, MCP, universal, gateway, general
- Dynamic tool registration/unregistration without restart
- Async task execution framework with event bus and queue

**MCP Integration:**
- stdio and SSE transports
- Tool discovery, schema proxying, reserved parameter injection
- Server lifecycle: start on demand, health-check, restart on failure, graceful shutdown

**Session Management:**
- JSONL storage with atomic writes (tmp + rename)
- Branching (`pekobot session branch`), recovery (`SessionRecovery`), maintenance
- Compaction with dual-threshold triggers and structured summaries

**CLI:**
- Core commands: `agent`, `team`, `ext`, `session`, `send`, `daemon`, `system`
- Top-level config CLI (`pekobot config get/set/validate/init/defaults/path`) — ADR-028
- `--json` support on major data commands
- Shell completions via `clap_complete`
- `PEKOBOT_*` environment variables for all global flags

**Security:**
- API key stripping from subprocesses (`*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`)
- Credential detection in config (partial enforcement)
- DID identity with ed25519 keys

**Test Coverage:**
- 1,024 unit tests passing, 0 failed, 19 ignored
- 60+ PowerShell E2E tests covering agent, session, send, tools, extensions, packaging, cron, A2A, subagent, compaction
- 0 compiler warnings, 0 clippy warnings

### Deferred to Phase 2
- `system doctor` / `system clean` (stubs remain)
- `pekobot validate` command
- `--json` on remaining commands
- MCP Streamable HTTP transport
- Performance benchmarks with baseline data
- Package signing & encryption
- Extension source references (GitHub, URL, MCP endpoint)
- OpenTelemetry export
- Public registry web UI

---

## [0.1.0] - Team Registry Layer Deduplication (Issue 023) - 2026-05-11

Team registry push/pull now uses content-addressable layers instead of a single opaque blob, enabling cross-team agent deduplication.

### Added
- **`LayerType::TeamConfig`** — New layer type for team metadata (agent index) in registry manifests
- **`TeamAgentIndex`** / **`AgentLayerRef`** — Types for the agent → layer digest mapping inside `TeamConfig` layers
- **`TeamLayerBuilder`** (`src/portable/team_layer_builder.rs`) — Decomposes `.team` archives into content-addressable layers
- **`TeamLayerReconstructor`** (`src/portable/team_layer_reconstructor.rs`) — Reconstructs agents from registry layers for direct in-memory import
- **E2E test** — `e2e_tests/packaging/team_registry_dedup.ps1` — Verifies cross-team agent deduplication on mock registry

### Changed
- **`handle_team_push`** (`src/commands/team.rs`) — Now decomposes team into `TeamConfig` + per-agent standard layers (`Config`, `Identity`, `Skills`, etc.) instead of storing a single opaque blob. Shared agents across teams are automatically deduplicated via `RegistryClient::check_existing_layers()`.
- **`handle_team_pull`** (`src/commands/team.rs`) — Now reconstructs agents directly from registry layers without creating a temporary `.team` file. Imports each agent via `Unpackager::import_from_files()`.
- **`LayerType`** — Now implements `Hash` (required for use as `HashMap` key in layer builders)

### Integration Tests
- `portable::team_layer_builder::tests` — 9 tests (basic decomposition, empty team, all layer types, shared content, digest determinism)
- `portable::team_layer_reconstructor::tests` — 6 tests (roundtrip, missing optional layers, empty index, error handling)

---

## [0.1.0] - Packaging System (Phases 1–7) - 2026-05-08

Unified packaging layer with content-addressable storage, registry push/pull, and integrity checks.

### Added
- **`src/portable/`** — Unified packaging layer (merged from `src/image/`)
  - `AgentBuilder` — Build `.agent` packages from source directories with content-addressable layers
  - `AgentRegistry` — Local content-addressable store for layers and manifests
  - `Packager` / `Unpackager` — Export/import `.agent` packages
  - `TeamPackager` / `TeamUnpackager` — Export/import `.team` packages with SHA-256 checksums
  - `ExtensionPackager` / `ExtensionUnpackager` — Export/install `.ext` packages
- **Registry client** — OCI-inspired HTTP push/pull with layer existence checks (HEAD)
- **Mock registry server** — FastAPI-based mock for integration testing ~~(`e2e_tests/mock_registry/`)~~ *(was `e2e_tests/packaging/mock_registry/main.py`; both deleted in Phase A. The Rust integration tests now exercise the real pekohub fixture server at `pekohub/backend/tests/fixtures/server.ts`.)*
- **CLI commands**
  - `pekobot agent build <path> -t <tag>` — Build `.agent` from directory
  - `pekobot agent push <tag>` — Push to registry
  - `pekobot agent pull <ref>` — Pull from registry
  - `pekobot ext export <id> -o <path>` — Export extension to `.ext`

### Changed
- **`AgentManifest` clean manifest** — Stripped of `capabilities`, `tools`, `mcp`, `tool_sources`, `memory`. Packaging metadata only. `agent.toml` is the single source of truth.
- **`src/image/` deleted** — All functionality merged into `src/portable/`

### Removed
- `AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig` — Superseded by extension framework

### Integration Tests
- `tests/build_integration.rs` — 3 tests (valid build, missing config, layer deduplication)
- `tests/registry_integration.rs` — 4 tests (manifest roundtrip, blob roundtrip, push+pull, layer skip)
- `tests/team_integration.rs` — 4 tests (checksums, import validation, checksum mismatch, legacy warn)
- `tests/extension_packaging.rs` — 5 tests (export, manifest, install roundtrip, missing ext, checksum mismatch)
- `tests/packaging_integration.rs` — 3 tests (full pipeline, build→import roundtrip, clean manifest verification)

---

## [0.1.0] - Documentation Reorganization - 2026-04-11

Major documentation update to reflect the Unified Extension Architecture (ADR-017) implementation.

### Documentation Restructure ✅

**New Structure:**
- `docs/executive/` - Executive summaries and overviews
- `docs/architecture/` - Technical architecture (OVERVIEW.md, EXTENSION_SYSTEM.md, ADRs)
- `docs/planning/migration/` - Consolidated migration guides
- `docs/archive/` - Historical and superseded documents

**Key Updates:**
- **EXECUTIVE_SUMMARY.md** - Updated, now reflects unified extension architecture with 22 hook points
- **API_SURFACE.md** - Updated, documents new Extension Core and Extension Manager APIs
- **Architecture Overview** - New document documenting post-ADR-017 architecture
- **Extension System Guide** - New comprehensive guide for the unified extension system
- **Migration Guide** - Consolidated migration documentation

### Archived Documents ✅

Moved to `docs/archive/`:
- UNIFIED_ARCHITECTURE_SPEC.md (superseded by new architecture docs)
- ASYNC_INFRASTRUCTURE_COMPARISON.md (historical analysis)
- LEGACY_CODE_AUDIT.md
- PHASE1_ROADMAP.md (retired)

### API Changes

**New APIs:**
- `ExtensionCore` - Central hook registry with 22 hook points
- `ExtensionManager` - Unified extension lifecycle management
- `HookHandler` trait - Extension implementation interface
- `ExtensionTypeAdapter` trait - Type-specific extension adapters

**Removed APIs:**
- `MessageService` (replaced by `StatelessAgentService`)
- `AgentManager` (replaced by `StatelessAgentManager`)
- `SessionResolver` (merged into `SessionManager`)
- `AgentCreationService` (merged into `AgentService`)

---

## [0.1.0] - Phase 1 - 2026-03-18

Phase 1 establishes the **Core Runtime** including agent image/instance model, daemon with HTTP API, session management, built-in tools, team composition, and event bus.

### Milestone 1: HTTP API Server Foundation ✅

**Core infrastructure for the daemon HTTP API.**

- Created `src/api/` module with Axum-based HTTP server
- Implemented `GET /health` and `GET /info` endpoints
- Implemented `X-Pekobot-Version` and `X-Request-ID` headers
- Standard error envelope: `{error: {code, message, request_id, details}}`
- API request/response types with validation
- Graceful shutdown handling

### Milestone 2: Agent Image and Instance Model ✅

**Image/instance distinction with filesystem-first agent definition.**

- `src/image/` module for image manifest management
- `config.toml` loader with validation
- `POST /images/build` with SHA-256 digests
- `.pekobot/registry/images/` content-addressable storage
- Instance pinning to image digest at creation time
- Full instance lifecycle API (`POST /agents`, `GET /agents`, `DELETE /agents`)
- Sessions excluded from images

### Milestone 3: Session Management ✅

**Durable JSONL sessions with atomic writes.**

- Atomic JSONL writes (tmp + rename)
- All 13 event types in JSONL format
- `.index.json` sidecar generation
- `GET /agents/{id}/sessions` and history endpoints
- `POST /agents/{id}/sessions/{id}/branch`
- Session state recovery on daemon restart
- Auto-generated titles from first assistant response

### Milestone 4: Core Runtime and Agentic Loop ✅

**Turn-based agentic loop with sync/async tool calling.**

- `AgentInput` enum: UserMessage, HookTrigger, A2AMessage
- Synchronous tool execution via `TaskManager`
- Asynchronous tool execution via `UnifiedAsyncExecutor`
- Tool timeout handling (120s default)
- Tool panic isolation with `catch_unwind`
- `POST /agents/{id}/chat` with SSE streaming
- WebSocket chat endpoint `ws://localhost:11435/agents/{id}/ws`
- Watch mode (`--watch`) with file watcher
- All 4 LLM providers: Anthropic, OpenAI, Ollama, OpenAI-compatible

### Milestone 5: Built-in Tools Completion ✅

**All 13 required built-in tools with sandboxing.**

| Tool | Description |
|------|-------------|
| `filesystem` | read, write, list, exists, delete, move with path sandboxing |
| `process` | Execute commands with shell blocking, env var stripping |
| `apply_patch` | Atomic file patches with rollback |
| `agent_spawn` | Spawn subagents (sync/async) |
| `agent_spawn_status` | Check subagent status |
| `agent_spawn_list` | List spawned agents |
| `agents_list` | Team-scoped agent listing |
| `agent_info` | Get agent information |
| `sessions_send` | Send messages (with cross-team blocking) |
| `sessions_list` | List sessions |
| `sessions_history` | Get session history |
| `session_status` | Check session status |
| `cron` | 7 sub-commands: at, every, cron, idle, event, list, cancel |

- Path sandboxing enforced (filesystem, apply_patch, process cwd)
- Process env var stripping (`*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`)
- Shell blocking (sh, bash, zsh, cmd, powershell, pwsh)
- `disabled_tools` config support

### Milestone 6: Custom Tools and MCP Integration ✅

**Custom tool discovery and MCP client support.**

- `tools/` directory discovery
- Custom tool JSON protocol (stdin/stdout)
- Optional `<toolname>.json` schema sidecar
- MCP client in `src/mcp/`
- `mcp.json` parsing
- MCP tool discovery (`list_tools`)
- MCP tool call proxying
- MCP server startup failure handling
- Capability resolution order: built-in → local → MCP

### Milestone 7: Team Runtime and Event Bus ✅

**Multi-agent teams with shared services and A2A communication.**

- `team.toml` parser
- `src/team/` module for team management
- `POST /teams` (deploy from config)
- `GET /teams`, `GET /teams/{id}`, `DELETE /teams/{id}`
- In-memory event bus backend
- All 5 A2A message types: Direct, Task, TaskResult, Broadcast, Subscribe
- Shared file workspace
- Shared MCP server reference counting
- `POST /teams/{id}/scale`
- Unified runtime (no separate team runtime)

### Milestone 8: Outbound Hooks and System Events ✅

**Cron, webhook, event, and file_watch hooks.**

- Cron implementation with spec compliance
- `cron.json` persistence
- Missed job handling on restart
- Webhook server in orchestration layer
- `POST /webhooks/{instance_id}/{token}`
- Webhook token validation (constant-time comparison)
- File watcher hook
- Event-triggered hook (event bus integration)
- System event stream `ws://localhost:11435/events`
- Lifecycle events on system stream

### Milestone 9: Registry and Image Distribution ✅

**Image packaging, push/pull, and registry client.**

- OCI-inspired packaging in `src/portable/`
- Layer compression (gzip tar)
- Content-addressable layer storage
- `POST /images/pull` with streaming progress
- `POST /images/push` with streaming progress
- Registry client with bearer token auth
- Registry client with HTTP Basic auth
- Multiple registry sources in `runtime.toml`

### Milestone 10: CLI Completion and Interfaces ✅

**Complete CLI commands and Web UI.**

- CLI uses HTTP API (not direct calls)
- All commands non-interactive
- `--output json` for list/show commands
- Proper exit codes (0 success, non-zero error)
- `pekobot init ./agent/` command
- `pekobot session show <session-id>`
- Web UI embedded HTML at `/ui`
- WebSocket service endpoint
- `--debug` flag for stack traces

### Milestone 11: Security and Hardening ✅

**All security requirements and sandboxing.**

- Process tool strips sensitive env vars (`SENSITIVE_ENV_PATTERNS`)
- Credentials never appear in sessions/logs
- `config.toml` credential detection
- Filesystem path traversal rejection
- Symlink handling in sandbox
- Localhost-only default binding with warning
- Audit logging for all agent actions
- No credential leakage in API responses
- 831 tests passing including 48 security tests

### Milestone 12: Performance Optimization and Testing ✅

**Performance targets and end-to-end use cases.**

- Performance benchmarks (`benches/m12_performance_benchmarks.rs`)
- Performance measurement infrastructure (`PerformanceMetrics`, `LatencyStats`)
- `GLOBAL_METRICS` singleton
- Performance hooks in critical paths
- Metrics API endpoint (`GET /metrics/performance`)
- Use case tests for UC-001 through UC-005
- Concurrent instance stress test (50 instances)
- Comprehensive test coverage for M12 components

**Performance Targets:**
| Metric | Target | Status |
|--------|--------|--------|
| Cold Start | < 500ms | Framework Ready |
| Warm Start | < 100ms | Framework Ready |
| First Token | < 500ms | Framework Ready |
| Tool Latency | < 5ms | Framework Ready |
| Concurrent Instances | 50 stable | Framework Ready |
| Team Deploy | < 30s | Framework Ready |

### Milestone 13: Documentation and Polish ✅

**Complete documentation and Phase 1 finalization.**

- Updated Getting Started guide (`docs/getting-started/GETTING_STARTED.md`)
- Error codes reference with fix suggestions (`docs/reference/ERROR_CODES.md`)
- `--help` examples for all CLI commands
- API usage examples (`docs/api-examples.md`)
- Contributor guide (`docs/dev/CONTRIBUTOR_GUIDE.md`)
- Phase 1 CHANGELOG (this file)
- Review and documentation of [SHOULD] item deferrals

---

## Deferred Items (Phase 2/3)

The following items from the specification are explicitly deferred:

| Item | Phase | Reason |
|------|-------|--------|
| Control Plane (lifecycle policies, scheduling) | Phase 2 | Runtime foundation needed first |
| Resource enforcement (cgroups) | Phase 2 | Requires control plane |
| Capability package manager (`pekobot install`) | Phase 3 | Ecosystem maturity needed |
| Auto-install from dependencies | Phase 3 | Requires package manager |
| Redis/NATS bus backends | Phase 2 | In-memory sufficient for single-node |
| Session plugins | Phase 2 | Can use raw sessions initially |
| Package signing | Phase 2 | Verification warning mode acceptable |
| TUI (`pekobot-tui`) | Phase 2 | Web UI sufficient for Phase 1 |
| Base image inheritance | Phase 2 | Can use explicit config copying |
| Session branching UI | Phase 2 | API exists, CLI can be added later |

---

## Statistics

- **Total commits:** ~500+
- **Lines of code:** ~50,000
- **Test coverage:** 80%+
- **Documentation pages:** 15+
- **Milestones completed:** 13
- **Duration:** 21 weeks

---

## Contributors

Thank you to everyone who contributed to Phase 1!

---

## License

MIT License - See [LICENSE](../LICENSE) for details.
