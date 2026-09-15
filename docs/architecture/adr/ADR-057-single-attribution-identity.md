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

### D1 — One attribution identity per caller, derived server-side

The daemon derives the attribution identity from the `CallerContext`
(`CallerContext::attribution_subject()`); no request packet carries a
user identity:

| Caller surface                          | Attribution identity            |
|-----------------------------------------|---------------------------------|
| Local IPC, not logged into pekohub      | `Subject::User("local")`        |
| Local IPC, logged into pekohub          | `Subject::User(<hub owner id>)` |
| Pekohub JWT bridge                      | `Subject::User(<jwt sub>)`      |
| API key                                 | `Subject::Principal(apikey:{id})` |
| Tunnel runtime peer (runtime-to-runtime)| `Subject::Principal(<runtime DID>)` (unchanged; signature-verified per event) |

Hub login status is the presence of
`{config_dir}/runtime/pekohub.toml` with a stored `owner_id` (the
pekohub user that owns the runtime registration; captured at
`peko tunnel setup` from the register endpoint's response). The
runtime DID (`did:key:…`, ADR-032) is reserved for runtime
identification and tunnel peer authentication — it is never a
conversational attribution identity for the local terminal.

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
  caller's derived identity (Local/JWT) or no requester (API key,
  matching its previous ungated posture).
- CLI: the global `-U/--user` flag and `peko send --peer` are
  removed. `peko log --peer` / `peko stop --peer` keep their
  owner-gated thread-selector role (they select *which* thread to
  read/stop — including `principal:<did>` threads — and the server
  privacy rule `caller == peer || caller == owner` still gates
  them); the user-form value is now only ever meaningful for the
  owner reading a remote user's thread, never for *becoming* one.
- `CallerBinding::Trusted` is replaced by
  `CallerBinding::Local(Subject)` carrying the derived identity; the
  declared-equals-own rule now applies uniformly to Local and JWT
  callers.

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
