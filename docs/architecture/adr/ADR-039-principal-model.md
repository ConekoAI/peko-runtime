# ADR-039: Principal model — unifying User / Agent / Team subjects

**Status:** Accepted (rev. 2026-07-05)
**Date:** 2026-06-18 (original); revised 2026-07-05
**Author:** rlsn
**Related:** [ADR-033](ADR-033-ownership-and-permission-model.md), [ADR-034](ADR-034-runtime-authentication-and-authorization.md), [ADR-035](ADR-035-runtime-pekohub-tunnel-protocol.md), [ADR-041](ADR-041-principal-as-container.md) (successor — renames the type to `Subject`).

---

## 1. Context

At the time of this ADR, the runtime had three places that tried to model "who is this?" but disagreed on the universe of subjects:

- `Peer::{User, Agent}` in `src/session/types.rs:16-21` — no `Team` variant. (The `Peer` enum was removed entirely in the v1 refactor; see ADR-041.)
- `SubjectType::{User, Team, Public}` in `src/auth/ownership.rs:42-49` — no `Agent` variant. (The `SubjectType` enum was removed in issue #30; the wire shape was collapsed to a single `subject` field.)
- `AgentConfig::owner_id: String` in `src/types/agent.rs:40` — free-form, default `""`. (The file path has since moved to `src/agents/agent_config.rs`.)

Each of these was a partial model. The "user" and "agent" concepts were partially modeled in two of them; the "team" concept was half-modeled in one. None of them were first-class.

This ADR introduced the canonical actor type that all three projections collapsed onto.

---

## 2. Decision

Introduce a single canonical actor enum, `Principal`, in `src/auth/principal.rs` (the file has since been removed — the actor enum was renamed to `Subject` in ADR-041 and now lives at [`src/subject.rs:37`](../../src/subject.rs#L37)). All three of the pre-existing partial models collapsed onto it as projections:

```rust
// Original ADR-039 shape. Renamed to Subject and collapsed in ADR-041:
pub enum Principal {
    User(String),   // hub user id (after #17)
    Agent(String),  // agent instance id (did:key or hub-assigned)
    Team(String),   // team id (after #11)
    Public,         // unauthenticated / world-readable
}
```

### 2.1 Design choices

1. **`Peer` became a type alias for `Principal`** (rather than a newtype wrapper). The pre-existing `Peer::User(...)` / `Peer::Agent(...)` constructions kept compiling unchanged; no `From<Principal>` boilerplate at every call site. We accepted the conflation with `Team`/`Public` and protected session keying with `Principal::is_session_peer`. (`Peer` was subsequently removed entirely in ADR-041; the per-peer concept now lives on `Subject::is_session_peer()`.)

2. **`AgentConfig::owner` and `TeamMetadata::owner` became `Principal`** (rather than `String`). A two-field back-compat shim (`owner: Principal` + `owner_id: Option<String>`) accepted both the legacy `owner_id = "string"` form and the new `owner = { kind = "user|agent|team|public", id = "..." }` form. The service layer's `resolved_owner()` folded the legacy `owner_id` into `owner` if `owner` was still the default `Principal::User("")`. (Subsequently, `AgentConfig` was removed entirely along with the `peko agent *` command tree.)

3. **`SubjectType` was collapsed** in issue #30. The IPC `RequestPacket` no longer carries `(subject_id, subject_type)` for back-compat; it carries a single `subject: Subject` field that serializes as `{kind, id}` via the `#[serde(tag = "kind", content = "id")]` derive. The `Subject` enum on the wire matches what the in-memory model uses.

4. **`PermissionGrant.granted_by: Principal`** (subsequently `Subject`). Composes cleanly with the new type. `Subject::from_str` round-trips `user:alice`, `principal:<did>`, `public`, etc. for log lines and audit events. `AuditEvent` first-class caller field is still deferred to a follow-up ADR.

5. **Session key byte-stability is a non-negotiable contract**. The legacy `derive_base_session_key` format (`agent:{a}:peer:{kind}:{id}`) is preserved byte-for-byte for the duration of the migration; this ADR's job is to introduce the canonical actor type without invalidating on-disk sessions. Any future change to the format is a forced migration for every peko-runtime user. (After ADR-041 the session key format was switched from `agent:{a}:peer:{kind}:{id}` to a principal-native format at `<workspace>/memory/sessions/{session_id}.jsonl`.)

6. **Latent-bug fix at `manager.rs:1743`**: the wildcard arm of the parse-spawn-peer match previously defaulted to `Peer::Agent(id)`, inconsistent with the v1 defaults at `manager.rs:1046, 1049, 1052` which default to `Peer::User("default")`. Aligned to `Peer::User(id)` (or `Peer::User("default".into())` if `id` is empty).

### 2.2 Wire compatibility

Issue #30 collapsed the IPC wire to a single `subject: Subject` field on grant/revoke packets. The legacy `(subject_id, subject_type)` shape is no longer on the wire. Pre-#30 PekoHub clients that sent the two-field shape must be updated; the change was kept opaque to pekohub by bridging in the IPC server handler.

---

## 3. Post-ADR-039 evolution

This ADR records a transitional state. Two subsequent changes are recorded in their own ADRs and should be read alongside this one:

1. **ADR-041 renamed `Principal` to `Subject`** to free up the name `Principal` for the container entity. The renamed type lives at [`src/subject.rs`](../../src/subject.rs). The `Principal(PrincipalDID)` variant replaced `Agent(String)`, and `Team(String)` was removed (see ADR-041 §2.2).

2. **The legacy `peko agent *` and `peko session *` command trees were removed**. The `AgentConfig` and `TeamMetadata` types are gone; the `Peer` enum is gone; the `SubjectType` enum is gone. What remains is the `Subject` enum and the Principal container entity.

The "as-built" shape (post-ADR-039 + ADR-041 + issue #30) is:

```rust
// src/subject.rs:37
pub enum Subject {
    User(String),                       // pekohub user or local DID
    Principal(PrincipalDID),            // an AI principal
    Public,                             // unauthenticated
}
```

---

## 4. Consequences

### 4.1 Positive

- **The 3-place partial model is unified.** A single canonical actor type backed every authorization check, session key, and ownership relationship.
- **`#11`, `#17`, per-user extension permissions, agent-as-peer, and cross-runtime a2a all became smaller follow-ups.** They no longer had to invent their own subject model.
- **Cross-kind guard is automatic.** `Subject::User("alice") != Subject::Principal(...)` with the same wire id. The pre-039 string-equality check silently allowed User/Agent subject collisions; the new typed equality is safe.
- **No on-disk session format change at the time.** The v2 key format was byte-stable across the ADR-039 migration.

### 4.2 Negative

- **`PermissionGrant` JSON shape changes in memory** (issue #30 closed the loop): `{subject_id, subject_type}` → `{subject: Subject}`. Any code that read `g.subject_type` directly broke; the audit found one such site in `tunnel/dispatcher.rs`, which was updated. Pekohub's `instance.ownerId` enforcement (~9 sites in `pekohub/backend/src/routes/api/instances.ts`) is out of repo.
- **`Peer`'s JSON shape changed**: `{User: "alice"}` → `{kind: "user", id: "alice"}`. The on-disk session key format was unchanged (string-keyed, not JSON-tagged), so this only affected in-memory `serde_json` round-trips.
- **The back-compat shim was two fields, not a clever deserializer.** I tried the visitor approach first; it didn't work with TOML inline tables (the `deserialize_with` shim silently failed to be called). The two-field approach (`owner: Principal` + `owner_id: Option<String>`) was less elegant but worked, and the service-layer `resolved_owner()` kept the call sites clean.
- **`Identity::Local` returns bare `"local"`, not `"local:{did}"`** — the doc comment at `src/auth/caller.rs:92-96` was stale at the time of this ADR. The behavior matched the legacy wire format; the doc was fixed in this PR.

---

## 5. Out of scope (follow-up issues)

Each was a separate issue once this ADR landed. Items that shipped in ADR-041 are noted.

- **`principal_send` masquerade** (formerly the `a2a_send` work item): route as `Subject::Principal(caller_did)`. **(Part of ADR-041's P2P scope; not yet shipped.)**
- **Collapse IPC `(subject_id, subject_type)` wire field** into a single `subject: Subject`. **(Shipped in issue #30; the wire is now a single `subject: Subject` field.)**
- **`AuditEvent` first-class caller field** — add a `caller: Subject` to `AuditEvent`. **Not yet shipped.**
- **Pekohub-side `instance.ownerId` enforcement** for the principal case — ~9 owner-check sites in `pekohub/backend/src/routes/api/instances.ts`. **Out of repo.**
- **Per-principal persistent ed25519 keypair** in the runtime keychain; announce `principal_did` over the tunnel.
- **Principal marketplace / public discovery**: not started; depends on the above.

---

## 6. Migration risks

- **Session index on disk uses the v2 key format as its lookup key.** The byte-stability guarantee protected on-disk sessions through the ADR-039 migration. The follow-on ADR-041 migration to `<workspace>/memory/sessions/{session_id}.jsonl` was a separate migration step.
- **On-disk `owner_id = "string"` configs continued to work** through the `resolved_owner()` shim. The runtime migration (`src/runtime/migration.rs`) continued to backfill the empty-owner sentinel; no change.
- **The `PermissionGrant` JSON wire shape is unchanged at the IPC boundary** (the IPC packets now carry `subject: Subject`, which Pekohub must understand).

---

## 7. References

- [`src/subject.rs`](../../src/subject.rs) — current `Subject` enum (post-#30 + ADR-041)
- [`src/principal/`](../../src/principal/) — `Principal` container entity
- [ADR-033](ADR-033-ownership-and-permission-model.md) — the pre-existing permission model that `Subject` generalizes
- [ADR-041](ADR-041-principal-as-container.md) — successor; renames the actor enum to `Subject` and uses `Principal` for the container entity