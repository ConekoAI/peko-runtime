# ADR-042: No External `session` Concept in the Peko CLI / IPC Surface

**Status:** Accepted
**Date:** 2026-07-04
**Author:** rlsn
**Related:** [ADR-039](ADR-039-principal-model.md) (principal type unification), [ADR-041](ADR-041-principal-as-container.md) (principal-as-container and session blackboxing), [ADR-040](ADR-040-tool-timeout-and-async-refactor.md) (steering-message read surface).

**Note:** This is a clean-slate pre-production design. Backward compatibility with Peko 0.1.0 is intentionally discarded so the codebase and UX remain coherent.

---

## 1. Context

Earlier commits (`docs/user-guide/USERS_GUIDE.md` §Session, ADR-039 §Rationale, ADR-041 §Consequences) deliberately avoided a `peko session` command and asserted session-key byte-stability. As the read surface grows with `peko log` (PR — `feat/principal-log`), the no-session-externally contract is at risk of being lost to a naming accident or a convenience-driven regression. We record it explicitly so a future contributor can't widen it without amending this ADR.

The tension: a Principal is supposed to be an integral actor — a human talking to another human does not ask "which session is this?" — but the runtime's storage layer naturally partitions work by `(principal, peer)` pairs. The session is an implementation noun that gives the principal long-running memory for each peer. Users should never need to name one, list one, or route through one.

## 2. Decision

- **No `peko session` subcommand, ever.** The CLI command tree never carries a `session` noun in any namespace (`peko session`, `peko principal session`, etc.). The user-facing read surface is `peko log` (added in PR `feat/principal-log`).
- **No generic "list sessions" IPC variant.** `RequestPacket` does not contain a peer-keyed session enumeration variant exposed to the CLI. Read access to a peer's thread is gated behind the principal's `Chat` grant plus a peer-privacy match (`caller == target_peer || caller == principal.owner`).
- **`Session` remains an internal storage noun.** The word "session" appears in CLI help and user-facing docs only where it refers to the underlying JSONL shape and the privacy contract — never as something the user names, lists, or selects by id.
- **`peko log` is the only inspection surface.** A peer who wants to know what the principal is doing between two `peko send` calls reads it via `peko log <PRINCIPAL>` (default owner-root view) or `peko log <PRINCIPAL> --peer <self>` (their own thread). Operators who need to inspect raw session files do so via the filesystem directly.

## 3. Consequences

**Positive:**

- The privacy boundary is hard to widen by accident. Any future IPC variant that returns per-peer content must re-implement the `caller == peer || caller == owner` check; a `peko session list` shortcut can't expose peer-keyed session ids to the CLI without a fresh ADR.
- The CLI surface stays small. One read command per peer-thread concept (`peko log`).
- The "Principal is an integral actor" thesis (ADR-041) remains a hard user-facing contract, not just an internal framing.

**Negative:**

- Operators debugging session storage will reach for `peko log` or direct filesystem reads under `<workspace>/memory/sessions/`. There is no `peko session inspect` or `peko session show`. That is the intended trade-off; if a real operator need surfaces, the path is a new ADR that addresses the privacy implications explicitly, not a slip-in `peko session` command.
- Tooling that wants to render a per-peer conversation view must either consume `peko log --json` or read the JSONL files directly. The first is the supported path; the second is fine because session storage format is documented and stable.

## 4. References

- `docs/user-guide/USERS_GUIDE.md` §Session (rewritten in PR `feat/principal-log`).
- `docs/user-guide/CLI_REFERENCE.md` §``log`` — Inspect Principal activity (added in PR `feat/principal-log`).
- `src/commands/log.rs` — CLI implementation, privacy contract enforced server-side.
- `src/ipc/packet.rs` — `RequestPacket::PrincipalLog` / `ResponsePacket::PrincipalLog` wire shapes.
- ADR-039 §Rationale: "Session key byte-stability is a non-negotiable contract."
- ADR-041 §Consequences: "Sessions are blackboxed. Users and external Principals address a Principal; sessions are internal routing/continuity mechanics of the Principal's memory layer."

## 5. Terminology map (canonical reference)

This section is the cross-repo glossary that doc sweeps and reviews in
both `peko-runtime` and `peko-desktop` align against. It codifies the
**public surface vocabulary** derived from this ADR and ADR-041. Add
to it when introducing a new public noun; do not introduce a new term
that contradicts the map.

### Public nouns (use these in user-facing docs and CLI help)

| Term          | Meaning                                                                          | Source                       |
|---------------|----------------------------------------------------------------------------------|------------------------------|
| **Principal** | The top-level runtime actor. Owns identity, memory, capability grants, prompts.  | ADR-039, ADR-041             |
| **Agent prompt** | A thin Markdown file inside a Principal that names a specialization. *Not* a top-level entity. | ADR-041                      |
| **Peer**      | The Subject (`user:<id>`, `principal:<did>`, `public`) on the other side of a thread. The runtime enforces `caller == peer || caller == owner` for read access. | ADR-042 §2                   |
| **Subject**   | The discriminated-union identity form used by auth + privacy code.               | ADR-033, ADR-034             |
| **Extension** | A plug-in capability (tool, hook, provider, skill). Granted via `capability_grant`. | ADR-024, ADR-026             |
| **Workspace** | The on-disk directory a Principal owns; contains `principal.toml`, `memory/`, etc. | ADR-041                      |

### Internal nouns (do **not** surface to users)

| Term            | Meaning                                                                | Source    |
|-----------------|------------------------------------------------------------------------|-----------|
| **Session**     | `(Principal, peer)`-keyed JSONL thread — the storage layer's atomic unit. Used in CLI help only when describing the underlying file format. | ADR-042   |
| **Tool call**   | A single in-flight invocation of an extension tool. Internal telemetry only. | —         |

### Forbidden public-noun forms

These shapes leaked the pre-ADR-041/042 model. Do not introduce them
in user-facing docs, CLI help, or new IPC variants.

- ❌ `peko session …` — no CLI subcommand.
- ❌ `peko agent …` as a top-level command — `agent` survives only as
  `peko principal agent list <PRINCIPAL>` / `peko principal agent show <PRINCIPAL> <AGENT>`.
- ❌ `session_id` / `agent_id` / `session show` / `agent select` in any
  user-facing form (help text, README, guide, MCP-reserved-params).
- ❌ IPC variants named `agent_*` or `session_*`. The post-migration
  names are `principal_*` (see ADR-041 audit C1 / C4).
- ❌ `extension_enable` / `extension_disable` IPC variants — retired
  alongside the legacy `agent_*` surface. Lifecycle is `ext_start` /
  `ext_stop` / `ext_restart` / `ext_status` plus `capability_grant` /
  `capability_revoke`.

### Idioms the docs **may** keep (they are not violations)

- "session memory" / "session storage" / "session JSONL" when
  describing the underlying file format or the Principal's
  per-peer memory layer — these are internal-noun references.
- "agent prompt" / "agent_prompt" — the post-ADR-041 vocabulary for
  the Markdown files inside a Principal. Always paired with "inside
  a Principal" or `peko principal agent list` to make the hierarchy
  explicit.
- "session-router" / "session start" inside *implementation* notes
  (e.g. internal spec docs explaining how the memory layer
  partitions work) — fine, but the user-facing summary should
  defer to "Principal's memory layer" or "per-peer memory".
