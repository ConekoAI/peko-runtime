# ADR-092: Groups have a home runtime, take people as members, keep their history, and get Hub IDs

**Status:** Accepted (2026-11-02); phases 1–4 built (2026-11-03), open items in §6 phase 4 notes
**Date:** 2026-11-02
**Builds on:** [ADR-049](ADR-049-multi-party-group-channels.md) / [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) (group channels, the `Channel` tool, who may manage them), [ADR-090](ADR-090-local-user-dms-a-remote-peko-over-the-tunnel.md) (a person on the tunnel, vouched for by their runtime and the Hub), [ADR-091](ADR-091-remote-dm-history-from-the-host.md) (history from the conversation's record, gated to the people in it; a runtime that changes hands forgets), [ADR-081](ADR-081-actor-identity-ownership-and-single-online-copy.md) (one owner per peko, online links).
**Counterpart:** PekoHub ADR-009 (one ID format per kind, exact lookup only); a new PekoHub ADR for the group registry and invite links.

---

## 1. Context

Group channels today are a mesh of principals. Verified on `master` @ `d8065407`:

| # | Fact | Where |
|---|---|---|
| F1 | A group's members are principals; a remote invite is sent only to a principal (`ActorRef::Principal`, resolved by Hub ID). A person is added to the local roster only, never across runtimes. | `tunnel_channel_port.rs` `invite` |
| F2 | Every member runtime keeps its own copy from the moment it joined; each post fans out to every member runtime. No runtime is the group's record: only the creator's copy starts at creation, and it can leave. | `fanout_event`, `ChannelStore::join_remote` |
| F3 | Roster changes travel as signed snapshots from the inviting principal (`sync_group_membership`). | `tunnel_channel_port.rs` |
| F4 | A person (`user:hub:<id>`) is accepted on the tunnel only in the two-member DM shape (ADR-090 D3); the Hub relays a person's envelope only from the runtime registered to that account. | `dispatcher::is_human_dm_invite`; PekoHub `speaksForItsOwnPerson` |
| F5 | ADR-091 serves history only for DMs, from the peko's host, to the person in it, and deletes a runtime's copies when its account changes. | ADR-091 |
| F6 | Groups have no address outside their runtimes (`group:<slug>` or `chan_*`, runtime-local), so they cannot be shared, linked, or found. | ADR-082 D1 |

## 2. Decision

### D1 — A group has a home

Every group has a **home runtime**: the runtime of the actor that created it (a peko's host, or the person's runtime when a person creates it). The home holds the group's **record** (every post since creation) and its **roster** (who may be in it). `meta.json` records `home: { runtime, owner }`; the signed invite carries both, so every member runtime knows where the record lives.

- The home serves history (D3) and decides joins (D5). Other member runtimes keep copies, as today, and keep posting peer to peer (F2): the home is not a relay, so a group works while its home is offline, and only history and joining wait for it.
- **Owner.** The group's owner is its creator: a person, or a peko (and through it, the peko's owner, ADR-081). Only the owner, or a member it makes an admin, changes the roster, the name, or the settings.
- **Moving home.** When the owner moves (a peko's online link moves, ADR-081 D7) the home moves with it: the old home sends its record to the new one (the D3 messages, in bulk) and a signed roster snapshot names the new home. If the home runtime is gone for good, the owner names a new home from a member runtime that holds a copy; history before that copy's start is lost, and the ADR says so rather than pretending otherwise.

### D2 — People are members

A person joins a group as `user:hub:<account>`, like a peko joins as `principal:hub:<id>`:

- The roster names the person by account. Each runtime registered to that account that has opened the group is a **route** for that member (as for DMs, ADR-090 D6): posts fan out to every route, and the person posts from any of them.
- A person's post is runtime-signed with no author signature, and is accepted only from a runtime listed as that person's route and relayed by the Hub from the runtime registered to that account (F4 generalized). The ADR-090 D3 exception for unsigned invites widens from "a two-member DM" to "a person's join to a group that admitted them" (D5), nothing more.
- Trust is ADR-078's: in a group a person is `Owner` for the pekos they own, `Trusted` where granted, `Untrusted` otherwise; the group's wake policy (ADR-082 D3) decides which human posts start a peko's turn.
- **The desktop**: a person sees their groups beside their DMs, posts, and reads; managing members is for the owner and admins.

### D3 — History comes from the home, to members only

The ADR-091 messages carry group pages too. The home serves a page only when **all** hold:

1. The requester is a current member: a person (`user:hub:<id>`, Hub-vouched as in ADR-091 D3) or a peko (`principal:hub:<id>`, author-signed by its key) — on a runtime that is that member's route.
2. The channel is a group whose home is this runtime.
3. The page is within what the member may see — the group's **history visibility**:
   - `shared` (default): everything since the group was created, for as long as they are a member.
   - `joined`: only what was posted after they joined.

Leaving ends access; rejoining under `shared` gives it back. Pages are stripped as in ADR-091 (no `via`, `parent`, or attachment paths). Refusals are empty pages.

### D4 — Your groups follow you

A person's groups come back on any runtime registered to their account, and on their phone:

- **Which groups.** PekoHub keeps, per account, the groups the account is a member of: each group's Hub ID and home runtime — no content, no other members (D6). A runtime newly registered to the account asks for that list and opens each group from its home: the home sends a fresh invite (roster snapshot) and the runtime catches up (ADR-091 D4: newest page, then forward).
- **Catch-up** runs on opening, `tunnel_ready`, and `peer_runtime_online` for the home, as for DMs; scroll-back reads the home and is not stored.
- **Changing hands.** ADR-091 D5 applies: a runtime whose account changes deletes the previous account's group copies along with its remote DMs (`meta.account`), refuses reads of them, and leaves its route in each group.
- **Phones** read a group's pages through the Hub's proxy to the home (ADR-091 D6), as the signed-in member.

### D5 — Hub IDs, invite links, and joining

A group may be **registered** on PekoHub, like a peko:

- **ID.** A number PekoHub assigns when the group is registered, like a Discord server ID: a 64-bit, time-ordered snowflake written in decimal (`1203948571029384756`). Groups live in their own ID space, apart from people and pekos (`@…`) and seeds (`owner/name`), so a bare number is always a group. The **name** is free text, needn't be unique, and can change; the ID never does. Page `pekohub.ai/g/<id>`; the search box reads an all-digit query as a group ID (PekoHub ADR-009 D4). The runtime keeps its own channel id (`group:<slug>` / `chan_*`) and records the number in `meta.hub_id`.
- **Visibility.** `invite-only` (default): joining needs an invite link or the owner's approval. `public`: anyone signed in who has the ID may join. Never listed or searched by text (PekoHub ADR-009 D2).
- **Invite links.** The owner (or an admin) mints a link on the home runtime: a token signed by the home, with an optional expiry and use count, listed and revocable there. `pekohub.ai/g/<id>?invite=<token>` opens the group's page on the website, which shows its name and owner and how to join in Peko.
- **Joining.** The person's runtime sends a signed join request (the person, the token if any) to the home over the tunnel. The home checks the token or the visibility, or asks the owner (the ADR-078 owner-approval path: desktop Approve/Deny, `peko approve`), then adds the person to the roster and sends the invite to every runtime registered to that account that is online. PekoHub records the membership for D4.
- **Unregistered groups** keep working exactly as today, among their members, with no ID.

### D6 — What PekoHub keeps

For a registered group: its Hub ID, name, owner, home runtime, visibility, and the accounts that are members (for D4 and for checking a person's group posts in the relay). Never posts, never peko members, never history. Membership is kept so a person's devices can find their groups; it is shown to no one but that person. An unregistered group leaves nothing on the Hub.

## 3. Alternatives considered

- **No home; every member serves what it holds.** History would be stitched from whichever copies are online, with gaps, and no runtime could decide joins. A home is one answer to "where is the record" and "who admits people".
- **The home relays every post.** Simpler consistency, but the group would stop when the home's computer sleeps. Peer-to-peer posting stays; only history and joins wait for the home.
- **PekoHub stores group history.** Rejected for the same reason as ADR-091: the Hub holds no messages.
- **Handle-scoped names** (`@alice/+team`). Names would have to be unique per owner and could not change without breaking links; a group also outlives the person who made it more often than a peko does. A number assigned once avoids both, as Discord's server IDs do.
- **History visibility `joined` by default.** Safer for what was said before someone joined, but every chat app people use (Discord, Slack, Matrix by default) shows a group's past to its members; the owner can choose `joined`.

## 4. Consequences

- Groups gain people, history, an address, and invite links, at the cost of a home whose computer must be online for history and joins.
- PekoHub learns which accounts belong to which registered groups. It is kept for the member alone and never shown, but it is social metadata the Hub did not hold before; unregistered groups avoid it.
- Moving a home when its peko moves is new machinery; a home lost for good loses the history no remaining copy holds.

## 5. Decided at review (2026-11-02)

1. **ID**: a Hub-assigned number (D5), not a handle-scoped name.
2. **History default**: `shared`.
3. **Membership on the Hub** (D6): kept, for the member alone.

## 6. Implementation outline (phases, each its own PRs)

**Phase 1 notes (as built).** Receivers learn the home from the first roster
snapshot's signed `source_runtime_id`, since invites leave a group only from
its home, so no wire field was added; they record only the home runtime, and
the owner and admins are known and enforced at the home. Moving the home with
its owner comes in phase 3, which already moves records. `group:<slug>` ids are
unique only per runtime: two runtimes' `group:launch` used to merge in a third
runtime's copy; now the second one's roster is dropped there, and a group meant
to span runtimes should take a generated `chan_*` id until phase 4 gives it a
Hub ID.


1. **Home and roster** — `meta.home`; invites carry the home; owner/admin roster changes; moving home with the owner.
2. **People as members** — group invites and joins for `user:hub:<id>`; routes per account; the relay check generalized; desktop group view and posting.

   *Phase 2 notes (as built).* The home cannot address a person's runtimes (envelopes are signed per recipient runtime, and only PekoHub knows which runtimes an account has), so it sends `tunnel_group_invitation` to the **account**, and PekoHub delivers it to every runtime registered to it. No one is put in a group without saying yes: each of those runtimes keeps it as a **pending invitation** until the person accepts or declines (desktop sidebar; `peko channel invitations|join|decline`). The answer, `tunnel_group_join`, goes to the home from one runtime, which PekoHub relays only from a runtime registered to that person; on accept the home records that runtime as the person's route and sends the roster to every route. A roster addressed to the person is taken only for a group they joined here, or one whose invitation waits here and which the roster shows they joined from another of their runtimes. A person joining from a roster trusts the home's signed claims about who the other members are. Groups created by a person come with phase 4.
3. **Group history** — D3 on the ADR-091 messages; history visibility; catch-up and scroll-back for groups; phones.

   *Phase 3 notes (as built).* The home answers the ADR-091 history messages for a group: to a person who is a member, by their Hub-vouched account (any of their runtimes, so a new device can read too), and to a peko member by its signing DID from the runtime that is its route (PekoHub relays a peko's request only from the runtime its online link points at). `Channel history { channel, visibility }` sets `shared` / `joined` at the home (owner or admin; an owner decision under ADR-082 D6); under `joined` a member reads only posts at or after the `MemberJoined` that admitted them. A person's group catches up on joining, on `tunnel_ready`, and on `peer_runtime_online` for the home; a peko's runtime catches up when the peko is added to a group that already talked; the desktop's group view reads earlier pages from the home on request, unstored. A phone's route for groups needs a group's Hub ID, so it comes with phase 4. Moving a home when its owner moves is still open (§4).
4. **Hub IDs and invites** — PekoHub group registry, pages, invite links; join requests; per-account group list (D4); desktop deep link and Join.

   *Phase 4 notes (as built).*
   - **Registering.** `Channel register { channel, visibility }` (also `peko channel register`, desktop Share) runs at the home, by the owner or an admin, as an owner decision (ADR-082 D6). The home sends `group_register` with the owner's Hub address; PekoHub refuses unless the owner is a peko whose online link is that runtime, or the person the runtime is registered to. It answers `group_registered { groupId, pageUrl }`, which the home keeps in `meta.hub` with the visibility. Registering again changes the visibility and keeps the ID.
   - **Invite links.** `Channel link { channel, days }` mints a compact JWS signed by the home runtime, `{ v: "peko-group-invite/1", g, c, h, exp, n }`, good for 1–30 days (default 7); the link is `pageUrl?invite=<token>`. The home keeps each nonce; `link { revoke: true }` stops them all. PekoHub checks a token's signature and expiry only to show an invite-only group's page; the home decides joins.
   - **Joining.** A person pastes a link or a public group's ID (desktop Join a group, `peko channel join <link|id>`). Their runtime asks PekoHub (`group_lookup_request` → `group_lookup`), which shows a public group, or an invite-only one only with a valid invite. It then keeps a *requested* entry (not shown as an invitation) and sends `tunnel_group_join_request` (signed by the runtime, with the token) to the home. PekoHub relays it only from a runtime registered to that person. The home admits a member, a live link it minted, or anyone to a public group, adds the person with that runtime as their route, and sends the roster. The requested entry takes the roster as the home's yes; a refusal leaves it waiting, and nothing arrives.
   - **Devices (D4).** After every change to a registered group's roster, the home sends `group_roster { groupId, accounts }` with the persons who joined (a route, or its own local user), never invitees. On `tunnel_ready` a runtime asks `account_groups_request`. For each group of its account that is not here, it sends the same join request without a token, which the home admits because the person is a member. The group then catches up from the home (phase 3).
   - **Phones.** `GET /v1/groups/:id/history` proxies a member's read to the home in the owning peko's name. The home checks membership and the history setting, not the peko's exposure.
   - **Open.** These are not built:
     - links carry no use count, and are revoked all at once;
     - an invite-only group refuses a link-less request instead of asking the owner;
     - only a peko can own a registered group, because roster snapshots are signed by the owning peko;
     - there is no desktop deep link: the website tells the person to paste the link in Peko;
     - moving the home with its owner (D1) remains open.
