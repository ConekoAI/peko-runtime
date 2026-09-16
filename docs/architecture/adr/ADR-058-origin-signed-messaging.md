# ADR-058: Origin-Signed Messaging — Per-Principal Keys, Proof-of-Possession Registration, Typed Bridge Claims

**Status:** Draft
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
keypair for the principal, stores the private key in the existing
identity key storage (`peko-identity`, `0700` dir / `0600` files),
and derives the principal DID as `did:key` of *that* key. The
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

## 4. Implementation map (planned)

**peko-runtime:**

- `peko-rs/identity/` — generalize key storage to per-principal keys
  (spike item: confirm the current single-runtime-key store
  generalizes cleanly).
- `peko-rs/core/src/principal/factory.rs` + genesis (ADR-054) —
  keypair generation, DID derivation.
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
  `jose` for JWS (D3/D4; spike item: verify the relay path can
  check the second signature without restructuring).
- `packages/shared` — envelope and token schema updates.

## 5. Verification spikes before commit

1. `peko-identity` key storage generalizes to N principal keys
   without restructuring (file layout, locking, vault interaction).
2. Hub `tunnel-manager` can verify the author (principal) signature
   on relayed envelopes without becoming a message inspector — it
   only needs the *runtime* counter-signature for its allowlist;
   principal verification belongs to the receiver. Confirm no relay
   fast-path depends on parsing event internals.
3. `jose` ↔ Rust JWS interop for EdDSA detached payloads across the
   exact envelope schema (one round-trip test vector committed on
   both sides).

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
