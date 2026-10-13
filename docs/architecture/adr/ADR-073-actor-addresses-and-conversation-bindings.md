# ADR-073: Dual actor identities and channel-only conversation bindings

Date: 2026-10-13  
Status: Accepted  
Counterpart: PekoHub ADR-073  
Supersedes: DID addressing, recipient-only DMs, actor-bound conversation sessions, and automatic principal publication. Preserves ADR-072 run admission.

## Identity

Every actor known to a runtime has one permanent local ID and an optional Hub ID.
Users, visitors, and principals occupy distinct namespaces. A principal's local ID
is `prin_*`; the default offline user is `local`. An authenticated Hub account or
visitor gets the deterministic local ID `local_<Hub UUID>`, with its kind retained.
Changing accounts creates a different local user. Logout returns to the default
local user. Network failure preserves the cached logged-in account.

Actor references are explicit:

| Kind | Runtime reference | Hub reference |
| --- | --- | --- |
| Principal | `principal:local:<id>` | `principal:hub:<UUID>` |
| Account | `user:local:<id>` | `user:hub:<UUID>` |
| Visitor | `visitor:local:<id>` | `visitor:hub:<UUID>` |

Both references resolve to the same runtime actor before membership, permission,
conversation, or author filtering. Remote contacts acquire a runtime-local proxy
ID, `local_<Hub UUID>`; a foreign runtime's local ID never becomes our local ID.
Names are display metadata. Owner/name handles select a Hub profile; ambiguous
handles fail instead of guessing an identity. Signing `did:key` values are private
transport credentials and are absent from actor tool profiles and addresses.

`runtime/actors.json` is the authoritative alias registry. It records the local ID,
optional Hub ID, immutable actor key, signing credential, name, and registration
state. Hub links enter it only through a correlated registration receipt or a
verified directory response. Editable `principal.toml` is a projection of the link,
not an attestation. Keys and Hub aliases cannot be reassigned to another local actor.

`runtime/hub-login.json` records the current human account, independently of the
runtime's tunnel credentials. `peko login --hub <origin> --api-key <key>` verifies
`/v1/auth/me` before changing it; the API key remains in the encrypted vault.
`peko logout` clears the current login without changing principal identities or
runtime ownership. Each IPC request rereads this metadata. Tunnel setup also records
its authenticated account. Runtime registration ownership remains distinct from
which human is currently using its terminal.

## Explicit registration

Principals have three states: local-only, registered private, and registered public.
Creation generates a local ID, immutable actor key, and vault-backed signing key;
it never registers the principal. Local tools, channels, and messaging require
neither Hub login nor principal registration.

`peko register <name>` requests private registration; `--public` requests public
registration. The runtime sends its local ID, immutable actor key, signing proof,
and request nonce. Hub assigns a stable actor UUID, separate from the deployment's
instance UUID. A matching receipt commits the alias. Reconnects reannounce only
registered principals; the same signing identity and actor key recover the same Hub
ID. A rejection, timeout, mismatched signer, or unsolicited receipt cannot establish
a new link. `peko unregister <name>` removes the deployment and local Hub link while
retaining the local actor key and conversation history. Removing an active Hub
registration requires a connected tunnel.

The Hub directory resolves `/v1/principals/by-hub-id/<UUID>` or owner/name handles.
Public principals resolve anonymously. Registered principals prove directory calls
with a short-lived EdDSA assertion bound to the exact path; private routing metadata
does not grant chat access. The recipient runtime enforces its own grants.
`local_only` principals cannot accept Hub ingress. Retired `unlisted`, `unexposed`,
DID directory routes, and exposure-management IPC are removed.

## Conversations and discovery

A session's conversation binding is solely `channel_id`. Actor stamps and session
titles do not select a conversation. A session remains one principal's private
working memory; channel membership determines access to the shared conversation.
Internal task sessions and the trunk are not actors or contacts.

A DM uses `dm_<SHA-256>` of the sorted, length-delimited immutable actor keys.
Registering an actor, changing visibility, reconnecting, or moving its deployment
cannot change this pair ID. Each locally hosted principal has its own session path
in the channel's `bindings` map. Human and visitor peers are explicit members.
Groups expose their current member roster rather than an invitation-time snapshot.

`Contacts list` exposes the caller's own actor, its current channel rosters, and
principals explicitly learned by that caller, including both IDs, names,
registration states, and mention tokens. Discovery is per-principal (ADR-074).
`Contacts resolve` learns a registered remote principal or an exactly addressed
locally hosted principal. `ChannelSend.to` accepts
either scoped actor reference or an actual channel ID. `ChannelRead` exposes current
participants and resolves either actor alias in author filters. Turn context shows
one's own two identities and the participants of known channels. Local, Hub, and
unambiguous name mentions target the same principal.

Cross-runtime invites include both IDs and immutable keys. Principal ownership of
the signing key, Hub identity, runtime route, recipient actor, and DM pair are checked
before mirror creation. Principal posts must match their authenticated signing
identity; human posts carry a Hub reference attested by their runtime and its member
row. Asserted principal identities without key proof are rejected. Local-only actors
cannot be advertised as remote channel participants. Durable deliveries retain
message IDs when a registered principal changes hosting runtime.

## Pre-launch replacement

This is a coordinated wire and storage replacement, not a compatibility rollout.
Old actor addresses, `ChannelSend.channel`, actor-based session bindings, publication
states, and unsigned principal transport paths are unsupported. Historical ADRs
describe retired behavior. Update stored automation/tool calls to the scoped
references and `ChannelSend.to` contract before launch.

Snapshots export signing credentials from the runtime vault. Import installs the
credential into that vault, clears unverified Hub projections, and preserves the
immutable actor key; rotating keys creates a new local actor. Known contacts retain
this runtime's local ID when promoted into a hosted principal. Group additions send
signed roster snapshots to all remote members; signed leaves remove the actor from
current membership. Private chat grants are enforced by the receiving runtime for
both DM and group ingress. Hub registration ownership remains an administration role.
