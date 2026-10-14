# ADR-077: Local and Hub actor identities

Date: 2026-10-14
Status: Accepted
Refines: ADR-073 identity and snapshot promotion; preserves ADR-074 discovery

## Decision

An actor known to a runtime has exactly one of three identity states:

| State | Local ID | Hub ID | Canonical address |
| --- | --- | --- | --- |
| Local-only actor | Present | Absent | `kind:local:<id>` |
| Registered local principal | Present | Present | `principal:local:<id>` |
| Remote principal, Hub user, or visitor | Absent | Present | `kind:hub:<id>` |

Local is relative to the runtime whose registry is being read. A remote
principal's hosting-runtime local ID remains directory routing metadata; it
never becomes an ID in our local namespace. Learning a contact records knowledge,
not a new locally hosted identity. Neither ID may be empty, and an actor cannot
have neither ID. Public is an authorization wildcard, not a registered actor.

`ActorInfo.local_id` becomes optional. A hosted actor's Hub reference remains an
alias of its local canonical address. A remote actor's address and principal
mention use Hub scope. No `local_<Hub ID>` proxy is allocated. Authenticated Hub
users and visitors remain Hub-only even when the user logs into a local terminal;
logout selects the offline local user. Authorization retains the Hub namespace
instead of equating local and Hub IDs with the same text. Existing bare
`local_<id>` user/visitor grants retain their old Hub meaning during permission
checks; explicit `kind:local:<id>` grants never use that compatibility rule.

Locality and registration visibility are separate: remote and hosted principals
can each be private or public. ADR-074's per-principal learned relationships and
channel-derived discovery remain unchanged. Caching an actor never makes it
visible to unrelated principals and never grants chat permission.

## Identity continuity

Immutable actor keys, rather than canonical addresses or deployment IDs, identify
DM pairs. Registration adds an alias without changing a local actor's address.
Installing a remote contact's snapshot gives it a local ID and promotes the
existing registry record by its immutable key. Signing identity checks still
apply; principal-manager promotion also requires the signing credential in the
runtime vault. Directory claims of same-runtime hosting must match an actual
local principal and its immutable key and signing identity. Promotion preserves learned relationships and the verified Hub alias;
foreign local IDs are not adopted for an already known remote contact. Existing
hosted actors retain their local IDs. Removing a registered hosted principal
drops its local ID and retains a Hub-only contact; historical local references
continue resolving internally. Local-only removed identities retain their local
history. Rotated signing identities remain new actors.

Channel participant comparisons normalize aliases before comparing the fixed DM
pair. Private historical aliases retain author/membership resolution after unlinking;
removed aliases are rejected by public actor resolution. Historical Hub authors
continue resolving to the promoted actor, and the
pair hash and principal-local session bindings remain stable. Directory refreshes
cannot remove an existing hosted actor's local ID or substitute an actor key,
signing identity, or Hub identity.

## Scope

This refines the pre-launch identity replacement in ADR-073. Old proxy-addressed
automation calls must use Hub references. Before accepting traffic, daemon startup
reconciles existing actor records against the complete loaded-principal inventory.
Registered principals absent from that inventory, and Hub accounts/visitors,
become Hub-only. Promoted proxies that are actually hosted retain their local IDs.
Migration preserves keys, learned contacts, history, and DM hashes, using private
historical aliases. A failed principal load skips reconciliation rather than
using an incomplete inventory. The `local_` spelling is never proof of locality. No Hub wire change is required: directory
responses may continue carrying the hosting runtime's local ID as routing metadata.
