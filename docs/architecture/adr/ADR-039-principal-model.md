# ADR-039: Principal model — unifying User / Agent / Team subjects

**Status:** Accepted
**Date:** 2026-06-18
**Supersedes:** none
**Related:** [ADR-033](ADR-033-ownership-and-permission-model.md), [ADR-034](ADR-034-runtime-authentication-and-authorization.md), [ADR-035](ADR-035-runtime-pekohub-tunnel-protocol.md), [issue #11 (teams model)](../../), [issue #17 (per-user attribution)](../../), [issue #20 (this ADR)](../../)

## Context

The runtime had three places that tried to model "who is this?" but
disagreed on the universe of subjects:

- `Peer::{User, Agent}` in [src/session/types.rs:16-21](../../src/session/types.rs#L16) — no `Team` variant.
- `SubjectType::{User, Team, Public}` in [src/auth/ownership.rs:42-49](../../src/auth/ownership.rs#L42) — no `Agent` variant.
- `AgentConfig::owner_id: String` in [src/types/agent.rs:40](../../src/types/agent.rs#L40) — free-form, default `""`.

Each of these was a partial model. The "user" and "agent" concepts
were partially modeled in two of them; the "team" concept was
half-modeled in one. None of them were first-class.

This is the foundation gap that [#11 (teams model)](../../),
[#17 (per-user attribution)](../../), per-user extension permissions,
agent-as-peer support, and cross-runtime a2a all need. Fix the
foundation and the three follow-ups become much smaller.

## Decision

Introduce a single canonical actor type, `Principal`, in
[src/auth/principal.rs](../../src/auth/principal.rs). All three of
the pre-existing partial models collapse onto it as projections:

```rust
pub enum Principal {
    User(String),   // hub user id (after #17)
    Agent(String),  // agent instance id (did:key or hub-assigned)
    Team(String),   // team id (after #11)
    Public,         // unauthenticated / world-readable
}
```

### Rationale (one paragraph per design choice)

1. **`Peer` becomes a type alias for `Principal`** (rather than a
   newtype wrapper). The 25 `Peer::User(...)` / `Peer::Agent(...)`
   constructions across 14 files keep compiling unchanged. No
   `From<Principal>` boilerplate at every call site. We accept the
   conflation with `Team`/`Public` for now and protect session
   keying with `Principal::is_session_peer`.

2. **`AgentConfig::owner` and `TeamMetadata::owner` are `Principal`**
   (rather than `String`). A custom two-field back-compat shim
   (`owner: Principal` + `owner_id: Option<String>`) accepts both
   the legacy `owner_id = "string"` form and the new
   `owner = { kind = "user|agent|team|public", id = "..." }` form.
   Service layer reads `resolved_owner()` which folds the legacy
   `owner_id` into `owner` if `owner` is still the default
   `Principal::User("")`. New configs should set `owner` only.

3. **`SubjectType` is kept** as the IPC wire-side tag (`RequestPacket`
   still carries `(subject_id, subject_type)` for back-compat). The
   in-memory `PermissionGrant` collapses `subject_id + subject_type`
   into a single `subject: Principal`. The IPC server handler
   bridges to/from `Principal` at the boundary via
   `principal_from_wire` and the `deserialize_owner_principal` shim.
   `SubjectType::Agent` is added as a new variant.

4. **`PermissionGrant.granted_by: String` → `Principal`**. Composes
   cleanly with the new type. The `Principal::from_str` round-trip
   preserves the wire format (`user:alice`, `agent:helper`, etc.)
   for log lines and audit events. `AuditEvent` first-class caller
   field is deferred to a follow-up.

5. **Session key byte-stability is a non-negotiable contract**.
   `derive_base_session_key` keeps the v2 format
   `agent:{a}:peer:{kind}:{id}` byte-for-byte for `Principal::User`
   and `Principal::Agent`. `Principal::Team` and `Principal::Public`
   are not valid session peers; the function falls back to
   `peer:user:default` and emits a `tracing::warn!`. This is the
   documented escape hatch, not a bug — without it, a stray
   non-peer principal would orphan every on-disk session for the
   affected agent. Any change to the format is a forced
   migration for every peko-runtime user.

6. **Latent-bug fix at `manager.rs:1743`**: the wildcard arm of
   the parse-spawn-peer match previously defaulted to
   `Peer::Agent(id)`, inconsistent with the v1 defaults at
   `manager.rs:1046, 1049, 1052` which default to
   `Peer::User("default")`. Aligned to `Peer::User(id)` (or
   `Peer::User("default".into())` if `id` is empty). This is the
   one intentional behavior change in the PR; called out in the
   PR description.

### Wire compatibility

The IPC `RequestPacket` keeps its current shape:

- `(subject_id: String, subject_type: SubjectType)` on grant/revoke.
  The server handler builds a `Principal` from these two fields via
  `crate::auth::ownership::principal_from_wire`.
- The Revoke packets (`AgentRevokePermission`, `TeamRevokePermission`)
  don't carry `subject_type`. The server defaults to
  `Principal::User(subject_id)` (the legacy default kind). New code
  that needs to revoke an Agent/Team/Public grant would use a new
  packet variant — that's a follow-up ADR.

A follow-up ADR will collapse the wire to a single
`subject: Principal` field. Until then, the IPC wire format is
byte-identical to pre-039.

## Consequences

### Positive

- **The 3-place partial model is unified.** `Principal` is the
  single source of truth. New code only needs to learn one type.
- **#11, #17, per-user extension permissions, agent-as-peer, and
  cross-runtime a2a all become smaller follow-ups.** They no longer
  have to invent their own subject model.
- **Cross-kind guard is automatic.** `Principal::User("alice") !=
  Principal::Agent("alice")`. The pre-039 string-equality check
  silently allowed User/Agent subject collisions; the new
  `Principal` equality is type-safe.
- **No on-disk session format change.** The v2 key format is
  byte-stable; the `AgentConfig` / `TeamMetadata` on-disk TOML
  adds an optional `owner = { kind, id }` block but the legacy
  `owner_id = "string"` is still read.

### Negative

- **`PermissionGrant` JSON shape changes in memory**:
  `{subject_id, subject_type}` → `{subject: Principal}`. Any code
  that reads `g.subject_type` directly is now broken. The
  production code audit found one such site at
  [tunnel/dispatcher.rs:142](../../src/tunnel/dispatcher.rs#L142),
  which was updated. Pekohub's `instance.ownerId` enforcement
  (~9 sites in [pekohub/backend/src/routes/api/instances.ts](../../pekohub/backend/src/routes/api/instances.ts))
  is out of scope for this PR.
- **`Peer`'s JSON shape changes**: `{User: "alice"}` →
  `{kind: "user", id: "alice"}` (from the
  `#[serde(tag = "kind", content = "id")]` derive). The on-disk
  session key format is unchanged (string-keyed, not JSON-tagged),
  so this only affects in-memory `serde_json` round-trips. The
  one such test in `session::types::tests::test_serialization`
  was updated.
- **The back-compat shim is two fields, not a clever
  deserializer.** I tried the visitor approach first; it didn't
  work with TOML inline tables (the `deserialize_with` shim
  silently failed to be called). The two-field approach
  (`owner: Principal` + `owner_id: Option<String>`) is less
  elegant but works, and the service-layer `resolved_owner()`
  keeps the call sites clean.
- **`Identity::Local` returns bare `"local"`, not `"local:{did}"`**
  — the doc comment at [src/auth/caller.rs:92-96](../../src/auth/caller.rs#L92)
  is stale. The behavior matches the legacy wire format; the doc
  was fixed in this PR.

## Out of scope (follow-up issues)

Each is a separate issue once this lands:

- **`a2a_send` masquerade**: route as `Principal::Agent(caller_agent_id)`.
  The one-liner is now trivial; the cross-runtime transport work
  is the bigger piece.
- **Collapse IPC `(subject_id, subject_type)` wire field** into a
  single `subject: Principal`. Currently the IPC packets keep
  the two-field shape; once all clients have migrated, collapse.
- **`AuditEvent` first-class caller field** — add a `caller:
  Principal` to `AuditEvent`. The `PermissionGrant.granted_by:
  Principal` migration in this PR is the foundation.
- **Pekohub-side `instance.ownerId` enforcement** for the agent
  case — ~9 owner-check sites in
  [pekohub/backend/src/routes/api/instances.ts](../../pekohub/backend/src/routes/api/instances.ts).
  Out of repo for this PR.
- **Per-agent persistent ed25519 keypair** in the runtime keychain;
  announce `agent_did` over the tunnel.
- **Agent marketplace / public agent discovery**: not started;
  depends on the above.

## Migration risks

- **Session index on disk uses the v2 key format as its lookup
  key.** The byte-stability guarantee is what protects it. Any
  future change to the format is a forced migration for every
  peko-runtime user.
- **On-disk `owner_id = "string"` configs continue to work**
  through the `resolved_owner()` shim. The runtime migration
  (`src/runtime/migration.rs:170-171, 234-235`) continues to
  backfill the empty-owner sentinel; no change.
- **The `PermissionGrant` JSON wire shape is unchanged** (the IPC
  packets keep the `(subject_id, subject_type)` pair). Pekohub
  reads grants via the agent_config.toml file, not the IPC
  packet, so the in-memory shape change doesn't propagate to the
  hub.

## Test plan

- `cargo test -p pekobot --lib` — 1392 unit tests pass.
- `cargo test -p pekobot --test principal_back_compat` — 8
  integration tests for the new `Principal` type (Display /
  FromStr round-trip, JSON wire shape, session-key byte-stability
  for User/Agent, Team/Public fallback to `peer:user:default`,
  `principal_from_string_with_default_user` legacy forms).
- `cargo test -p pekobot --lib auth::ownership::tests` — the two
  ADR-039 acceptance-criteria tests:
  - `test_agent_caller_denied_for_user_owned_resource`
  - `test_agent_caller_allowed_for_agent_owned_resource`
  Plus `test_agent_caller_denied_for_other_agent_owned_resource`
  and `test_team_user_caller_does_not_match_agent_members` for
  the cross-kind guard.
- `cargo test -p pekobot --test cli_basics` — 6 tests pass;
  the 8 ignored tests are gated on external services.

## References

- [ConekoAI/peko-runtime#20](../../) — the issue this ADR closes.
- [ConekoAI/peko-runtime#11](../../) — teams model (follow-up).
- [ConekoAI/peko-runtime#17](../../) — per-user attribution
  (follow-up).
- [ADR-033](ADR-033-ownership-and-permission-model.md) — the
  pre-existing permission model that `Principal` generalizes.
