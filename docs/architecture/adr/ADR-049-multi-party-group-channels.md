# ADR-049: Multi-Party Group Channels (Users + Principals)

**Status:** Accepted
**Date:** 2026-08-29
**Author:** rlsn
**Related:** [ADR-048](ADR-048-channel-native-cli-surface.md) (partially
superseded — group-channels section), [ADR-044](ADR-044-chat-session-separation.md)
(peer DM channels), [ADR-042](ADR-042-no-external-session-concept.md) (privacy
rule), [ADR-031](ADR-031-agent-team-membership.md) /
[ADR-033](ADR-033-ownership-and-permission-model.md) (ownership model).

**Note:** This is a clean-slate pre-production design. Backward compatibility
with Peko 0.1.0 is intentionally discarded so the codebase and UX remain
coherent.

---

## 1. Context

ADR-048 scoped groups as *principal-authored spaces*: the channel IPC
authorizes writes against a member principal sender, no user-authored post
path existed, and `peko send group:<slug>` was refused with a pointer to
`peko channel post … <sender-principal>`. That posture was recorded as a
designed boundary with a sketched future change (ADR-048:141-145), not as the
end state.

An audit of the group-channel implementation (2026-08-29) showed the gap is
wider than the refusal — the `group:` wire form is pre-launch plumbing
without a mint:

- **`group:<slug>` channels cannot be created.** `peko channel create`
  always mints a bare `chan_*` id
  (`peko-rs/cli/src/commands/cli_handlers.rs:83-85` →
  `peko-rs/channel/src/store.rs:967`); `CreateOpts::with_id` is used in
  production only for peer DMs (`peko-rs/core/src/principal/peer_dm.rs:174`);
  `ChannelId::for_group` (`peko-rs/protocol/src/channel.rs:125`) is
  referenced only by tests.
- **Membership is principal-typed end-to-end.** `invite` takes
  `PrincipalId`s (`store.rs:1010-1039`), `list_members` returns
  `Vec<PrincipalId>` (`port.rs:113`), `members.json` holds principal-id
  strings. Users cannot be members; on DMs the human peer is deliberately
  excluded (`port.rs:64`).
- **Authorship is an untyped string.** `ChannelEvent::Posted.author`
  (`protocol/src/channel.rs:244-250`) carries a bare `String`; consumers
  infer user-vs-principal by ad-hoc parsing (e.g.
  `peko-rs/core/src/daemon/channel_binding.rs:243`).
- **No user write path.** `ChannelPost` requires a principal sender name
  (`peko-rs/core/src/ipc/handlers/channel.rs:274-285`); the attribution
  escape hatch `ChannelPort::post_attributed` (`port.rs:56-82`) exists but
  is unwired from IPC and CLI.
- **No wake on group traffic.** Unbound channels get
  `NoopChannelResponder` (`channel_binding.rs:495-522`); member principals
  can only participate by polling (`cron` + `ChannelRead`).
- **No read authorization.** `peek` has no membership check
  (`store.rs:1068-1076`) and the channel IPC handler ignores `CallerContext`
  (`ipc/handlers/channel.rs:148-154`) — ADR-048's documented known gap.

The product direction is that a group is a **multi-principal, multi-user**
channel — the Slack-with-bots model — where both users and principals read
and write asynchronously. This ADR records that decision and the roadmap to
close the gaps above.

## 2. Decision

### D1 — Groups are multi-principal, multi-user channels

Both users and principals are first-class participants who read and write
asynchronously. The user (human) surface is `peko send` / `peko log`; the
principal (agent) surface is the `ChannelRead` / `ChannelSend` tools.

### D2 — Membership is `Subject`-typed

`members.json` accepts both `principal:<did>` and `user:<id>` entries.
`invite`, `check_membership`, and `post` authorize against
`peko_subject::Subject`, not `PrincipalId`. Users post **as themselves**;
no vouching member principal is required. This resolves ADR-048's open
question ("which member principal authorizes a human's write") by making
the vouch unnecessary — membership itself is the authorization.

### D3 — Authorship is `Subject`-typed at the boundary

The wire field `ChannelEvent::Posted.author` stays a `String` (serde
compatibility), but writers produce canonical `Subject` wire forms
(`user:<id>` / `principal:<did>`) and consumers parse via `Subject` instead
of ad-hoc string matching.

### D4 — Wake policy (loop-safe)

- A `user:*`-authored **root** post in a group channel wakes every member
  principal, each in its own per-`(principal, channel)` session.
- A `principal:*`-authored post **never** wakes other members. Principals
  keep reading the channel on their own rhythm (cron + `ChannelRead`) and
  deliberately decide when to `ChannelSend`.
- `@mention`-triggered wake for principal-authored posts is explicitly
  deferred.

The loop risk in a multi-principal channel comes from principals reacting
to principals; that path stays forbidden, so the anti-feedback-loop
property of paradigm §3.1 / ADR-048 is preserved structurally, while humans
— scarce, slow, and expecting responses — get the reactive behavior they
expect from a group chat.

### D5 — Mint path for `group:<slug>`

`peko channel create --id group:<slug>` and an `id` field on the
`ChannelCreate` IPC packet map to `CreateOpts::with_id`, so named group
channels can actually exist.

### D6 — Read authorization

`peek` and `ChannelEventsWatch` become membership-gated against the
caller's identity, and the channel IPC handler consults `CallerContext`.
This closes ADR-048's known gap (group log reads bypassing membership
checks).

### D7 — CLI outcomes

- `peko send group:<slug> <msg>` posts as the caller's user identity
  (`-U/--user`). `--wait` / `--model` stay refused (a wake fans out
  to one run per member principal — there is no single run to await
  or steer).
- `peko log group:<slug> [--watch]` is unchanged in shape, now
  membership-gated per D6.
- `peko stop group:<slug>` stays refused for now: a wake creates one run
  per member principal, so there is no single run to stop. Per-member stop
  is future work.

## 3. Roadmap

Phases are independently testable and land in order.

- **Phase 1 — Subject-typed core.** Generalize `MembersJson`, `invite`,
  `check_membership`, `post` / `post_attributed`, and `list_members` in
  `peko-rs/channel/` to `Subject`; adjust `ChannelPort` signatures
  (`peko-rs/channel/src/port.rs`) and the IPC handler's principal-only
  resolution (`peko-rs/core/src/ipc/handlers/channel.rs`);
  `peko channel invite` accepts `user:<id>`.
- **Phase 2 — Mint + user write path.** `--id` on create (CLI +
  `ChannelCreate` packet); a `Subject` sender / `author` field on
  `ChannelPost`; un-refuse `peko send group:<slug>` (posts as the caller's
  user; the refusal tests at `peko-rs/cli/src/commands/send.rs:591-627`
  become success-path tests); membership-gate `peek`.
- **Phase 3 — Group wake.** Extend `select_responder`
  (`peko-rs/core/src/daemon/channel_binding.rs:495-522`) with a group
  responder that triggers only on `Subject::User` root posts;
  per-`(principal, channel)` child sessions mirroring the peer-DM child
  mechanism (`peko-rs/core/src/principal/peer_children.rs`); the reply is
  posted back to the group channel.
- **Phase 4 — Hardening + docs.** `CallerContext`-based authorization in
  the channel IPC handler; `ChannelRead` tool description/error text
  mention `group:<slug>` (`channel_read.rs:47,105-110`); `peko log --watch`
  on groups via the now-gated `ChannelEventsWatch` (or keep the 2s poll —
  decide at implementation); revisit `FAN_OUT_CAP = 8` (`store.rs:71`) for
  multi-user groups; fix `CLI_REFERENCE.md:457-459` drift (the nonexistent
  `channel config` / `channel pin` rows); update
  `docs/architecture/builtin-tools.md` and the user guide.

## 4. Consequences

**Positive:**

- The user surface matches the mental model: a user speaks in a group as
  themselves via `peko send`, exactly as they do in a DM.
- Loop safety is preserved by construction (D4): the principal-reacts-to-
  principal path remains forbidden, so multi-user adds no feedback risk.
- The authorization gaps around channel reads and IPC callers get closed
  as part of the design rather than as afterthoughts.

**Negative:**

- Wire-shaped changes to `ChannelCreate` (new `id` field) and `ChannelPost`
  (subject sender / `author`) — acceptable pre-launch.
- Per-member wake sessions add daemon state (one child session per
  `(principal, channel)` pair that has seen user traffic).
- Multi-user membership pressures `FAN_OUT_CAP = 8`; the cap is revisited
  in Phase 4.
- `peko stop group:<slug>` remaining refused is a UX wart once wakes exist;
  per-member stop is noted as future work (D7).

## 5. References

- `peko-rs/channel/src/{store,port,cursors,subscription}.rs` — channel core.
- `peko-rs/protocol/src/channel.rs` — `ChannelId` / `ChannelEvent` wire types.
- `peko-rs/core/src/daemon/channel_binding.rs` — subscriber/responder and
  the wake policy.
- `peko-rs/core/src/ipc/handlers/channel.rs` — channel IPC surface.
- `peko-rs/cli/src/commands/{send,log,stop,channel}.rs` — user CLI surface.
- [AGENT_SESSION_PARADIGM.md](../AGENT_SESSION_PARADIGM.md) §3.1 — the
  passive/active communication model this ADR extends.
