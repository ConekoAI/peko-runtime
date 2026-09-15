# ADR-057: Single Attribution Identity per Caller — Server-Derived, Never Caller-Declared

**Status:** Accepted (implemented on branch `feat/single-identity-surface`)
**Date:** 2026-09-15
**Author:** rlsn (with WorkBuddy)
**Related:** [ADR-033](ADR-033-ownership-and-permission-model.md) (ownership +
`CallerContext::local()`), [ADR-034](ADR-034-runtime-authentication-and-authorization.md)
(auth methods, secure-by-default), [ADR-039](ADR-039-subject-and-permission-model.md)
(Subject model), [ADR-046](ADR-046-trust-and-audit.md) (trust + audit),
[ADR-048](ADR-048-channel-native-cli-surface.md) (CLI surface — `-U` is
retired by this ADR), [ADR-049](ADR-049-multi-party-group-channels.md)
(D6/D7 identity binding — amended by this ADR),
[ADR-032](ADR-032-runtime-identity-and-multi-host-awareness.md) (runtime
DID = runtime identification, **not** a user identity).

---

## 1. Context

An audit of the identity path (2026-09-15) found that every
caller-facing surface carried a *caller-declared* identity field, and
that the IPC server trusted it verbatim for local callers:

1. **`PrincipalSend` / `PrincipalSendStream` carried a `user` field.**
   The dispatch in `ipc/handlers/principal.rs` did not pass the
   `CallerContext` into `run_principal_send` at all — the packet's
   `user` string was wrapped as `Subject::User(user)` and became the
   peer (session child, DM channel, author attribution). Any process
   able to reach the local socket could run a turn *as any actor*,
   on any surface including the pekohub JWT bridge and API-key
   callers.
2. **`ChannelPost.sender_name` / `ChannelPeek.requester` were
   accepted verbatim for `Identity::Local` callers**
   (`CallerBinding::Trusted` in `ipc/handlers/channel.rs`).
3. **The CLI made this trivially reachable**: global `-U/--user`,
   `peko send --peer user:<id>`, `peko channel post <sender>` — all
   client-side fictions the daemon never verified.

The permission layer was *not* entirely blind — `build_router_context`
checked `Permission::Chat` against the (declared) peer, and the
log/stop privacy rule anchors on the authentic `caller.subject()` —
but authorization keyed on a spoofable identity is not attribution.
The audit trail recorded the real caller in most places, yet the
*conversational record* (DM channels, group posts, session children)
was attributed to whatever string the caller typed.

Two distinct concepts were being conflated, and must now be explicit:

- **Authority** — who may act. Derived from the transport
  authentication only (`CallerContext`, ADR-034). Never spoofable.
- **Attribution identity** — who a message says it is from; the peer
  a conversation is keyed to. Previously caller-declared; after this
  ADR it is *derived by the server from the caller*, never read from
  the wire.

## 2. Decision

### D1 — Three identity classes; one attribution identity per caller, derived server-side

There are exactly **three identity classes** in the system. What the
table below lists per row is the verification path — the *class* is
the `Subject` kind:

| Class | Verification path | Attribution identity |
|-------|-------------------|----------------------|
| **user** | Local terminal whose runtime is logged into pekohub (credential + register binding) | `Subject::User(<hub owner id>)` |
| **user** | Remote pekohub client through the tunnel bridge (validated JWT sub; unverified headers rejected whenever a validator is configured) | `Subject::User(<jwt sub>)` |
| **local** | Local IPC without a pekohub login (OS socket boundary — anonymous) | `Subject::User("local")` |
| **principal** | Runtime-to-runtime tunnel / p2p (did:key signature verified per event) | `Subject::Principal(<principal DID>)` |

Every user-class identity shares one id space (pekohub user ids);
`local` is the unverified fallback, not a member of that id space.
The runtime DID (`did:key:…`, ADR-032) is reserved for runtime
identification and tunnel peer authentication — it is never a
conversational attribution identity for the local terminal, and the
remote *principal* (not the hosting runtime) is what cross-runtime
messages attribute to.

**API keys are not a fourth class.** An API key is a *scoped
credential of the runtime owner*: it attributes exactly as the owner
would (`Subject::User(<hub owner id>)` when logged in, else
`Subject::User("local")`), and channel writes/reads gate on that
identity. Least privilege comes from the key's scopes
(`ApiKeyScope`), which stay the authorization mechanism — the key's
grant-matching authority subject remains the typed
`Subject::Principal("apikey:{id}")` projection so owner grants do not
silently apply to key traffic — and the audit trail records the
credential id alongside the attribution identity.

Hub login status is the presence of
`{config_dir}/runtime/pekohub.toml` with a stored `owner_id` (the
pekohub user that owns the runtime registration; captured at
`peko tunnel setup` from the register endpoint's response).

### D2 — Permission checks use the authority, not the attribution

`build_router_context` gains an explicit `authority: &Subject`
parameter (the existing function delegates with
`authority = &peer`). For the IPC send path:

- `authority = caller.subject()` — the grant-matching projection
  (ADR-033/issue #68/R5 conventions unchanged).
- `peer = caller.attribution_subject()` — the conversation key.

Consequence: a local terminal logged into pekohub keeps full owner
authority (local trust = owner, ADR-033) while its messages are
attributed to `user:<hub owner id>`. Remote JWT/API-key callers are
unchanged: their authority and attribution are the same subject.

### D3 — Wire cleanup (clean break, pre-launch)

- `PrincipalSend` / `PrincipalSendStream`: the `user` field is
  removed. `DaemonClient::principal_send{,_stream}` lose the `user`
  parameter.
- `ChannelPost.sender_name` becomes `Option<String>`:
  `None` = "speak as my own identity" (server-derived); `Some`
  must resolve to a **principal name** — a runtime-hosted actor
  operated by the caller. A `user:<id>` sender is refused with
  `[forbidden]`: users cannot be named on the wire, only derived.
  Principal-name posting remains legitimate: the actor is hosted by
  this runtime, the caller's authority is checked, and the audit
  trail records the real caller (`audit_with_caller`).
- `ChannelPeek.requester` is removed; the read gate binds to the
  caller's derived identity — the membership gate now applies to
  every caller class, including API keys (which read as the owner
  they belong to).
- CLI: the global `-U/--user` flag and `peko send --peer` are
  removed. `peko log --peer` / `peko stop --peer` keep their
  owner-gated thread-selector role (they select *which* thread to
  read/stop — including `principal:<did>` threads — and the server
  privacy rule `caller == peer || caller == owner` still gates
  them); the user-form value is now only ever meaningful for the
  owner reading a remote user's thread, never for *becoming* one.
- `CallerBinding::Trusted` is replaced by `CallerBinding::Derived`
  (local terminal or its scoped key) and `CallerBinding::User`
  (pekohub JWT); the declared-equals-own rule applies uniformly.

### D4 — Pekohub hub side

No protocol change is required from the hub: the register endpoint
(`POST /v1/runtimes/register`) already returns the owning user's id
in its response row, the tunnel bridge already forwards the validated
JWT sub, and `x-pekohub-user-id` cross-checks stay. The runtime-side
`peko tunnel setup` learns and persists `owner_id` from the register
response it already receives.

## 3. Consequences

- **Impersonation is closed**: no surface accepts a user identity
  from the wire. Spoofing a user requires forging transport
  authentication (OS socket access, JWT possession, key possession) —
  which is the trust boundary itself.
- **Attribution is trustworthy end-to-end**: session children,
  DM channels, group posts, and audit events all key off the same
  derived subject.
- **Peer child slugs** (`/local-user`, `/user-<id>`,
  `/principal-<did>`) now reflect real callers only. A runtime that
  logs into pekohub switches its local attribution from
  `user:local` to `user:<hub owner id>`; conversations continue in
  the new peer child. Pre-launch: no data migration is provided for
  old `-U`-derived `/user-<id>` children — they remain readable
  history.
- **Principals minted while logged in** record
  `owner = user:<hub owner id>`; principals minted offline keep
  `owner = user:local` and remain owner-accessible via local trust
  (D2).
- **peko-desktop and other IPC clients** that passed a `user` field
  must drop it (clean break; pre-launch).
- ADR-048's "defaults to the global `-U/--user`" text and ADR-049
  D6/D7's Trusted-binding and `user:<id>`-sender rules are superseded
  by this document.

## 4. Implementation map (as built)

- `peko-rs/auth/src/caller.rs` — `CallerContext.hub_owner` field,
  `attribution_subject()`, `local_with_hub_owner()`.
- `peko-rs/core/src/ipc/server.rs` — `resolve_caller` builds Local
  callers with the daemon-resolved hub owner.
- `peko-rs/core/src/daemon/state.rs` — `hub_owner` resolved from the
  pekohub credential at build time; `AppState::hub_owner()`.
- `peko-rs/core/src/ipc/packet.rs` — field removals (D3).
- `peko-rs/core/src/ipc/handlers/principal.rs` —
  `run_principal_send` takes `caller`; peer/authority split (D2);
  authority threaded into `run_steering_successor`.
- `peko-rs/core/src/ipc/handlers/channel.rs` — `CallerBinding`
  rework; post/peek binding (D3).
- `peko-rs/core/src/principal/manager.rs` —
  `build_router_context_as` authority split (D2).
- `peko-rs/core/src/tunnel/credential.rs` — `owner_id` field.
- `peko-rs/cli/src/commands/{mod,send,channel,tunnel}.rs` — flag
  removals, derived senders, owner capture at setup.

---

## 5. Post-implementation security audit (2026-09-15, second pass)

A full re-audit of the identity model and the transport layer
(three parallel sweeps: IPC identity-field census, runtime transport,
hub-side verification) confirmed the core invariant — **no
caller-declared user identity reaches attribution on any surface** —
and surfaced the following, fixed in the same change set:

1. **Authority split was missing on the initial send path.**
   `run_principal_send` still called `build_router_context`
   (authority = attribution peer), so API-key callers passed the
   owner-equality check via their owner attribution. Now uses
   `build_router_context_as(caller.subject())` like the successor
   path (D2 restored everywhere).
2. **Channel surface could act as any loaded principal.**
   `ChannelCreate`/`ChannelInvite`/`ChannelLeave` resolved arbitrary
   principal names with no caller check — a remote JWT caller could
   create a channel as any principal, invite any `user:<id>`, and
   leave. New `gate_principal_operator`: operating a runtime-hosted
   principal by name is a Derived-caller capability (local terminal /
   its scoped key); pekohub users are refused, consistent with the
   post rule.
3. **Bridge ACL keyed on the unverified header.**
   `handle_proxied_request` now resolves the caller (JWT `sub` when a
   validator is configured) *before* `check_request_allowed`, and the
   Private-instance allowlist matches the resolved identity instead
   of the raw `x-pekohub-user-id` header.
4. **Cross-runtime envelope hardening (receiver-side).** Inbound
   signed channel events/invites now (a) verify
   `recipient_runtime_id == own DID` (was signed but unchecked), (b)
   dedupe on the signature-covered `request_id` via a bounded FIFO
   replay cache (`REPLAY_CACHE_CAPACITY = 4096`) — the signed
   pre-image carries no timestamp/nonce, so captured envelopes used
   to re-verify forever — and (c) require the inner event's channel
   to match the envelope's signed `channel_id`.
5. **Invite revocation overflow.** `InviteRevocationSet::revoke`
   cleared the entire set at the 1024 cap — a fresh-UUID flood
   un-revoked everything at once. Now FIFO-evicts the oldest entry.
6. **Half-configured mTLS failed open silently.** Setting only one of
   `tls.cert_path`/`tls.key_path` silently disabled client auth; now
   a hard config error.

### Residuals resolved (2026-09-15, third pass)

- **Bridge caller trust — CLOSED.** The hub now mints a short-lived
  (60 s) **EdDSA bridge token** when proxying each chat:
  `sub` = pekohub user id / `principal:<did>` / visitor id,
  `aud` = the target runtime DID, `iss` = the hub public origin
  (`PUBLIC_ORIGIN`). The signing key is an ed25519 keypair derived
  via HKDF-SHA256 from `JWT_SECRET`; the public key is published at
  `/v1/jwks.json`. The runtime builds its `JwtValidator` from its
  pekohub credential (issuer = tunnel-URL origin, audience = own
  DID, JWKS = `{origin}/v1/jwks.json`) and
  `resolve_bridge_caller` now **rejects every request without a
  valid token** — the unverified `x-pekohub-user-id` header fallback
  and the header itself are deleted from both sides.
  `enable_pekohub_jwt` defaults to true.
- **Direct p2p transport — fully removed.** The retired transport's
  dead config (`TransportPreference`, `direct_endpoint`,
  `direct_tls`, `directEndpoint`/`transportPreference` hub columns),
  the `build_server_config` TLS builder, and the
  `KnownRuntimes`-registry plumbing that carried them are deleted
  from both repos (hub migration `0013_drop_transport_fields`).
- **TrustLevel registry — removed.** No access decision ever
  consulted it; `KnownRuntimes`, the `runtime list/register/trust/
  remove` IPC variants, and the CLI commands are gone.

### Accepted residuals (documented, not fixed)

- **Hub-side** (fixed in the pekohub repo,
  `fix/tunnel-instance-ownership-scope`): instance
  heartbeat/status/deregister tunnel messages are now scoped to the
  owning runtime (`instance.runtimeId === conn.runtimeId`); previously
  any connected runtime could delete or flip another user's
  instances. Residual hub notes: visitor-cookie identity on public
  chat is forgeable (low impact), dev-bypass hinges on `NODE_ENV`
  defaulting to `development`, and the replay cache is a bounded
  4096-entry FIFO (full closure needs a signed timestamp in the
  envelope pre-image — a wire change).
