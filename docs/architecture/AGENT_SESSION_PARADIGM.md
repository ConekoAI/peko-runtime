# The Agent–Session Paradigm

**Status:** Design direction — partially implemented; codebase-audited 2026-08-15
**Date:** 2026-08-15
**Related:** [ADR-039](adr/ADR-039-principal-model.md) (principal model),
[ADR-041](adr/ADR-041-principal-as-container.md) (principal-as-container),
[ADR-042](adr/ADR-042-no-external-session-concept.md) (no external session concept),
[ADR-044](adr/ADR-044-chat-session-separation.md) (chat/session separation)

This document describes the target mental model for peko: what an agent
*is*, how a principal is organized, and how principals talk to the outside
world. It is the reference for collaborators proposing changes to sessions,
subagents, channels, or cron. Sections marked **(implemented)** describe
shipped behavior with code pointers; sections marked **(gap)** describe
work that does not exist yet. §7 lists latent issues the 2026-08-15 audit
surfaced that threaten the paradigm and should be fixed regardless.

---

## 1. Core claim: agent = session

An **agent** and a **session** are two views of the same thing:

- The **agent** is the *generative* side: it uses an LLM to create or
  continue a session.
- The **session** is the *persistent* side: it is the stored existence of
  the agent — JSONL on disk, paged in place, stable id for life.

Neither exists meaningfully without the other, so the spec treats the
words as interchangeable. A **run** is a live agentic process attached to
exactly one session.

**(implemented)** The built-in tool surface already reflects the two sides
of the coin:

- The `Agent` tool (`peko-rs/core/src/tools/builtin/messaging/agent.rs`)
  is the LLM side — three actions: `new` (spawn), `resume` (re-attach a
  run to an existing spawned session), `compact` (flag the session; the
  engine summarizes at the target's next run).
- The `session` tool (`peko-rs/core/src/tools/builtin/session/tool.rs`)
  is the storage side — nine non-LLM actions: `status`, `list`,
  `history`, `search`, `rename`, `delete`, `branch`, `archive`,
  `unarchive`.

**(implemented)** Sessions are JSONL with automatic paging: when a page
exceeds `rotate_bytes` it becomes `<id>.N.jsonl` and readers stitch pages
transparently. Ids are stable for life (the chapter/id-rotation concept
was deleted).

**(gap, design note)** Taken literally, agent = session implies that agent
identity (model, tools, persona) is *session-derived* — the session's
first events should be the agent spec, so "resume an agent" and "continue
a session" become the same operation. Today agent config still lives
outside the session; spawn overlays stamp it at creation time (in-memory
only, `peko-rs/session/src/manager.rs:1903` — tolerated on restart, but
not a spec-of-record). The coin model holds mechanically, but full
interchangeability requires config to be recoverable from the session
itself.

## 2. Principal = an organized tree of agents

A **principal** is an organized collection of collaborating agents —
structurally, a **tree of sessions**:

- The **root `/`** is a forever-continuous session owned by the principal
  itself. It auto-compacts, and cron fires turns into it to keep the
  principal an *active actor* rather than a passive request handler: it
  manages sessions, organizes memory, and supervises all children.
- **First-level children** are spawned by root to manage standing aspects
  of the principal: `/memory`, `/project-a`, `/about-user`, `/my-persona`.
- **External-facing children** are spawned in response to outside contact:
  `/local-user`, `/user-a`, `/user-b`, `/channel-a`.

### 2.1 The trunk `/`

**(implemented)** Sessions form a tree via `parent_session_id` pointers
(`peko-rs/session/src/metadata.rs:40`). The principal's root session
family (`root:*`) is continuous and engine-managed: delete/archive on it
is refused, and no caller may mutate the session it is running in.

**(gap) No principal trunk.** Audit findings:

- Today's root is *per-peer*: exactly two key patterns exist,
  `root:{peer}` and `root:cron:{peer}`
  (`peko-rs/core/src/principal/routers/root.rs:39-62`). There is no
  principal-self session — `Subject` has `User`/`Principal`/`Public` only,
  no self variant. Even the owner chatting via CLI lands in
  `root:user:local` — the owner is modeled as an *external peer*.
- Cron `Send` hard-resolves `peer = config.owner` and forces
  `ChannelKind::Cron`, so the turn lands in `root:cron:{owner}`
  (`peko-rs/core/src/daemon/cron_engine/mod.rs:617-675`). No "no peer"
  branch exists anywhere in the cron engine.
- Touch points for adding a trunk: the session-id constructor and channel
  branch (`routers/root.rs:39-62`), the cron engine's peer resolution
  (`cron_engine/mod.rs:629-650`), `PrincipalManager.receive`'s
  peer-mandatory signature (permission check + peer-keyed memory recall,
  `principal/manager.rs:682-709`), chat-log projection (keyed by
  `(principal_did, peer)` — needs a self-thread convention or an explicit
  skip, `manager.rs:1076-1149`), and the messenger's
  `peer_from_session_key` parser (`principal/messenger.rs:43-62`).
- **Id-choice pitfall:** the root-family guard is a literal
  `starts_with("root:")` prefix match
  (`session/session_runtime_impl.rs:383,432`). A trunk ided bare `root`
  would escape delete/archive protection and misparse in the messenger; an
  id like `root:self` inherits all existing guards for free.
- `err_resume_cross_family` (`session/ownership.rs:216`) is dormant
  scaffolding — defined, tested, zero call sites. The "conversation
  family" concept is currently enforced nowhere.

### 2.2 Standing named children

**(implemented, contrary to earlier assumption)** Spawned sessions are
*not* mechanically ephemeral: under the default `cleanup: keep` the child
JSONL, index entry, and metadata persist, and `Agent resume` re-attaches
to the same session indefinitely (`agents/subagent_executor.rs:657-799` —
guards: exists, not self/ancestor, `trigger=="spawn"`, subtree check, not
archived, no active run via `has_active_subagent_run_for_child`). Depth is
recomputed from the persisted parent chain so it survives daemon restarts.

**(gap)** What is actually missing for `/memory`-style standing children:

- **No name.** `SessionMetadata` has only `title: Option<String>` —
  free-form, non-unique (`peko-rs/session/src/metadata.rs:39`). `resume`
  and `compact` take raw UUID session keys (the `:subagent:{uuid}` key
  machinery in `peko-rs/session/src/subagent_key.rs` is dead code; real
  child ids are plain UUIDv4, `manager.rs:1584`).
- **The 30-day prune destroys idle children.** The daemon's hourly
  maintenance deletes every session — index entry, peer references, and
  the JSONL transcript — whose `updated_at` is older than 30 days, with
  **no exemption** for archived, spawned, or even `root:*` sessions
  (`peko-rs/session/src/index.rs:930-934`,
  `DEFAULT_PRUNE_AFTER_DAYS = 30`). A routinely-resumed child survives by
  touching `updated_at`; an idle one is destroyed. See §7.
- **No declaration registry** — nothing in `principal.toml` or elsewhere
  declares "these children are standing entities."
- **Spawn always mints a fresh UUID** — no spawn-or-attach-by-name in
  `SessionManager::spawn_session`, so "spawn once, resume by name" is two
  manual steps.

### 2.3 Scoping: an agent operates only within its own subtree

**(implemented)** An agent may use the `Agent`/`session` tools only inside
its own scope (`peko-rs/core/src/session/ownership.rs`):

- A caller in a base session manages the whole store (root ⇒ the entire
  principal tree).
- A spawned caller manages only its own subtree: `/user-a` can reorganize
  or create inside `/user-a`, but cannot touch `/user-b` or its own
  ancestors.
- Classification walks the parent chain; dangling metadata denies rather
  than promotes. Typed refusals cover self-mutation, ancestor deletion,
  out-of-tree access, live-run targets, and the protected root family.
- Guards are pure functions over a freshly-loaded metadata slice — no
  caches to invalidate (a 30-second `SessionIndex` TTL applies across
  separate manager instances, `peko-rs/session/src/index.rs:53-54`;
  `peek_compact_requested`'s `get_uncached` read-through is the precedent
  when freshness is critical).

### 2.4 Path addressing and `move`

**(implemented — Phase 1a of the paradigm sprint, 2026-08-15)** Session
`move` (reparent) exists: the session tool's 10th action reparents a
session with its subtree via `MetadataController::set_parent` +
`SessionManager::move_session` (`peko-rs/session/src/manager.rs`), guarded
in `SessionManagerRuntime::move_session`
(`session/session_runtime_impl.rs`): not-self, not-ancestor-of-caller,
subtree guard on both endpoints, `root:*` source refused (moving *under*
`root:*` is allowed), live-run refusal (refuse-and-retry — a reparent is
a single metadata write, unlike delete's multi-step permit protocol), and
a **cycle guard** (`err_move_cycle` in `ownership.rs`) refusing any move
whose destination is the target itself or one of its descendants. The
reparent is recorded as a `System` event (`event: "reparent"`,
old → new parent) in the session's JSONL; the header's parent field stays
stale-by-design (never read back — the index is the source of truth for
parentage). No reader caches the parent chain, so a reparent takes effect
on the next guard evaluation.

**(implemented — Phase 1b of the paradigm sprint, 2026-08-15)** Path
addressing exists. Sessions carry an optional **slug** — a
per-parent-unique path segment (`SessionMetadata.slug` /
`SessionEntry.slug`, serde-default, validated: 1–64 chars, no `/`, no
outer whitespace). The resolver (`peko-rs/session/src/path.rs`, pure
functions over a metadata slice, ownership.rs-style) anchors `/` at the
caller's topmost ancestor and walks children by slug; unknown segments
produce structured errors listing the available child slugs. Every
segment must be a slug — raw ids are not accepted as intermediate
segments (a slugless node is addressed by raw id). Set points: session
tool `rename` (optional `slug`), Agent tool `new` (optional `name`),
`branch` (derives `<source-slug>-branch`, uniquified), and `move`
(re-checks uniqueness among destination siblings). Resolution happens at
the tool-runtime boundary for every `session_key`-shaped param
(`/`-prefixed values resolve, raw ids pass through), *before* the
unchanged ownership guards — paths are a computed view; ids stay the
canonical key everywhere. `session list` shows `slug` + computed `path`
(`compute_path` skips slugless ancestors, display-only).

## 3. Channels: the external interface

A **channel** is external to the principal: the communication interface to
users, other principals, or groups of them. A channel's log is **not** a
session log — the channel record is an append-only event log shared by its
members, while session JSONL remains the principal's private working
memory (see ADR-044 for the same separation on the consumer-chat side).

The essential built-in tool set for a working principal is therefore:
**cron + session + Agent + channel read/send**.

### 3.1 Two types of external communication

1. **Passive (DM).** `user-a` sends to the `user-a` DM channel; the agent
   at `/user-a` picks it up automatically, runs an LLM turn, and the
   output streams back to the channel.
2. **Active (group).** In a group channel with multiple principals, no
   principal passively processes messages — if every principal reacted to
   every message (including each other's), the channel would explode into
   a feedback loop. Instead each principal *actively* reads the channel on
   its own rhythm (cron + `channel read`) and deliberately decides when to
   call `channel send`.

### 3.2 What exists today

**(implemented)** `peko-channel` (`peko-rs/channel/`) is the multi-
principal chat container. Agent-facing tools exist: `peko_channel_read` /
`peko_channel_send` (`peko-rs/core/src/tools/builtin/channel/`),
capability-gated, thin over `ChannelPort::peek` / `post`. Channel storage
is a file-backed append-only JSONL event log, separate from session JSONL;
`peko-chat-log` is a third, consumer-facing projection (`peko log`).

**(implemented, correcting "poll-only")** The daemon's `ChannelSubscriber`
polls every 5s, but the store also has a fully wired **push broadcast**:
`ChannelPort::subscribe_events` fires on every append (including
cross-runtime arrivals via `TunnelChannelPort::append_remote_event`), and
is already consumed by the desktop UI stream
(`ipc/handlers/channel.rs:561-632`). A passive binding can be
event-driven, not poll-driven — the `ChannelResponder::consider_response`
trait (`channel/src/responder.rs:36-40`) is the intended "should I
respond?" seam; only `NoopChannelResponder` ships, hardcoded at
`daemon/mod.rs:761`.

**(implemented)** Passive DM pickup exists — but on the tunnel/A2A path,
not channels. An inbound peer message auto-continues the stable
`root:{peer}` session (created if absent) with serial-run queueing and
steering fallback, and the reply returns as a synchronous RPC response
(`tunnel/dispatcher.rs:1246-1422`, `principal/manager.rs:879-946`).

**(gap) Channel → session binding.** No inbound channel event ever creates
or wakes a session; the one hook that could (`ChannelHost::
kickoff_channel_read`, `daemon/state.rs:2424-2457`) is a deliberate no-op
with a design comment. Missing pieces, with what to reuse:

- **Binding storage + tier semantics** — `CreateOpts`/`MetaJson` carry
  only `name` + `tier`, and `Tier::Runtime/Shared` is storage locality,
  *not* DM/group. The paradigm wants something like
  `passive_binding: Option<SessionKey>` on DM-tier channels.
- **A `ChannelKind` variant** — `ChannelKind`
  (`principal/router.rs:200-209`: Cli/Http/Hub/A2a/P2p/Webhook/Cron/
  FileWatch) has no peko-channel variant, so session-id derivation
  (`root_session_id_for_channel`) and chat-log classification wouldn't
  recognize channel-originated turns.
- **Self-post suppression** — a passive responder posts its reply via
  `post`, then observes its own post next tick; it must filter
  `event.author == principal`. Nothing does this today (the Noop responder
  never posts, so the anti-loop invariant has never been tested).
- **Cursor durability at boot** — subscribers are spawned with fresh
  in-memory cursors (`ChannelCursors::load` is never called in production,
  `daemon/mod.rs:763`); a non-noop responder would re-fire the channel's
  entire history on every daemon restart. See §7.
- **Post-boot membership** — subscribers are enumerated once at boot;
  channels joined later get none.
- The turn-driving machinery itself is reusable as-is:
  `PrincipalManager::receive` / `receive_streaming`
  (`principal/manager.rs:879-1023`) is exactly what the tunnel dispatcher
  already drives for inbound peer messages.

**(gap) Two transports, divergent semantics.** Tunnel/A2A is push,
passive, per-peer-session-bound, sync-RPC; channels are pull/poll, active,
group-oriented, unbound. If DM-tier channels gained passive binding, the
tunnel behaviors that must be preserved are: sync request/response with
pending-correlation + timeout, ed25519 envelope verification, hub
directory + transport selection, chat-log projection of both directions,
`root:{peer}` session continuity (or a deliberate migration of it), and
`[notify]` principal-view lines. What becomes redundant: channel-side
mirroring of DM traffic, most of `PeerMessenger` (survives as the
`[notify]` writer + originator walk), and the user branch of `send_peer`
(subsumed by a DM-channel post — the principal branch's sync RPC is not).
The tunnel *transport* itself stays regardless.

## 4. Cron: the principal's heartbeat

Cron is what makes a principal an *active* actor: it provides the rhythm
for root's self-continuation, active channel reads, memory organization,
and child supervision.

**(implemented)** `CronJobAction` (`peko-rs/cron/src/tools/mod.rs`) has
three variants:

- `Send` — fires a full agent turn, isolated in `root:cron:{owner}`; the
  reply is delivered as a note to the conversational root session.
- `Notify` — pure delivery, no agent turn, zero tokens.
- `SpawnTool` — an async tool run; with `wake_on_completion` it posts a
  steering message into the principal's root inbox.

Schedule kinds cover the needed rhythms: `At`, `Every`, `Cron` (with
timezone), `Idle`, and `Event` triggers. Missed intervals are anchored
(no catch-up bursts) and in-flight fires coalesce.

**(gap / inconsistencies the audit found)**

- A heartbeat turn *in the supervisor's own session* is not expressible:
  `Send` isolates into `root:cron:{owner}` (a deliberate 2026-08-07
  field-test decision), while `SpawnTool`'s wake lands in the
  conversational `root:{owner}` (`cron_engine/mod.rs:880`) — the two
  actions disagree about which root is "the" root. The trunk (§2.1)
  resolves this by giving cron a first-class self target.
- **No minimum-interval floor** — `Every{every_ms}` accepts any positive
  value; runaway protection is only failure-count-based (disable after 3
  consecutive failures).
- **(design note) An always-on actor needs a hard wake budget.** A
  self-triggering root will happily burn tokens reorganizing memory at
  3am. The instruments exist — `budget_per_cycle` (mid-stream rolling
  cap), `cost_per_call_max` (spawn pre-flight), per-principal quota — and
  should be wired as the supervision loop's ceiling from day one. Note
  they are *per-principal* (`quota_state.json` next to `principal.toml`);
  nothing attributes spend to a session or subtree (§5).

## 5. Supervision: what root can see and do today

The trunk's supervision loop needs observability into its children. The
audit measured the current tool surface against that need:

| Need | Status | Where |
|---|---|---|
| List children with timestamps, archived flag | ✅ | `SessionInfo.last_activity/archived`, `tools/builtin/session/mod.rs:40-68` |
| Live-run flag on `list` | ✅ | `run_active` ORs `InboxRegistry` permits with the `AsyncTaskRegistry` subagent-run check (`session/session_runtime_impl.rs`, Phase 0 sprint fix) |
| Per-child token usage | ⚠️ partial | per-session `status` only (`UsageStats`, lifetime not windowed); not on `list` |
| `compact_requested` visibility | ❌ | the flag is write-only from the tool surface; a supervisor cannot tell whether it already flagged a session |
| Per-child spend / budget view | ❌ | quota is per-principal (+ peer meters); no session/subtree attribution, and no built-in tool reads quota state at all |
| Compact a child without it running | ⚠️ deferred only | `compact` sets a flag consumed at the target's *next run*; no offline/headless compaction exists |
| Archive a finished subtree | ✅ | with run-permit + subagent-run guards (`session_runtime_impl.rs:376-504`) |
| Wake on child completion | ✅ | subagent announcements to parent inbox; cron `wake_on_completion` |
| Memory organization surface | ❌ | `PrincipalMemory` (`principal/memory.rs`) is only a session-artifact index for routing; preferences/notes are the LLM's job via plain workspace files — no agent-facing memory tool |

## 6. Current state vs. the paradigm — summary

| Paradigm element | Status | Where |
|---|---|---|
| Agent tool = LLM side (`new`/`resume`/`compact`) | ✅ implemented | `core/src/tools/builtin/messaging/agent.rs` |
| Session tool = storage side (10 actions) | ✅ implemented | `core/src/tools/builtin/session/tool.rs` |
| JSONL auto-paging, stable ids | ✅ implemented | `core/src/session/`, `peko-rs/session/` |
| Subtree-scoped Agent/session tools | ✅ implemented | `core/src/session/ownership.rs` |
| Spawned sessions retained + re-resumable (`cleanup: keep`) | ✅ implemented | `agents/subagent_executor.rs:657-799` |
| Passive DM → auto-continued per-peer session | ✅ implemented (tunnel path) | `tunnel/dispatcher.rs:1246-1422` |
| Channel read/send tools | ✅ implemented | `core/src/tools/builtin/channel/` |
| Channel push broadcast (`subscribe_events`) | ✅ implemented (desktop UI only consumer) | `channel/src/store.rs:296-320` |
| Channel log ≠ session log | ✅ implemented | `peko-rs/channel/`, `peko-rs/chat-log/` |
| Cron turn/notify/spawn-tool + idle/event schedules | ✅ implemented | `peko-rs/cron/`, `daemon/cron_engine/` |
| Principal trunk `/` (self session, cron-kept, supervising) | ❌ gap | root is per-peer today; touch points in §2.1 |
| Standing named children (`/memory`, `/about-user`, …) | ❌ gap | mechanics work; naming + registry + prune exemption missing (§2.2) |
| Session `move` (reparent) | ✅ implemented (Phase 1a) | `session/session_runtime_impl.rs` `move_session`, `peko-rs/session/src/manager.rs`; cycle guard `err_move_cycle` (§2.4) |
| Path addressing (`/user-a/task-b`) | ✅ implemented (Phase 1b) | `peko-rs/session/src/path.rs` resolver; `slug` on metadata; resolved at tool-runtime boundary before guards (§2.4) |
| Channel → auto-spawned/bound child (`/user-a`, `/channel-a`) | ❌ gap | seam exists (`ChannelResponder`); binding storage, ChannelKind variant, self-post suppression, cursor durability missing (§3.2) |
| Passive/active as channel tier property | ❌ gap | split is currently tunnel vs. channel; `Tier` is storage locality |
| Per-session/subtree budget attribution | ❌ gap | quota is per-principal; no quota-reading tool |
| Offline (headless) compaction | ❌ gap | `compact` is flag-only, fires at target's next run |
| Supervision loop (root reviews/compacts/archives children) | ❌ gap | observability partially broken (§5); pattern does not exist |

## 7. Latent issues surfaced by the 2026-08-15 audit

Items 1–3 were **fixed in Phase 0 of the paradigm sprint**
(branch `feat/agent-session-paradigm`, 2026-08-15).

1. ~~**The 30-day prune deletes transcripts with no exemptions.**~~
   **Fixed (Phase 0):** the prune filter now skips the `root:*` family,
   `archived` sessions, and sessions with the new `standing` flag
   (`peko-rs/session/src/index.rs`, `SessionIndex::maintenance`).
   Remaining: `MaintenanceConfig.max_sessions = 500` is still declared
   but never enforced (`peko-rs/session/src/maintenance.rs`).
2. ~~**`session list` under-reports liveness.**~~ **Fixed (Phase 0):**
   `run_active` now ORs the `InboxRegistry` permit with the unified
   `AsyncTaskRegistry` subagent-run check
   (`session/session_runtime_impl.rs`, `list_sessions`).
3. ~~**Channel subscribers boot with fresh cursors.**~~ **Fixed
   (Phase 0):** `spawn_channel_subscribers` loads persisted cursors via
   `ChannelCursors::load` (`daemon/mod.rs`); a corrupt file falls back
   to fresh cursors with a warning. First-ever boot still starts from
   offset 0 by design (benign while the responder is Noop).
4. **Cron actions disagree about the root session.** `Send` turns land in
   `root:cron:{owner}`; `SpawnTool` wake messages land in the
   conversational `root:{owner}` (`cron_engine/mod.rs:880`). Trunk work
   (§2.1) should settle this once.
5. **Dead code to reclaim or delete:** the `:subagent:{uuid}` key helpers
   (`peko-rs/session/src/subagent_key.rs` — tests + re-export only) and
   `err_resume_cross_family` (`session/ownership.rs:216` — zero call
   sites). Child ids are plain UUIDs; keep the messenger's defensive
   `:subagent:` trail-stripping for legacy keys.

## 8. Suggested build order

Each step depends on the previous one and is independently shippable:

0. **Fix the paradigm-threatening latent issues** (§7.1–7.3): prune
   exemptions (at minimum `root:*` + a future `standing` flag; decide on
   `archived`), `run_active` correctness, cursor load at boot.
1. **Session `move` (reparent) + slug/path view** — reparent action with
   cycle guard; per-parent-unique slug; `/user-a/task-b` → id resolver
   over the name index.
2. **Standing children registry** — declared first-level children with
   known names; spawn-or-attach-by-name so "spawn once, resume by name" is
   one operation; prune exemption wired to the registry.
3. **Principal trunk `/`** — a `root:self`-style id (inherits the
   existing prefix guards), cron `Send` targeting it, quota and cost
   ceilings wired in as the wake budget, chat-log self-thread convention;
   supervision patterns land here.
4. **Channel passive binding** — DM-tier channels bound to a child session
   (`/user-a`), event-driven via the existing broadcast, with self-post
   suppression and boot-time cursor loading; collapses type-1
   communication onto `peko-channel`.
