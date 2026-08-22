# The PEKO Primitive

**Status:** Canonical term for the Agent–Session Paradigm; codebase-audited 2026-08-15
**Date:** 2026-08-15
**Related:** [AGENT_SESSION_PARADIGM.md](AGENT_SESSION_PARADIGM.md),
[ADR-039](adr/ADR-039-principal-model.md) (principal model),
[ADR-041](adr/ADR-041-principal-as-container.md) (principal-as-container),
[ADR-042](adr/ADR-042-no-external-session-concept.md) (no external session concept),
[ADR-044](adr/ADR-044-chat-session-separation.md) (chat/session separation)

---

## Definition

**PEKO** *(n.)* — **P**ersistent **E**ntity with **K**eepalive **O**rchestration. The architectural primitive of peko: one PEKO per principal profile, defining how that principal exists, organizes, and continues to act.

The full design rationale lives in
[`AGENT_SESSION_PARADIGM.md`](AGENT_SESSION_PARADIGM.md). PEKO is the
canonical name for that paradigm — the term to use in code, comments,
commit messages, and design discussions.

---

## What a PEKO is

A self-organizing tree of dual-aspect nodes owned by one principal. Each
node is a single entity viewed two ways — as an *agent* (a live
LLM-driven run) and as its *session* (a persistent JSONL record on
disk). The root of the tree is cron-keepalived; it persists indefinitely,
supervises its children, and turns the principal into an active actor
rather than a passive request handler.

---

## Properties

### P — Persistent

- Every node carries a stable `session_id` for life. No id rotation, no
  chapter concept.
- State is JSONL on disk; pages auto-rotate when one exceeds
  `rotate_bytes` (e.g. `<id>.0.jsonl`, `<id>.1.jsonl`, …).
- **Standing nodes are prune-exempt**: the idle-prune filter skips
  `root:*`, `archived`, and `standing`-flagged sessions.

### E — Entity (one node, two views)

- An agent *is* its session. Same entity, two faces.
- **Generative face**: the `Agent` tool — `new`, `resume`, `compact`.
- **Persistent face**: the `session` tool — `status`, `list`, `history`,
  `search`, `rename`, `delete`, `branch`, `archive`, `unarchive`, `move`.
- Both tools operate on the same underlying node. The duality *is* the
  paradigm.

### K — Keepalive

- The root session receives periodic cron turns that keep it alive and
  continuously active. Cron `Send` defaults to the trunk; `SpawnTool`'s
  `wake_on_completion` steers the trunk inbox. Trunk-targeted `Every`
  sends enforce a 60s floor (`TRUNK_MIN_INTERVAL_MS`).
- External ingress never reaches the root: CLI/A2A/Hub DMs land in
  per-peer children, bound channels in their bound child (see §O).
- Without keepalive, the root idles, standing children become orphaned,
  and the principal becomes a passive request handler.
- The keepalive is **per-principal** and serves double duty: liveness
  signal + the principal's own supervision tick. It is self-regulating —
  the trunk agent holds the cron tools and can create/delete its own
  trunk-targeted jobs to adjust its cadence.

### O — Orchestration

- Children are organized hierarchically via `parent_session_id`.
- Each child carries a per-parent-unique **slug** (1–64 chars, no `/`,
  no `:`, no outer whitespace). `:` is reserved for raw session ids
  (the tree root shape `root:<dim>:<name>` and the runtime-extension
  prefixes `spawn:<uuid>:` / `channel:<id>:`), so slugs cannot collide
  with raw ids by construction.
- Reach is by **slug path only** on the LLM-facing surface, in one of
  two forms:
    - `/a/b/c` — absolute path anchored at the caller's topmost ancestor.
    - The caller's own session id (UUID) — engine self-reference; every
      other shape is **REFUSED** with a structured error pointing the
      caller at the `path` field in `session list`.
  Path resolution anchors at the caller's topmost ancestor and walks
  by slug; unknown segments produce structured errors listing
  available children. Engine-internal call sites (`resume_preflight`,
  `request_compaction`, `validate_context_parent`) receive canonical
  UUIDs from the runtime itself and canonicalize via
  `SessionId::from` — there is no engine-internal heuristic resolver
  (`resolve_id_or_path` retired in sprint 6 commit 2). The session
  layer is intentionally id-shape-agnostic: engine-internal ids are
  opaque UUIDs (sprint 6 commit 1) and peer identity lives at the
  channel layer, not in the session id.
- `session list` defaults to the **caller's subtree** (not the whole
  principal's tree). Privileged trunk callers opt into a wider view
  with `scope: "principal"`; non-privileged callers who ask for the
  wider scope get ownership-clamped to their subtree with a structured
  warning. The `path` parameter scopes further to any subtree the
  caller has ownership access to.
- **Subtree scoping**: a node manages only its own subtree — never
  siblings, never ancestors, never the protected `root:*` family.
- **Standing children** are declared in `principal.toml` `[children]`,
  ensured at root setup, and attach by name on `Agent new` rather than
  minting a fresh UUID.
- **External-facing children** are auto-spawned on contact
  (`principal/peer_children.rs`): each DM peer gets a per-peer standing
  child (`/local-user`, `/user-x`, `/principal-{did-fragment}`) parented
  at the trunk; bound channels wake their bound child. External traffic
  NEVER lands in the trunk — the root is cron-only (Phase 7, sprint 2).
  The owner's child (`/local-user`) is `privileged`: whole-store reach in
  the ownership guards; strangers' children stay subtree-scoped.
- **Move** reparents a session and its subtree with a cycle guard
  (`err_move_cycle`); refused on `root:*` source, live-run target, and
  ancestor descent.
- **Channels are external I/O**, not children — read via `channel read`,
  posted via `channel send`, stored in a separate append-only event
  log, never part of session JSONL. Since sprint 3 (Phases 10–13)
  this holds for DMs too: every peer has a per-peer DM channel, and
  that channel's event log IS the consumer-visible conversation
  record `peko log` reads (the chat-log projection store is
  retired). **Sprint 4 unifies the write surface**: `ChannelSend` is
  one tool whose `channel` parameter's wire form
  (`chan_<8 base36>` / `principal:<did>` / `user:<id>` / `group:<slug>`)
  selects the dispatch — bare post, principal RPC (ensure_peer_child
  + await reply + mirror), peer messenger note, or group post. The
  parallel `send_peer` tool retired outright; the design promise
  "`send_peer` and `ChannelSend` share one delivery mechanism" is
  now literal.

---

## Boundaries

### In scope

- The principal's root session (continuous, cron-keepalived).
- Standing children declared in `principal.toml`.
- Spawned children and their subtrees.
- The principal's memory organization (per-`principal.toml`).
- Cron jobs targeting the principal.

### Out of scope

- External peers — modeled as channels, not children.
- Other principals' PEKOs — each principal owns exactly one.
- Channel logs — separate append-only event log; not session JSONL.
- The consumer-facing `peko log` view — reads the peer DM channels;
  not part of the PEKO.

### Cardinality

- **One PEKO per principal.** A peko runtime may host many PEKOs
  simultaneously (one per principal profile), isolated by storage tier
  (Local / Shared / Runtime).

---

## Distinctions (what a PEKO is *not*)

- **Not a single session** — the structure that *contains* sessions.
- **Not an agent** — the structure that *contains* agents.
- **Not `principal.toml`** — config in, PEKO out; the PEKO is the
  runtime instance derived from the principal's config.
- **Not channels** — channels are the PEKO's external interface, not
  part of the PEKO itself.
- **Not a request handler** — the principal is an actor; the PEKO is
  its continuous existence.

---

## Contract violations (anti-patterns)

Any change that breaks one of the four properties is a **PEKO contract
violation** and should be rejected in review.

### Violates P (Persistence)

- Reassigning or rotating a node's `session_id` after creation.
- Deleting JSONL pages while the session is still live.
- Letting the idle-prune filter sweep a `standing`-flagged or `root:*`
  session.
- Treating the session JSONL as an ephemeral cache (e.g. truncating on
  daemon restart).
- Re-introducing the "chapter" / id-rotation concept from §7.5.

### Violates E (Duality)

- Storing agent config, prompts, or tool state outside the session
  JSONL (e.g. a sibling config file the agent needs to be "rehydrated"
  from on resume).
- The `Agent` tool and the `session` tool operating on different
  underlying nodes for the same id.
- Spawning a session that lacks one of the two faces (e.g. a session
  with no path to an LLM run, or an LLM run with no session record).
- In-memory spawn overlays (`peko-rs/session/src/manager.rs:1903`)
  accepted only as a tolerated-on-restart edge case — moving the spec
  out of the session is a violation.

### Violates K (Keepalive)

- Cron `Send` or `SpawnTool` wake targeting any session other than the
  principal's root.
- No cron firing into the root at all — turns the PEKO into a passive
  request handler.
- External ingress (CLI send, A2A, Hub, bound channels) landing in the
  trunk instead of a child — the trunk's context is the supervision
  loop's working memory, not a receptionist's. (Pre-Phase-7 the per-peer
  `root:{peer}` sessions violated this; they are retired.)
- Two "roots" for one PEKO (the retired §7.4 disagreement: `Send` in
  `root:cron:{owner}` while `SpawnTool` woke `root:{owner}`).
- Cron `Every{every_ms}` with no minimum-interval floor on
  self-targeted keepalive (runaway-token-burn anti-pattern; enforced by
  `TRUNK_MIN_INTERVAL_MS`).

### Violates O (Orchestration)

- A spawned agent accessing, modifying, or deleting a sibling's
  subtree.
- A spawned agent accessing, modifying, or deleting an ancestor.
- Any mutation of the `root:*` family (delete, archive, reparent under
  a non-root).
- A `move` whose destination is the target itself or one of its
  descendants (cycle).
- A `move`, `delete`, or `branch` against a live-run target without the
  refuse-and-retry protocol.
- A path resolution that lands outside the caller's topmost ancestor
  (i.e. climbing above `root:*` is impossible — paths anchor there).

### Violates cardinality

- Two PEKOs sharing the same root session id.
- One PEKO spanning two principals (no cross-principal subtree
  operations).
- A principal's root session owned by another principal's PEKO.

### Violates channel/session separation

- Channel events written into session JSONL.
- Session events posted to a channel as if they were channel messages.
- A passive-binding responder that posts a reply, then processes its
  own reply next tick (no self-post suppression — anti-loop invariant
  broken).
- Cursor durability gap: subscribers that re-fire a channel's full
  history on every daemon restart (§7.3 fixed; regressing it is a
  violation).

---

## See also

- [AGENT_SESSION_PARADIGM.md](AGENT_SESSION_PARADIGM.md) — full design
  rationale, gap audit, latent issues, build order.
- [adr/](adr/) — ADR-039, ADR-041, ADR-042, ADR-044 (canonical
  architectural decisions this primitive depends on).