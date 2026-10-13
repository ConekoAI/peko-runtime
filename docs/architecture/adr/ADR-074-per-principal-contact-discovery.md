# ADR-074: Per-principal contact discovery

- Status: Accepted
- Date: 2026-10-13
- Refines: ADR-073 discovery scope

## Decision

The runtime alias registry binds identities for routing and authorization; its
presence does not make an actor discoverable by every principal. Contacts lists
the caller's own actor, current participants of its channels, and principals it
has explicitly learned. Unrelated hosted principals, remote contacts, users, and
visitors are not enumerated.

An explicit local principal reference may learn a hosted principal without Hub
access. An unfamiliar Hub principal requires a directory lookup as the caller
(anonymous when local-only), even if another principal already cached that actor.
User and visitor references resolve only within the caller's current channel
participants. Learning routing metadata does not grant permission to send.

Actor records carry a private `known_by` set of local principal IDs. Missing sets
default to empty. Learned relationships survive restarts and profile updates;
file locking and atomic replacement preserve concurrent learners. Channel-derived
contacts follow current membership rather than an invitation or discovery snapshot.
`ChannelPort::known_actors` remains available for runtime administration.

Contacts validates its action contract before dependency access. It verifies the
returned Hub ID, nonempty signing DID, immutable key, runtime and local IDs, and
registered exposure before persisting a directory result. Rejected responses do
not change the registry or learned relationships.
