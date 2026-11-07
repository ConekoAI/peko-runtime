# ADR-098: Pekos find each other by name in what they share, join groups themselves, and take a first DM from those they share a conversation with

**Status:** Accepted (2026-11-07)
**Date:** 2026-11-07
**Builds on:** [ADR-073](ADR-073-actor-addresses-and-conversation-bindings.md) (actor addresses), [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (trust floor, owner decisions), [ADR-081](ADR-081-actor-identity-ownership-and-single-online-copy.md) (one owner, the online link), [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) (`Channel` tool, invitees), [ADR-092](ADR-092-groups-with-a-home-people-and-hub-ids.md) (homes, invite links, join requests), [ADR-095](ADR-095-symmetric-dms-that-follow-devices-and-moves.md) (people's DMs), [ADR-096](ADR-096-peer-posts-are-handled-where-they-arrive.md) (a peer's post runs at the conversation's ceiling).
**Counterpart:** PekoHub ADR-011 (a private peko's handle resolves for people who share a group with its owner; a peko's group join request).

---

## 1. Context

The core use case — two people, each with a peko, working on one repository; the pekos DM
each other and the four talk in a group — was run live on `master` @ `57023f34`
(2026-11-06). It worked only after the people dug out Hub UUIDs. Three blockers:

| # | Fact | Where |
|---|---|---|
| F1 | `Contacts resolve` takes only `kind:local:<id>` / `kind:hub:<id>`, and compares the text: `peko:hub:<id>` (the CLI's spelling) misses a known participant. A name (`ada`, `@frank/ada`) is refused; models then guess `principal:hub:ada`, which PekoHub answers as malformed. | `tools/builtin/channel/contacts.rs` |
| F2 | PekoHub resolves a private peko's handle only for its owner (hub ADR-010 D1). Its Hub ID resolves for anyone signed in. | hub `instances.ts` `mayResolvePrivate` |
| F3 | Only a person can ask a group's home to join (`group_hub.rs` `join_group`, hub `handleGroupJoin`: `user:hub:` only). The `Channel` tool has no `join`; a peko that runs `peko channel join <link>` in `Bash` joins its **owner**. `peko channel invite … peko:hub:<id>` refuses a peko not hosted here. | `tunnel/group_hub.rs`, `cli/src/commands/channel.rs` |
| F4 | A private peko takes a DM or group invite from another peko only with a `Chat` grant or when the sender is already a member. Sharing a group, or the two owners knowing each other, grants nothing. The sender is told only `queued`; the invite waits in its outbox and arrives once the receiver opens the DM itself. | `daemon/state.rs` `dm_channel_mirror_bootstrap` |

## 2. Decision

### D1 — Names resolve within what you share

`Contacts resolve { reference }` takes, besides an address:

1. **An address in either spelling.** `peko:` and `principal:` name the same actor
   everywhere (the parser already agreed; Contacts now compares canonical addresses).
   Output stays `principal:`.
2. **A name: `ada` or `@ada`.** Matched, case-insensitively, against the name of every
   actor `Contacts list` shows the run: its own principal, the participants of its
   conversations, and (owner runs) learned contacts. One match is the answer. Several
   are an error listing each candidate's address, kind, and name, so the model asks or
   picks by address. None is an error that says how to learn one (D1.3, a group link, a
   Hub ID from its owner).
3. **A peko by its owner: `@frank/ada`.** A peko named `ada` whose owner's handle is
   `frank`. The actors the run already sees come first: each peko named `ada` with a Hub
   ID is checked against the directory's `ownerHandle`. Otherwise an owner run asks
   PekoHub by handle, as the peko. PekoHub answers for a public peko, the caller's
   owner's own pekos, and (hub ADR-011) a private peko whose owner shares a registered
   group with the caller's owner. The answer is learned like a Hub ID: verified, then
   kept in the contact book.

A name is only a way to find an actor; what is kept and used is the canonical address and
the actor's immutable key (ADR-081 D1). Non-owner runs resolve names only among their own
conversation's participants and learn nothing new, as before. People are found by name
only among participants: their account IDs are never looked up by handle.

### D2 — A peko joins a group by its link, itself

`Channel join { link }` takes an invite link, its token, or a public group's ID. It is an
owner decision (ADR-082 D6): refused to untrusted runs; a trusted run waits for the owner.

1. The peko's runtime asks PekoHub to look the group up, as for a person (ADR-092 D5), and
   keeps a **requested** entry for the peko: channel, home runtime, time.
2. It sends `tunnel_group_join_request` to the home with `sourcePrincipalDid` = the
   peko's signing DID and `sourceHubId` = its Hub ID. PekoHub relays a peko's request only
   from the runtime its online link points at (as it does a peko's history request).
3. The home resolves the Hub ID, checks the signing DID matches, and admits as for a
   person: a member, a live link it minted, or a public group. It adds the peko to the
   roster, invited by the group's owner, and the invite goes to the peko's runtime.
4. The peko's runtime takes that invite because the peko's request waits for it, private
   or not; the entry is then removed. An invite from that home for another channel, or
   from another runtime, is judged as before.

`peko channel join <link> --peko <name>` does the same from the terminal; without
`--peko` the person joins, and the CLI says so ("joined as you"). `peko channel invite`
reaches a peko on another runtime by its Hub ID through the inviting peko, as the
`Channel` tool does.

### D3 — A private peko takes a first DM from pekos it shares something with

A DM invite from a peko is admitted at a private peko when any holds:

- a `Chat` grant, or the sender is already a member (as before);
- the sender is in the peko's contact book (its owner learned it);
- the two pekos are members of a group together here;
- the sender has the same owner (PekoHub's `ownerSubject`);
- the two owners share a conversation here: a group both are members of, or a DM
  between the two people.

The DM's turns run at the conversation's ceiling (ADR-096): a stranger peko is
untrusted, gets no `Bash`, and reaches only that conversation. So D3 widens who may
start a conversation, not what it may do.

A **group** invite from a peko still needs a grant, membership, a contact, or the peko's
own join request (D2): being in one group with someone must not let them pull the peko into
others.

Anything else stays refused, and the invite stays in the sender's outbox: it arrives once
one of these becomes true (the outbox resends on `tunnel_ready` and when the peer comes
online). The sender is told why it may wait: for a private peko `ChannelSend` says that it
takes DMs from pekos it shares a group with, its owner's pekos, contacts its owner added,
and pekos whose owner shares a conversation with its owner.

## 3. Alternatives considered

- **Hub-wide handle lookup for private pekos.** Hub ADR-010 just closed it as an
  enumeration oracle; ADR-011 reopens it only between people who chose a group together.
- **A message request for the owner** (ADR-095 D6 for pekos): every unknown peko's first
  DM waits for the owner's Accept. The right answer for strangers, but it needs desktop
  UI; D3 covers the people who already know each other, which is the core use case.
  Open (§5).
- **Let `peko channel join` in a peko's `Bash` join the peko.** The CLI cannot tell which
  peko ran it; an explicit `--peko` and a `Channel` action are unambiguous.
- **Group invites under D3's rules.** Rejected: a co-member could pull a private peko into
  any group; a join is the peko's owner's choice (D2).

## 4. Consequences

- "Coordinate with Frank's peko ada" works when Gina's peko shares a group with ada, or
  with Frank when Frank's group is registered (`@frank/ada`), and ada takes the DM when the
  pekos share a group or Gina and Frank do.
- People in a registered group learn the names of each other's private pekos when they
  ask by handle: PekoHub's ADR-011 says so.
- Wire: `tunnel_group_join_request` gains an optional `sourceHubId` (signed). The home
  accepts a `did:key` requester only with it.

## 5. Open

1. A message request for the owner when a stranger peko's first DM arrives (desktop
   Accept / Decline; until then it waits in the sender's outbox).
2. A refusal the sender can see. Today the sender learns only that it waits.

## 6. As built (2026-11-07)

- **D1.** `ContactsTool::resolve_address` / `resolve_name` / `resolve_owned`; the
  directory answer gains `ownerHandle` (`AgentResolution::owner_handle`).
- **D2.** `Channel join` (an owner decision in `security::floor`),
  `TunnelChannelPort::join_group_as`, `admit_peko` at the home, `PekoJoinRequest` in
  `<runtime>/peko_join_requests/`, `OutboxItem::GroupJoinRequest { peko }`, and IPC
  `channel_group_join { peko }` for `peko channel join --peko`. A request is consumed by
  the home's invitation however that invitation is admitted.
- **D3.** `principal::first_contact::admits` (over `ChannelStore::shared_by`, which reads
  both tiers and remote members), called from `dm_channel_mirror_bootstrap` when no grant
  covers the sender; the reason is logged.
- Tests: `contacts_tests.rs` (names, spellings, ambiguity, `@owner/name`, hosted pekos),
  `first_contact.rs` (each rule, and that group invitations stay narrow),
  `dispatcher.rs` `a_private_peko_joins_by_link_and_takes_a_dm_from_a_groupmate` (two
  runtimes and a stranger, end to end).

*Verified live (2026-11-07)* with a local PekoHub (ADR-011 branch), two runtimes
(Frank with private peko `ada`; Gina with private peko `bix`) and Claude Sonnet 5.5:

1. Gina asked `bix` for a group `repo-x` with an invite link; Frank joined with the link.
2. Gina told `bix` to coordinate with Frank's peko ada. `bix` tried `ada` (not found,
   with the hint), then `@frank/ada`, which PekoHub answered because Gina and Frank share
   the group. `ada` was not in the group. Ada's runtime admitted the first DM ("a peko
   whose owner shares a conversation with its owner"), and the two pekos talked. No one
   copied an ID.
3. Frank sent `ada` the link; `ada` ran `Channel join` and the home added it to the roster
   as `principal:hub:<ada>`. Its owner was not joined a second time.

Found live, not fixed here: the sender's first post arrived while the receiver was still
bootstrapping the DM from its invite. It was dropped ("channel not found") and came in on
the outbox's 30-second resend, so the sender first saw `queued`. In groups, a person shows
up in a peko's roster as `local` when it has no name (sibling work on group names).
