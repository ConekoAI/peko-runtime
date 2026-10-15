# ADR-081: Actor identity, ownership, and a single online copy

**Status:** Proposed
**Date:** 2026-10-15
**Counterpart:** PekoHub ADR-081 (to be written from §5)

**Supersedes:**
- [ADR-073](ADR-073-actor-addresses-and-conversation-bindings.md): terminal login, the `local_<Hub UUID>` derivation, and snapshot identity handling.
- [ADR-077](ADR-077-local-and-hub-actor-identities.md): local IDs without a runtime qualifier, and the "logged-in account is a separate Hub-only actor" rule.
- [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) §2: the owner row of the trust table.
- ADR-057: the terminal attributing as the logged-in Hub account.

**Preserves:** ADR-072 run admission, ADR-074 per-principal discovery, the ADR-078 floor and policy, and pair DMs keyed by immutable actor keys.

---

## 1. Context

The actor system grew over several eras (ADR-033 roles, ADR-041 `Subject`, ADR-057 attribution, ADR-073/077 dual IDs, ADR-078 stamps). Each era left its own spelling of "who", and each authorization site picks its own. Verified on `master` @ `b987324d`:

| # | Fact | Where |
|---|---|---|
| F1 | `Subject` encodes scope inside its ID string: `User("hub:x")`, `User("local")`, `User("local:x")`, bare `prin_*`, `did:*`, plus a `local_<id>` compatibility rewrite. Equality is string equality after `normalized_for_auth`. | `subject/src/lib.rs:179` |
| F2 | Only `ChannelSend` and cross-runtime invites resolve aliases before checking grants. Ingress `check_permission`, `trust_for`, and the trust ceiling compare raw strings. | `channel_send.rs:346`, `daemon/state.rs:2094`, `principal/manager.rs:1263`, `security/stamps.rs:31` |
| F3 | Owner trust goes to `config.owner`, **and** always to `user:local`, **and** to whichever Hub account is logged in at the terminal — for every principal on the runtime. | `security/stamps.rs:37-40` |
| F4 | `config.owner` depends on how the principal was created: `peko create` sets `user:local`, IPC create sets the caller (`user:hub:<id>` when logged in), snapshot import forces `user:local`, and a seed keeps its own owner and grants. There is no transfer, only hand-editing. | `cli/commands/principal.rs:631`, `ipc/handlers/principal.rs:1342`, `principal_unpackager.rs:489` |
| F5 | Local IDs carry no runtime. Import adopts the snapshot's `prin_*` unless the actor key is already known, so importing one snapshot twice under two names yields two principals sharing an ID and key. | `principal_unpackager.rs:283-313` |
| F6 | Learned contacts (`known_by`) and channel memberships live in runtime-tier files, so they are never in a snapshot. `POLICY.md`, `workflows/`, and `plugins/` are not exported either. | `channel/src/store.rs:73`, `principal_packager.rs:130` |
| F7 | The local user's DM identity key is a random UUID per runtime, so the owner's conversation forks on every move. | `channel/src/store.rs:2080-2090` |
| F8 | The runtime has two keys. `runtime/identity.toml` holds a vault-held `did:key` (used for invite tokens and announce `runtimeId`). `peko tunnel setup` mints a second key (`tunnel:<did>`, `pekohub.toml`) used for the tunnel handshake and `caller_runtime_id`. Registration falls back to no proof of possession when the challenge fails. | `identity/src/runtime.rs`, `cli/commands/tunnel.rs:100-215`, `daemon/state.rs:1481` |
| F9 | A directory result naming another runtime can still merge into a *hosted* principal's record by actor key and attach a Hub ID without a registration receipt. | `channel/src/store.rs:2034-2060`, `tunnel/cross_runtime.rs:74` |
| F10 | Hub: a new tunnel connection replaces the existing one for that runtime. `instances.hub_id` is not unique, so several runtimes may deploy one Hub principal; only *public* duplicates are rejected. | `pekohub tunnel-manager.ts:255`, `pekohub schema.ts:300`, `instances.ts:597` |
| F11 | Cron jobs replay the trust stored at creation; revoking a grant or changing the owner does not lower it. | `principal/trust.rs` `cron_job_stamp` |
| F12 | The Hub private allow-list is built from grants only, without the owner. Approval notices go to `config.owner`, then `user:local`, never to the account the operator is using. | `tunnel/dispatcher.rs:338`, `principal/policy_gate.rs:209` |

The goal: one identity model that every part uses the same way, and that survives snapshots and moves. Concretely:

- **Offline first.** A runtime, its local user, and its principals work fully without Hub.
- **Online means registered.** Remote access requires the runtime, the local user, and the principal to be registered.
- **Many copies, one online.** A principal may be copied to many runtimes, but only one copy is online.

---

## 2. Decision

### D1: An actor is its immutable key; IDs are routes to it

```rust
// peko-subject (leaf; replaces Subject)
pub enum ActorKind { Principal, User, Visitor }
pub enum Route {
    Local { runtime: RuntimeDid, id: String }, // meaningful only on `runtime`
    Hub(Uuid),
}
pub struct ActorRef { pub kind: ActorKind, pub route: Route }
pub enum Grantee { Actor(ActorRef), Public }   // Public is a wildcard, never an actor
pub struct ActorKey(String);                   // immutable identity
```

| Actor | `ActorKey` | Routes |
|---|---|---|
| Principal | `actor_key`, minted at creation | `Local` on each runtime that hosts or hosted it; `Hub` once registered |
| Local user, unregistered | `local@<runtime DID>` | `Local { runtime, "local" }` |
| Local user, registered | the account's Hub UUID (fixed from registration on) | `Local { runtime, "local" }` + `Hub` |
| Hub user / visitor | Hub UUID | `Hub` |

- **Persisted form.** Every stored reference — owner, grants, contacts, cron creators, stamps, `POLICY.md` sections, channel rows — is a structured `ActorRef` carrying its runtime.
- **Tool-facing text.** Stays `kind:local:<id>` (this runtime) and `kind:hub:<uuid>`. A local route on another runtime is shown, never addressable.
- **One comparison.** `ActorDirectory::key_of(&ActorRef) -> Option<ActorKey>` is the only identity comparison. Every authorization site matches keys: ingress chat, trust, ceiling, `ChannelSend`, invites, the Hub allow-list, policy sections, approvals, cron.
- **Reachability.** An actor is **reachable** when its key is hosted on this runtime, or it has a verified Hub route. Otherwise it is **dormant**: kept, listed with `reachable: false`, matched by no authorization.
- **Revival.** A dormant local route revives only when its runtime is this runtime **and** its key matches this registry's record for that ID.

### D2: One runtime identity

- **One key.** The runtime DID is the vault-held `did:key` in `runtime/identity.toml`. The tunnel handshake, registration, invite tokens, and `caller_runtime_id` all use it. The second tunnel key and `pekohub.toml`'s `runtime_id` are retired.
- **It labels routes; it doesn't prove anything locally.** Offline the DID only qualifies local routes, which are never accepted from the network. A copied runtime directory gains nothing its disk owner lacked.
- **Online it is a credential.** Registration and every tunnel connection prove possession of the key. Registration without proof fails; there is no fallback.

### D3: One local user per runtime, registered with the runtime

- **The terminal is always the local user.** Every runtime has exactly one local user.
- **`peko register` registers the runtime and the local user together** with one Hub account, which becomes the local user's Hub route and its key from then on (D1).
- **One-time re-key.** Registration re-keys the local user's existing DMs once: channel IDs are recomputed and their principal session bindings move with them.
- **Changing accounts.** Unregistering and registering under another account creates a new local user. The previous one remains a Hub-only contact.
- **Retired.** `peko login`/`logout` as an identity switch, `runtime/hub-login.json`, the "logged-in account" actor, and `pekohub.toml`'s separate `owner_id`. Other humans reach the runtime remotely as Hub users.

### D4: One owner

- **Exactly one owner.** `PrincipalConfig.owner: ActorRef`. Trust is `Owner` exactly when the actor's key equals the owner's key. The blanket `user:local` and logged-in-account rules are removed.
- **Operator ≠ owner.** Local IPC and admin-scoped API keys keep *administrative* authority: settings, removal, import, export, registration. In conversation the operator is the local user, so the operator is the owner only when the local user owns the principal. An operator hosting someone else's principal administers it but chats at whatever trust the owner granted. This doesn't stop a determined operator, who holds the files and the vault; it keeps the trust model honest.
- **Creation.** The owner is the local user, with whatever routes it has (a registered local user stores its Hub route).
- **Seeds.** Seeds drop the owner and grants. ADR-060 seeds are never copies.
- **Transfer.** `peko transfer <peko> <actor>` is an audited Security event.
  - The current owner may transfer; the operator may transfer when the local user is the owner.
  - Transferring an online principal also needs approval from the owner's Hub account (§5).
  - Grants and policy sections are unchanged.
- **Registration needs a Hub owner.** A principal can register only when its owner has a Hub route, so every online principal has a Hub owner.

### D5: Principal-owned identity state

These move from runtime files into the principal and travel with it:

| State | Where it lives |
|---|---|
| Owner, grants | `principal.toml`, as `ActorRef`s |
| Learned contacts | `local/contacts.json`: kind, routes, key, signing DID, name, verified-at |
| Channel memberships | `local/channels.json`: channel ID, roster, own session binding (no logs) |
| Policy | `POLICY.md`, sections resolved through D1 |
| Cron creators | `CronJob.created_by` as `ActorRef` + compartment; trust re-derived at fire time |

- **Runtime registry.** `runtime/actors.json` becomes a routing index rebuilt from hosted principals and the union of their contacts.
- **Where Hub routes come from.** Only this runtime's registration receipts and verified directory results write Hub routes.
- **No Hub routes on hosted principals from the directory.** A directory result never attaches a Hub route to a hosted principal; only that principal's own registration receipt does (fixes F9).

### D6: Snapshots carry identity; import decides locality

A `.peko` snapshot adds `local/contacts.json`, `local/channels.json`, `POLICY.md`, `workflows/`, and `plugins/`. On import:

1. **Local ID.**
   - If this runtime's registry has a record for the actor key, reuse its local ID; moving back restores the old ID.
   - Otherwise mint a new one. Never adopt the snapshot's ID.
   - If the key is currently hosted here, refuse; `--rotate-keys` creates a new actor instead (fixes F5).
2. **Owner.**
   - Has a Hub route: unchanged.
   - `Local { runtime: R, "local" }` with no Hub route: rewritten to this runtime's local user, which is the same user when R is this runtime.
3. **Grants, contacts, policy sections, cron creators.** Hub routes are unchanged. Local routes on another runtime go dormant (D1) and revive if the principal returns to that runtime.
4. **Hub contacts from the snapshot** are *unverified* until re-resolved through the directory. They never write the runtime's alias index directly.
5. **Channel memberships.** Hub-routed channels reattach on first traffic (pair DMs keep their IDs because keys are kept). Local-only channels go dormant. The owner's DM follows the owner rewrite in step 2, so the `/local-user` session continues.
6. **The import arrives offline**: not linked to this runtime on Hub until §5's move completes.

### D7: Online is one linked copy

- **Copies are allowed.** Any number of runtimes may host copies of a principal. They share its key, so key possession cannot tell them apart.
- **Online.** A principal is online on runtime R exactly when Hub links its Hub ID to R's runtime Hub ID and R's tunnel is live.
- **Only the linked runtime** announces it, sends or receives its traffic, and receives its durable deliveries.
- **Receivers check the link.** Hub and receiving runtimes reject principal-signed traffic from any runtime other than the linked one, not just traffic that fails signature checks.
- **Moving the link.** Registering a principal already linked elsewhere is a move request. It needs proof of the principal's key **and** approval from the owner's Hub account; approval is implicit when that account also owns the requesting runtime.
  - Hub then re-links, tells the old runtime (`principal_unlinked`), and the old copy becomes offline-only.
  - Queued deliveries follow the link, keeping message IDs (ADR-072).
- **Copies drift.** Copies diverge (sessions, kb, ledger). Moving the link changes which branch peers talk to; the move is audited on both sides and in the tool ledger.

### D8: Authorization uses one rule everywhere

- **Ingress** checks the owner first, then grants (`Grantee::Public` included), all through D1.
- **The Hub private allow-list** is computed by the same rule, so it includes the owner (fixes F12). The recipient runtime stays authoritative.
- **Stamps** carry `ActorRef`. Trust is derived from the key at minting, at completion wakes, and when a cron job fires (fixes F11).
- **Approval notices** go to the owner actor's own conversation (fixes F12).
- **Contacts and the roster prompt section** stay compartment-scoped (ADR-078 D4, already shipped).

---

## 3. Retired

| Item | Replacement |
|---|---|
| `Subject`, string scopes inside IDs, `normalized_for_auth`, the `local_<id>` rewrite, `principal_wire_id`, `subject_from_string_with_default_user`, the `User("")` default | `ActorRef` / `Grantee` / `ActorKey` |
| `subject_wire_form` and bare principal member rows | `ActorRef` rows |
| Separate tunnel key, `pekohub.toml` `runtime_id`/`owner_id`, registration without proof | D2, D3 |
| `hub-login.json`, `peko login`/`logout`, attributing the terminal as the logged-in account | D3 |
| Owner rules for `user:local` and the logged-in account in `trust_for` | D4 |
| `principal.toml` `hub_id` copy | Derived from the registry |
| `ActorInfo.registration` strings duplicating `Exposure` | One registration enum |
| `ActingFor.actor: String` | `ActorRef` |
| ADR-033 permissions nothing checks (audit during phase 1) | Removed |

Pre-launch: no migration code. Principals and runtimes are re-created; stored automation using old addresses must be rewritten.

---

## 4. Phases

1. **Types and one rule.** `ActorRef` and `ActorDirectory::key_of`. Every authorization site goes through D1 and D8. Owner rules per D4; cron re-derives trust. *Fixes F2, F3, F11, F12.*
2. **Runtime and local user.** One runtime key; mandatory registration proof; `peko register` for runtime + local user; deterministic local-user key and the one-time re-key. *Fixes F7, F8. Needs Hub §5.1–5.2.*
3. **Principal-owned state and snapshots.** D5 and D6, seed stripping, `peko transfer`. *Fixes F4, F5, F6.*
4. **Online link and moves.** D7, link-checked traffic, directory never attaching hosted routes. *Fixes F9, F10. Needs Hub §5.3–5.5.*
5. **Delete legacy.** Everything in §3, with docs (`API_SURFACE.md`, `DATA_MODEL.md`, `builtin-tools.md`).

---

## 5. PekoHub counterpart

1. **Runtime registration requires proof of the runtime key** and binds the runtime to one account, which is also the runtime's local user.
2. **One live tunnel per runtime.** A second connection is rejected while the first is live (heartbeat within the interval); a stale connection is replaced. The account can revoke a runtime key.
3. **A principal's Hub ID links to exactly one runtime** (`instances.hub_id` unique, or a separate link table). The move flow (D7) needs principal proof plus owner-account approval and notifies the old runtime.
4. **Principal traffic is accepted only from the linked runtime.** The directory answers with the linked runtime only.
5. **`ownerSubject` is the principal's owner** (a Hub actor), not the registration administrator. Transfers of online principals are recorded here.
6. **Retired:** the terminal-login `local_<UUID>` derivation and replace-on-reconnect.

---

## 6. Consequences

- One identity rule across ingress, trust, channels, Hub, policy, cron, and snapshots. Alias bugs stop being per-site.
- Contacts, owner, grants, policy, and channel memberships survive a `.peko` roundtrip. Local relationships go dormant away from home and revive on return.
- **The operator loses implicit owner trust** over principals they host for someone else. Powerful requests go through the owner or a `trusted` grant.
- **Shared machines share one local user.** Other people use Hub access.
- **No offline cross-runtime path.** Cross-runtime traffic goes through Hub.
- Registering the local user re-keys its existing DMs once.
- **Cloned runtime directories** can still impersonate a runtime after the original disconnects. Possession of the key is the identity; revocation is the remedy.

---

## 7. Tests to pin

- Alias equivalence: the same actor via local and Hub routes passes every authorization site; different actors with equal ID text never match.
- Owner: only the owner's key is `Owner`; the operator chatting with a principal owned by a Hub user is not the owner; seeds carry no owner or grants; transfer is audited.
- Import:
  - Reuses the local ID by key and never adopts a foreign ID.
  - A second import of a hosted key is refused.
  - A Hub owner is unchanged; a local owner is rewritten.
  - Foreign local contacts and grants are dormant and revive on return only when the key matches.
  - Snapshot Hub contacts stay unverified.
- Contacts, `POLICY.md`, memberships, and cron creators round-trip through export → import.
- Cron trust drops after its grant is revoked.
- A directory result never attaches a Hub route to a hosted principal.
- Link (with Hub fixtures): traffic from an unlinked copy is rejected; a move needs owner approval and takes the old copy offline; durable delivery follows the link.
- Registration without proof fails; a concurrent second tunnel is rejected.
