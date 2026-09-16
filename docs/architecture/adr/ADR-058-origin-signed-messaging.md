# ADR-058: Origin-Signed Messaging — Per-Principal Keys, Proof-of-Possession Registration, Typed Bridge Claims

**Status:** Accepted (2026-09-16). D1–D7 implemented on branch
`docs/adr-058-origin-signed-messaging` (D1+D2+D3 first; D4-runtime,
D5, D6, D7 in the follow-up commit; hub side in pekohub).
**Date:** 2026-09-16
**Author:** rlsn (with Kimi Code)
**Related:** [ADR-057](ADR-057-single-attribution-identity.md) (single
attribution identity — its §5 "accepted residuals" are re-scoped and
closed by this ADR), [ADR-035](ADR-035-runtime-pekohub-tunnel-protocol.md)
(tunnel protocol — envelope format superseded),
[ADR-032](ADR-032-runtime-identity-and-multi-host-awareness.md) (runtime
DID), [ADR-039](ADR-039-principal-model.md) (principal model),
[ADR-046](ADR-046-trust-and-audit.md) (trust + audit),
[ADR-049](ADR-049-multi-party-group-channels.md) (channels; invite
rules amended), [ADR-054](ADR-054-principal-genesis-pipeline.md)
(principal genesis — key generation hooks in here),
[ADR-034](ADR-034-runtime-authentication-and-authorization.md) (runtime
auth).

**Note:** Like ADR-046, this is a clean-slate pre-production change.
The tunnel wire format breaks; no compat shim is provided.

---

## 1. Context

A three-path security audit (2026-09-16, peko-runtime + pekohub)
covered every ingress a message can take: local user → local
principals/groups (not logged in), hub user → local/remote
principals/groups via hub relay, and local principal → remote
principal via hub relay (DID-addressed). The audit confirmed that
**runtime-level identity is cryptographically sound** — the tunnel
handshake (signed hello + replay-protected nonce challenge), the
hub's source-allowlist kill on `sourceRuntimeId` mismatch, and the
per-envelope ed25519 signatures with recipient binding all hold.
It also confirmed ADR-057's core invariant locally: no wire field
lets a caller declare a user identity.

But it found that everything *inside* the authenticated pipes is
**asserted, not proven**:

1. **Principal DIDs are claims, not identities.** Cross-runtime
   envelopes (`TunnelChannelEvent` / `TunnelChannelInvite`) are signed
   with the sending *runtime's* tunnel key only.
   `source_principal_did` and the inner event `author` are
   signature-covered assertions. The hub is a pure relay with no
   membership state; the receiver's `append_remote_event`
   (`peko-rs/channel/src/store.rs`) deliberately performs no
   membership or author check, and the remote-membership check that
   exists (`is_remote_member`) is never called on the inbound path.
   DM channel ids are deterministic (`ChannelId::for_principal(did)`).
   Consequence: **any single registered runtime can inject events
   authored by any principal DID into any runtime's mirror**, fire
   the bound responders (`PassiveBindingResponder`,
   `GroupWakeResponder`), and drive LLM turns with attacker text;
   it can also unilaterally "invite" any exposed principal via
   `dm_channel_mirror_bootstrap` and land itself in `remote_members`.
2. **The hub launders anonymous strings into signed identity.** The
   `pekohub_visitor` cookie is accepted verbatim (unsigned,
   length-capped only) and becomes the `sub` of the EdDSA bridge JWT
   the hub signs for proxied public-chat requests
   (`pekohub/backend/src/services/visitor-cookie.ts` →
   `services/tunnel-router.ts`). ADR-057 §5 recorded this as an
   accepted residual of *low* impact; the audit upgrades it to
   **high**: a cookie value of a victim's hub UUID forges
   `user:<uuid>`, a value of `principal:did:key:z…` forges a
   *principal* subject (the runtime's `from_bridge_user` maps
   `principal:`-prefixed strings to `Subject::Principal`), and the
   value `local` forges `user:local` — remote spoofing of the
   local-user identity into the `/local-user` peer child. Three
   identity classes share one stringly-typed `sub`, and one of them
   is attacker-controlled.
3. **DID registration requires no proof of possession.**
   `POST /v1/runtimes/register` accepts any `runtime_did` string from
   any authenticated user; first registrant owns the directory row
   (DID squatting / directory poisoning). `principalDid` in instance
   announces is likewise stored on say-so.
4. **Local transport trust is softer than its own comments claim.**
   The Unix socket relies on "filesystem mode bits" that nothing
   sets (no `0600` on `daemon.sock`, plain `create_dir_all` on
   `$PEKO_HOME/run`); UDP local trust is source-IP-based; and the
   `daemon.bind_address` config knob is written by `peko config set`
   but never read — the loopback bind is a hardcoded constant, so
   `enforce_auth_for_public_bind` can never fire.
5. **Minor:** IPC `ChannelMembers`/`ChannelList` are ungated
   (enumeration by any caller); mirrored events persist with no
   provenance marker distinguishing them from local ones.

### The model error

These are not five bugs; they are one design posture. The system
implements **channel security**: the pipe is authenticated (tunnel
handshake, bridge JWT) and the contents are trusted because of the
pipe they arrived on. The industry-standard posture for relayed
messaging between independently-owned actors is **object security**:
every message is individually signed by its *originator's* key, and
the relay is assumed hostile. This is the model behind Signal/Matrix
(end-to-end signed+encrypted messages over untrusted servers), Nostr
(every event signed by the author's key; relays are dumb and
interchangeable), ActivityPub (HTTP signatures over a federated
relay mesh), and OIDC (audience-restricted tokens for delegated user
identity). Pre-launch is the one moment when migrating to the
standard model is cheaper than patching the old one.

## 2. Decision

Migrate identity-on-the-wire from channel security to object
security. Concretely:

### D1 — Every principal gets its own keypair; the principal DID is the key

At principal genesis (ADR-054), the runtime generates an ed25519
keypair for the principal and derives the principal DID as `did:key`
of *that* key. (Genesis already mints a per-principal ed25519
identity today, but as `did:peko:public:<name>:<keyhash>` with the
private key never loaded for signing — this ADR switches minting to
`did:key`, moves the private key into the vault
(`CredentialKind::PrivateKey`, `system_owned`, under a dedicated
`principal-identity` namespace so the runtime key's first-match
vault reconstruction scan can never grab a principal key), and
actually signs with it; `peko_identity::runtime::public_key_to_did_key`
is reused as-is for DID derivation.) The
principal DID stops being an arbitrary string a runtime asserts and
becomes a self-certifying identifier only the key holder can act
for. The runtime DID (ADR-032) remains what it is: runtime
identification and tunnel peer authentication.

### D2 — Cross-runtime messages are signed by the author principal, counter-signed by the runtime

Every `TunnelChannelEvent` and `TunnelChannelInvite` gains an
**author signature** produced by the originating principal's key
(D1) over the full event/invite payload. The runtime counter-signs
the envelope as today, so the hub's source-allowlist
(`conn.runtimeId === msg.sourceRuntimeId`) keeps working unchanged
for relay ACL and abuse control.

The receiving runtime:

1. Verifies the runtime counter-signature against the
   `did:key`-derived source runtime key (existing check, unchanged).
2. Verifies the author signature against the key embedded in the
   claimed `source_principal_did` — forgery now requires the
   victim's private key, not just a registered runtime.
3. Enforces remote membership (`is_remote_member`) before
   `append_remote_event`, and validates invite `creator_did` /
   invitee binding before `dm_channel_mirror_bootstrap`. Membership
   checks become *meaningful* for the first time, because membership
   claims are now backed by principal signatures.

A compromised hub can no longer forge, alter, redirect, or replay
envelopes (already true), and a compromised *runtime* can no longer
speak for principals it does not host (new).

### D3 — Envelope cryptography migrates to detached JWS (EdDSA)

The bespoke length-prefixed pre-image in
`peko-rs/core/src/tunnel/tunnel_channel_signature.rs` is replaced by
**detached-payload JWS with EdDSA** (RFC 7515 / RFC 8037), giving
canonicalization, algorithm agility, and audited implementations for
free: `jose` on the hub (TypeScript), `jsonwebtoken` (or
`ed25519-dalek` + JWS compact serialization directly) on the runtime.
Because the hub `JSON.parse`→`stringify` round-trips every relayed
frame, signatures must cover a canonical pre-image, never raw wire
bytes — the JWS signing input (`b64u(header) || "." || b64u(payload)`)
satisfies this by construction.
The signed payload gains `iat`/`exp`, closing ADR-057 §5's residual
replay window properly — the bounded 4096-entry FIFO dedupe cache
becomes a belt-and-suspenders second layer instead of the only
replay defense. COSE (RFC 8152) is the acceptable alternative if
binary framing wins over JSON interop; JWS is preferred because the
hub must also produce/verify these structures.

### D4 — Hub registration and directory writes require proof of possession

`POST /v1/runtimes/register`, runtime re-registration, and
`instance_announce`'s `principalDid` claim each require a self-issued
JWS over a server-issued nonce, signed with the claimed DID's key
(the ACME key-change / SIWE pattern; the tunnel handshake already
implements exactly this challenge flow — it is extended to the
directory write paths). Directory rows become verified claims:
`runtime DID → owner`, `principal DID → home runtime`. DID squatting
and directory poisoning (finding 3) are closed, and D2's receivers
gain a directory they can actually consult when they choose to check
principal ↔ runtime binding.

### D5 — Bridge token claims become typed; visitor identity is hub-minted

The bridge JWT (ADR-057 §5, kept — audience-restricted issuer-signed
tokens *are* the standard pattern for delegated user identity) gains
a typed claim: `kind: "user" | "visitor"`.

- `user`: `sub` is a hub account id from a validated session/API
  key. Unchanged.
- `visitor`: `sub` is a **hub-minted** anonymous id — either an
  HMAC-signed cookie value or a server-stored session id; the
  client-supplied cookie is never signed into a token verbatim.
  The runtime maps `kind: visitor` to a distinct
  `Subject::Visitor(<id>)` namespace that cannot alias `user:`,
  `principal:`, or the reserved `local` id, at the type level (a new
  `SubjectKind`, not a string convention). Visitor subjects land in
  their own peer children (`/visitor-<id>`) and carry no
  user-principal authority assumptions.

The stringly-typed `sub` → `Subject` coercion
(`from_bridge_user`'s `principal:` prefix mapping) is deleted; a
bridge token can never again produce a principal subject. (If a
future hub feature needs authenticated principal-to-principal chat
originating *from the hub UI*, that identity must come from a D2
principal signature, not a hub claim.)

### D6 — Local transport: OS peer credentials, hardened modes, dead knob removed

- The Unix socket path adopts `SO_PEERCRED` / `getpeereid`: a peer
  whose uid differs from the daemon's is rejected (the Docker /
  systemd local-socket posture; the Windows named-pipe DACL from
  ADR-038 already implements the equivalent).
- `$PEKO_HOME/run` is created `0700`, `daemon.sock` `0600` — matching
  the vault and identity-key hygiene that already exists.
- The `daemon.bind_address` knob is either wired end-to-end (with
  mandatory credential auth when non-loopback) or **deleted**. A
  config knob an operator can set that silently does nothing is a
  latent exposure, not a feature. Preference: delete it pre-launch;
  reintroduce with auth when remote daemon access is a designed
  feature.
- The loopback-source-IP trust heuristic goes away with the knob;
  `Identity::Local` then means exactly "a process running as this OS
  user," which is a defensible boundary for a personal runtime.

### D7 — Channel read gates and provenance

- IPC `ChannelMembers` / `ChannelList` gain the same caller gate as
  `ChannelPeek` / `ChannelEventsWatch` (membership / Derived-caller
  checks).
- `events.jsonl` records author provenance per event:
  `local` (written by this runtime), `verified-remote` (D2 author
  signature verified), or — during rollout only — `asserted-remote`.
  Post-migration, `asserted-remote` appends are refused. `peko log`
  and the audit surface can then distinguish "this principal said
  this" from "a relay claimed so."

### D8 — Explicitly not adopted (recorded to prevent re-litigation)

- **Capability-token authz (UCAN / Biscuit).** UCAN fits
  conceptually (DID-issued, delegated, attenuable capabilities
  matching the `tool:*` grant model), but its Rust ecosystem is
  immature (rs-ucan stale; active implementations are JS), and
  Peko's grants are static principal config, not delegated tokens.
  Server-side ACLs + origin signatures cover every finding. Revisit
  only if principals must *delegate* authority across runtimes.
- **End-to-end content confidentiality (MLS / Signal / Noise).**
  Everything in this ADR is authenticity and integrity. Making
  message *content* unreadable to the hub is a separate, much larger
  commitment (key packages, group epochs, membership churn) with
  mature standards when needed: MLS (RFC 9420, OpenMLS in Rust) for
  groups, Noise (`snow`) or Signal for 1:1. Deferred to a future
  ADR; the D2 envelope format is designed not to preclude it.

## 3. Consequences

- **The hub downgrades from trusted-third-party to relay +
  directory.** After D2/D4/D5, a compromised or malicious hub can
  drop, delay, and reorder traffic, and can mint *visitor* identities
  — but cannot forge any user, principal, or runtime identity, and
  cannot author messages. This is the standard posture for federated
  / relayed systems and the correct pre-launch foundation.
- **Trust boundary moves from "the source runtime vouches for its
  principals" to "each principal proves itself."** A malicious
  registered runtime retains only nuisance powers (spam from its own
  principals, invite floods) that are attributable and rate-limited,
  not impersonation.
- **Breaking wire change** (`TunnelChannelEvent` /
  `TunnelChannelInvite` envelope format; bridge token schema;
  `Subject` gains a kind): acceptable pre-launch per the clean-slate
  note. peko-desktop and any external IPC clients must track D5/D6.
- **Principal DID stability**: existing principals' DIDs change
  meaning (DID = key). Pre-launch: principals are re-genesised or
  re-keyed; no migration of old DID-keyed data (DM channel ids,
  peer children) is provided — they remain readable history, as in
  ADR-057.
- **Operational cost**: one keypair per principal (negligible), one
  extra signature verification per envelope (negligible), and the
  hub gains nonce issuance on registration paths (small).
- **Amendments**: ADR-057 §5's "visitor-cookie identity on public
  chat is forgeable (low impact)" residual is re-scoped to high and
  closed by D5; its replay-cache residual is closed by D3. ADR-035's
  envelope/signature sections are superseded by D2/D3. ADR-049's
  invite rules are amended by D2's verified-creator requirement.

## 4. Implementation map (as built, 2026-09-16)

**peko-runtime:**

- `peko-rs/identity/` — no restructuring needed (spike 1): vault
  slots are already keyed by arbitrary `key_id`, the `IdentityVault`
  port mirrors that, and `KeyStorage` is per-DID. Changes: mint
  principal keys via `KeyPair::generate()` +
  `public_key_to_did_key` in
  `PrincipalManager::generate_identity`, store private keys in the
  vault under a new `principal-identity` namespace (NOT the runtime
  key's `"identity"` namespace — `try_reconstruct_from_vault`
  first-match scan would rebuild the runtime DID from a principal
  key), and add a per-principal signing-key loader/cache analogous
  to `load_runtime_signing_key` in `daemon/state.rs`.
- `peko-rs/core/src/principal/factory.rs` + genesis (ADR-054) —
  keypair generation, DID derivation (replaces the current
  `did:peko:public:<name>:<keyhash>` minting).
- `peko-rs/core/src/tunnel/tunnel_channel_signature.rs` — replaced
  by JWS envelope sign/verify (D3); dual-signature envelope types in
  `peko-rs/protocol/` (D2).
- `peko-rs/core/src/tunnel/dispatcher.rs` — author-signature
  verification, `is_remote_member` gate, invite creator validation.
- `peko-rs/subject/` — `SubjectKind::Visitor` + typed bridge
  mapping; delete `from_bridge_user` string coercion (D5).
- `peko-rs/core/src/ipc/server.rs` — peer-credential check, dir /
  socket modes, `bind_address` removal (D6).
- `peko-rs/core/src/ipc/handlers/channel.rs` — read gates (D7).
- `peko-rs/channel/src/store.rs` — provenance field on append (D7).

**pekohub:**

- `backend/src/services/visitor-cookie.ts` — HMAC-signed or
  server-stored visitor ids (D5).
- `backend/src/services/bridge-token.ts` — typed `kind` claim (D5).
- `backend/src/routes/api/runtimes.ts` +
  `services/tunnel-manager.ts` (register + announce) — nonce
  challenge + PoP verification; `services/tunnel-crypto.ts` +
  `jose` for JWS (D3/D4). Spike 2 confirmed the relay path needs
  **no** change for the author signature: the hub never parses
  inner event payloads and does not verify envelope signatures
  today (its only signature checks are the handshake nonces). The
  envelope changes must preserve the top-level `type` /
  `sourceRuntimeId` / `recipientRuntimeId` fields (routing +
  allowlist) and should keep `channelId` / `requestId` names for
  log correlation; the TS mirror types in
  `backend/src/services/tunnel-protocol.ts` track the new schema.
- `packages/shared` — envelope and token schema updates.

## 5. Verification spikes (2026-09-16) — all three pass

1. **Identity storage generalizes to N principal keys — PASS, no
   structural blocker.** Every N-key primitive already exists: vault
   slots keyed by arbitrary `key_id` (`Vault::set/get_identity_private_key`,
   `CredentialKind::PrivateKey` marked `system_owned`), the
   `IdentityVault` trait port, per-DID `KeyStorage`
   (`identity/src/storage.rs`, 0600 files / 0700 dirs), and the pure
   `public_key_to_did_key` codec (`identity/src/runtime.rs:225`),
   reusable as-is. Singleton assumptions are confined to
   `RuntimeIdentity` itself (fixed `identity.toml` path; the
   `try_reconstruct_from_vault` first-match scan — hence the
   dedicated `principal-identity` vault namespace in D1). Discovery:
   principals *already* receive a per-principal ed25519 keypair at
   genesis (`PrincipalManager::generate_identity`,
   `principal/manager.rs:705`), but minted as
   `did:peko:public:<name>:<keyhash>` and never loaded for signing —
   so D1 is a minting-format switch + vault custody + first actual
   use, not new machinery. Existing principals' `did:peko` DIDs
   change format under D1 (pre-launch: acceptable, no migration —
   see Consequences).
2. **Hub relay path is payload-agnostic — PASS.** For channel
   events/invites the hub reads only top-level `type`,
   `sourceRuntimeId` (allowlist), and `recipientRuntimeId`
   (connection-map routing); `channelId`/`requestId` appear in warn
   logs only. It never parses inner event payloads, has no channel
   ACL/metrics coupling, and — correction to a working assumption
   during drafting — does **not** verify the runtime envelope
   signature at all (its only signature checks are the handshake
   nonces); the receiver-side envelope verification in
   `tunnel/dispatcher.rs` is the only check, which D2 extends.
   Constraint recorded in D3: the hub re-encodes frames, so
   canonical pre-images are mandatory. The retired
   `principal_to_principal_request` path *does* have field coupling
   (`targetPrincipalDid` directory lookup, `callerPrincipalDid` ACL)
   — another reason it must stay retired.
3. **`jose` ↔ Rust EdDSA JWS interop — PASS, byte-exact both
   directions.** Round-trip spike (scratch:
   `target/tmp/spike-jws/` + `target/tmp/spike-jws-rs/`): jose 6.2.3
   `CompactSign` (EdDSA, the ADR-058 envelope schema with
   `iat`/`exp`) verified by Rust `ed25519-dalek` 2.2 `verify_strict`,
   and an `ed25519-dalek` compact JWS verified by jose
   `compactVerify` — payload bytes identical both ways. The signing
   input is the standard `b64u(header) || "." || b64u(payload)`;
   detached-payload transport only omits the middle segment, so this
   vector covers the D3 design. These vectors should be promoted to
   committed round-trip tests on both repos during implementation.

No premise changed materially; the hub-signature correction (spike
2) is reflected in D2/D3 and the implementation map.

### Implementation notes (D1+D2+D3, merged on this branch)

Two adjustments fell out of code review that the draft did not
anticipate:

1. **Invite `initial_members` re-keying.** The sender re-keys
   source-local principal rows in the invite snapshot to their
   vault-backed `did:key` DIDs — the receiver files those rows as
   its `remote_members`, and its inbound author gate
   (`is_remote_member`) matches the event envelope's
   `source_principal_did`, which is the same DID. Without the
   re-key the receiver would hold a local-id row that a
   did:key-authored event can never match.
2. **Author-DID resolution tolerates both id forms.**
   `Subject::Principal` carries either the principal's id
   (`prin_<uuid>`, the `principals` map key) or its DID, depending
   on the call path. `VaultPrincipalSigningKeys::did_for_principal`
   resolves by id first and falls back to
   `PrincipalManager::find_by_did`; a miss on the wrong form would
   have silently downgraded cross-runtime DM traffic to
   runtime-vouched on exactly the paths D2 protects.

The hub TypeScript mirror types (`pekohub`
`backend/src/services/tunnel-protocol.ts`) gained the
`authorSignature` field on both envelope interfaces; the hub needed
no logic change (spike 2).

### Implementation notes (D4-runtime, D5, D6, D7)

1. **D5 — typed claims, one mapping point.** The `kind` claim is
   optional at the `JwtValidator` layer (the validator also serves the
   IPC JWT path) and *required* in
   `tunnel::dispatcher::resolve_bridge_caller`, which now returns a
   typed `Subject` via `Subject::from_bridge_claim` (fail-closed on
   missing/unknown kind, empty/`:`-carrying subs, visitor `local`).
   The Private-exposure ACL match became kind-aware `Subject`
   equality. `Subject::Visitor` is a session peer everywhere `User`
   is, with these least-privilege exceptions: no Local-tier authority
   (`common/authority.rs`), `/visitor-<id>` peer children, and no
   deterministic principal-DID DM channel (slug-based `dm-<slug>`,
   same as users).
2. **D6 — peer-cred is Linux-only, via `nix` from the existing
   tree.** The unix IPC transport is a *datagram* socket: macOS has
   no per-message credential passing for `AF_UNIX/SOCK_DGRAM`, so the
   `0700` run dir + `0600` socket modes are the mechanism there
   (documented residual). On Linux, `SO_PASSCRED` +
   `SCM_CREDENTIALS` (nix 0.26.4, already vendored via
   keyring→secret-service→zbus; no new crate) rejects different-uid
   datagrams in the receive loop. The inert `bind_address` knobs
   (`network.bind_address` typed field + the free-form
   `daemon.bind_address` examples/templates/docs) were deleted per
   the ADR's preference; `[direct].bind_address` belongs to the
   B5-retired direct transport and is untouched (separate cleanup).
3. **D4 register PoP — `owner` source.** The signed register payload
   is `{"nonce","runtimeDid","owner","iat","exp"}`; `exp` and `owner`
   pass through verbatim from the hub's register-challenge response
   (the hub discloses `owner` = the authenticated user id at
   challenge time and compares it at register time — added during
   review after the two sides initially disagreed on where `owner`
   comes from). Hubs without the challenge endpoint receive the
   legacy register body without `pop` (warn-only), so setup keeps
   working against a pre-D4 hub.
4. **D7 provenance is storage-layer only.** The envelope
   (`{"origin","event"}`) wraps the line on disk; `ChannelEvent` wire
   type, the `ChannelPort` API, and `peko log` output are unchanged.
   Legacy bare lines parse as `local`.

---

## Appendix A — Audit finding → decision map

| Audit finding (2026-09-16) | Closed by |
|---|---|
| Principal DID / event author asserted, never proven; no remote-membership check; unilateral invite bootstrap | D2 (+ D1) |
| Unsigned visitor cookie signed into bridge JWT → forge `user:<uuid>`, `principal:<did>`, `user:local` | D5 |
| DID squatting / directory poisoning at registration + announce | D4 |
| Bespoke envelope crypto; replay closure needs signed timestamp | D3 |
| Unguarded local socket (modes, uid), source-IP trust, dead `bind_address` knob | D6 |
| Ungated `ChannelMembers`/`ChannelList`; no mirrored-event provenance | D7 |
| (Not findings, future asks) delegated authz, hub-blind content | D8 (deferred) |

---

## 6. Post-implementation review (2026-09-16, ADR-057 §5-style pass)

A decision-by-decision re-audit of both repos on the implementation
branches (`peko-runtime` `docs/adr-058-origin-signed-messaging` @
`17c5d8d1`, `pekohub` `feat/adr-058-envelope-mirror` @ `f812ec1`)
confirmed every decision is implemented as specified — D1 minting +
`principal-identity` vault custody + dual-form `did_for_principal`
resolution; D2 dual-JWS with byte-identical payload-segment binding,
receiver-side `is_remote_member` + creator≡source gates; D3 fixed
canonical JWS header + `iat`/`exp` freshness (exp 300 s, leeway 30 s /
60 s future-iat) with the replay cache demoted to second layer; D4
nonce-burning single-use challenge with `owner` disclosed and
pass-through-verbatim on both sides; D5 required `kind` claim with the
`principal:` path deleted and fail-closed `from_bridge_claim`
(empty / `:`-carrying / visitor-`local` all rejected); D6 `0700`/`0600`
modes + Linux `SO_PASSCRED` same-uid (fail-closed on missing creds) +
`bind_address` knob deletion, macOS datagram residual documented; D7
membership/operator read gates + `{"origin","event"}` provenance
envelope with legacy lines parsing as `local`. Targeted suites green:
12/12 signature, dispatcher author-gate, 3/3 PoP, members-gate,
19/19 subject (hub's 236/236 + tsc clean reported at commit, not
re-run here). The pass surfaced the following:

1. **Sender-side silent downgrade (medium).** In
   `tunnel_channel_port.rs`, when a vault-backed `did:key` principal's
   key load fails transiently (`did_for_principal` returns the DID but
   `signing_key_for_did` returns `None` — vault read error), the
   envelope silently falls back to the legacy wire id with an empty
   author signature, which the receiver accepts as runtime-vouched.
   The downgrade is exactly the path D2 exists to close. Fix: when the
   resolved principal DID is `did:key`, a missing signing key must
   fail the send (or at minimum warn loudly), never downgrade.
2. **No cutoff for the runtime-vouched path (medium, process).** Both
   inbound handlers accept non-`did:key` authors as runtime-vouched
   indefinitely; the ADR's "post-migration, `asserted-remote` appends
   are refused" has no enforcement switch or date. Legacy
   `did:peko:public:*` principals still exist, and the offline-CLI
   minting fallback (no vault) still produces them — so the
   unauthenticated-author path stays live in practice. Needs either a
   re-key tool for existing principals or a config gate with a cutoff.
3. **Hub register upsert fires before the ownership check (low).**
   `runtimes.ts` runs `onConflictDoUpdate` (which overwrites
   `displayName` and bumps `lastSeenAt`) *then* 403s when
   `row.ownerId !== user.id` — a valid-PoP caller who is not the row
   owner still clobbers the owner's display name and last-seen stamp.
   Move the ownership check before the upsert or make the update
   conditional on `ownerId`.
4. **Migration 0014 not applied (deploy blocker, hub).**
   `principal_did_verified` exists in `schema.ts` and
   `drizzle/0014_add_principal_did_verified.sql` is committed but not
   applied to the database; announce persistence of the verified flag
   will fail until it runs.
5. **Per-process challenge stores (low, scale-out note).** The D4
   register-challenge store (like the pre-existing tunnel-handshake
   nonce store) is in-memory per process; a multi-replica hub breaks
   both PoP flows. Fine for the single-instance deployment; revisit
   with shared storage at scale-out.
6. **Accepted, no action.** (a) Replay-cache FIFO eviction remains
   flood-evictable, but the 300 s signed `exp` bounds a replayed
   envelope's usefulness — the D3 belt-and-suspenders posture as
   designed. (b) Invites deliberately skip `is_remote_member` (an
   invite *creates* membership); a compromised sending runtime keeps
   only the nuisance powers the Consequences section already accepts.
   (c) The per-DID signing-key cache is never invalidated — sound,
   since DID = key means a re-genesis mints a new DID.

### Resolutions (2026-09-16, same-day fix pass)

1. **Fixed (fail-closed send).** `fanout_event` / `fanout_dm_invite`
   now return an error when a `did:key` author/creator's signing key
   cannot be loaded — no silent downgrade to runtime-vouched.
2. **Fixed (cutoff gate).** `auth_config.toml` gains
   `accept_asserted_remote_channel_authors` (default `true` for
   rollout; `false` refuses asserted-remote authors), wired
   `AuthConfig` → `TunnelHost::accept_asserted_remote_channel_authors`
   → both inbound handlers.
3. **Fixed (hub register ordering).** Ownership is checked (and 403
   returned) *before* the upsert; the post-write re-check remains as
   defense in depth.
4. **Applied (hub schema).** `principal_did_verified` reached the
   database via `drizzle-kit push` — the repo's actual schema workflow.
   **Follow-up flagged:** the drizzle *migrate* journal cannot
   bootstrap a fresh database (entries 0010–0013 missing, synthetic
   `when` timestamps skip 0008a–d, and no migration ever creates the
   `runtimes` table — dev databases were built with `push`). The
   migrate chain should be squashed to a fresh baseline or retired in
   favor of push.
5. **Cleaned (dead code).** The hub's retired P2P RPC relay
   (`principal_to_principal_request`/`_response` — no runtime has sent
   it since sprint 3 Phase 12b) is deleted: dispatch arms, handlers,
   in-flight registry, `HubA2A*` counters, mirror types, and the
   `principal_forwarding` integration suite (replaced by a
   channel-event metrics test). The runtime's parsed-but-never-read
   `[direct]` config block (`DirectNetworkConfig`) is deleted with a
   parse-compat test.
