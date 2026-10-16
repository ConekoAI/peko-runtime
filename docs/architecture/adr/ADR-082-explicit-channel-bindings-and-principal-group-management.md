# ADR-082: Explicit channel kind, bindings, and wake policy; principals manage their groups

**Status:** Proposed
**Date:** 2026-10-16
**Counterpart:** none expected — PekoHub relays signed invites verbatim and does not read the DM marker (verified: no `passive_binding` reference in `pekohub`).

**Supersedes:**
- The `passive_binding` field as the DM marker (agent-session paradigm Phase 4, ADR-049 Phase 3).
- [ADR-049](ADR-049-multi-party-group-channels.md): group creation and invitation reachable only from the CLI / IPC.

**Preserves:** [ADR-072](ADR-072-human-started-runs-and-durable-delivery.md) D1 (only humans and cron start runs) and D2 (a principal-authored post or a reply never wakes anyone), [ADR-073](ADR-073-actor-addresses-and-conversation-bindings.md) pair DM ids, [ADR-074](ADR-074-per-principal-contact-discovery.md) per-principal contacts, and [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) compartments and the trust floor.

---

## 1. Context

A channel and the principal's sessions are linked by the runtime alone. Three separate questions — *what kind of channel is this*, *which session handles its posts*, and *what starts a turn there* — are all answered by one optional field. Verified on `feature/adr-081-hub-owner` @ `03c64faa`:

| # | Fact | Where |
|---|---|---|
| F1 | A channel is a DM if and only if `meta.json` carries `passive_binding`. Nothing else records the kind. | `channel/src/store.rs:335-357` |
| F2 | Responder selection switches on that field: bound → `NoopChannelResponder`, unbound → `GroupWakeResponder`. | `daemon/channel_binding.rs:744` |
| F3 | The same field is read as the DM marker by `ChannelRead`, the channel digest, the tunnel invite and its signature, and the mirror bootstrap. | `channel_read.rs:57`, `principal/channel_digest.rs:160-215`, `tunnel/protocol.rs:477`, `tunnel/tunnel_channel_signature.rs:329` |
| F4 | A group's session is linked only from the session side (`SessionMetadata.channel_id`); `meta.bindings` has no entry for it. A DM is linked from both sides. | `principal/peer_children.rs:124-168`, `store.rs:2410` |
| F5 | The signed invite carries the *creator's local session path* (`/principal-bob`) to the other runtime, where it means nothing but "is a DM". | `tunnel/protocol.rs:1199` |
| F6 | A principal can already open a DM: the first `ChannelSend` to a principal or user provisions the `dm_<sha256>` pair channel and both bindings. | `tools/builtin/channel/channel_send.rs:316-400` |
| F7 | A principal cannot create a group, invite anyone, leave, or change when it is woken. `ChannelCreate` / `ChannelInvite` / `ChannelLeave` exist only as IPC behind `gate_principal_operator`. | `ipc/packet.rs:714-830`, `ipc/handlers/channel.rs:276` |
| F8 | A principal can already see who is in a channel: `ChannelRead` returns `participants`, and `Contacts list` returns each channel's participants (a non-owner run sees only its own conversation). Neither reports the channel's kind, creator, or the caller's wake policy. | `channel_read.rs:545`, `channel/contacts.rs:106-118` |
| F9 | A group wakes only on a user's root `@<name>` mention. A principal in a busy group cannot opt into more (every human root post) or less (never). | `daemon/channel_binding.rs:16-22` |

Two practical problems follow. A principal that needs to bring a user and another principal together must ask a human to run `peko channel create`. And every change to how channels behave has to be made by reinterpreting `passive_binding` — the field is local session data, yet it is signed and sent to other runtimes (F5).

## 2. Decision

### D1 — The channel kind is explicit

`meta.json` gains `kind: "dm" | "group"`, set at creation and never changed. `passive_binding` is deleted from `MetaJson`, `CreateOpts`, `ChannelPort`, and the tunnel invite.

A `dm` channel:
- has the id `dm_<SHA-256>` of its two members' immutable actor keys (ADR-073), and the store refuses `kind: dm` with any other id;
- has exactly those two members, for life. Invite, leave, and remote roster changes are refused. Adding a third party means creating a group.

A `group` channel has a `group:<slug>` or `chan_*` id and a mutable roster.

Every site in F3 reads `kind` instead of probing the binding. The signed invite carries `kind` in place of `passive_binding` (signed as `"dm"` / `"group"`), and the receiver checks the pair id when `kind` is `dm`. No session path leaves the runtime.

**Load-time conversion.** On first load, a `meta.json` without `kind` is rewritten once: `passive_binding` present → `dm`, absent → `group`. There is no wire compatibility: an old runtime's invite without `kind` is rejected, as ADR-073 did for its own replacement.

### D2 — A binding is a record, written by the runtime

`meta.bindings` becomes the single channel-side link, for every kind:

```json
"bindings": {
  "prin_ab12": { "session": "<session id>", "wake": "mention" }
}
```

Keys are local principal ids; bindings are runtime-local and never on the wire. `SessionMetadata.channel_id` stays as the reverse pointer. Both sides are written together under the principal's `session_creation_lock`, and every lookup (`find_dm_child`, `ensure_group_child`, the channel digest, responder selection) goes through the record. A boot sweep that finds only one side present repairs it from the session side, which is authoritative, and logs a warning.

A binding is only ever created **together with a new session** made inside the channel's compartment (ADR-078 D3), exactly as `ensure_channel_child` does today:
- DM: on the first human ingress or the first `ChannelSend`, as now (F6);
- group: when the principal is first woken there, or when it creates the group (D5).

No tool binds an existing session to a channel, and no tool removes a DM's binding. Binding an internal session would let channel members drive a session that holds the owner's private context, or leave a session in a compartment that contradicts its channel; ADR-078 makes a session's compartment immutable for exactly this reason.

### D3 — The wake policy is per binding

`wake` is one of:

| Policy | A turn starts for… | Allowed on |
|---|---|---|
| `human` | every human (user or visitor) root post | DM (fixed), group |
| `mention` | a human root post that mentions `@<name>` (ADR-072 D2) | group (default) |
| `none` | nothing; posts wait for `ChannelRead` or the unread summary | group |

A DM's policy is always `human` and cannot be changed: a user's message to a principal always starts a turn. DM turns are still driven by the ingress path (`PeerChildTurns::drive_turn_streaming`), not by the subscriber, so a DM keeps `NoopChannelResponder`. A group gets `GroupWakeResponder` with the binding's policy, or `Noop` for `none`.

Whatever the policy, a post authored by a principal and a reply (`parent: Some`) never start a turn. Those rules stay hard-coded in the responder, so no policy can reopen the principal-to-principal loop ADR-072 closed.

### D4 — Principals see their channels (`Channel info`)

Roster lookup already exists (F8). `ChannelRead.participants` remains the canonical roster and is unchanged. `Channel info { channel }` adds what the new actions need, in one call:

```json
{
  "channel": "group:launch",
  "kind": "group",
  "name": "launch",
  "creator": "principal:local:prin_ab12",
  "participants": [ /* same rows as ChannelRead.participants */ ],
  "you": { "member": true, "wake": "mention", "session": "/group-launch" }
}
```

`info` requires membership and follows the same compartment rule as `ChannelRead`: a non-owner run may read only its own conversation's channel. `you.session` is shown only to owner runs.

### D5 — Principals manage their groups (`Channel` tool)

A new domain tool `Channel` (ADR-071 shape: a required `action`, a strict schema per action, exclusive dispatch) with five actions:

| Action | Parameters | Effect |
|---|---|---|
| `create` | `name`, optional `slug`, optional `members: [actor ref]` | Creates a `group` channel with the principal as creator and first member (`group:<slug>`, else `chan_*`). Adds each member as `invite` does. Provisions the principal's group session and binding with `wake: mention`. Returns the channel id. |
| `invite` | `channel`, `actor` | Adds one actor to a group the principal belongs to. Remote members get the existing signed roster snapshot. |
| `leave` | `channel` | Removes the principal from a group. Its binding is deleted; its session is kept, unbound, as history in the same compartment. The group continues while it has members. |
| `wake` | `channel`, `policy` (`human` / `mention` / `none`) | Sets the principal's own policy on a group. |
| `info` | `channel` | D4. |

Every action except `info` refuses a `dm` channel with a structured error that names the alternative (`ChannelSend` to open a DM; `create` for a group). DMs are still opened only by `ChannelSend` (F6); the deterministic pair id is what keeps one channel per pair, so `Channel` never creates DMs.

**Whom a principal may invite.** Its owner, and principals in its own contact book (ADR-074). In v1, another human is refused with "ask your owner to add them"; the owner adds them through `peko channel invite`. Humans are never put into a group by a principal they have not chosen to talk to.

The builtin count goes from 22 to 23. `Channel` follows the new-builtin checklist (`builtin-tools.md` heading, ledger redaction rules, harness registration).

### D6 — Who may manage channels

Checked by the dispatcher from the run's `ActingFor` stamp (ADR-078), before the action runs:

| Action | Owner run | Trusted run | Untrusted run |
|---|---|---|---|
| `info` | any channel it belongs to | its own compartment's channel | its own compartment's channel |
| `wake`, `leave` | any group it belongs to | its own compartment's group | its own compartment's group |
| `create`, `invite` | allowed | waits for the owner's `approve <id>` | refused |

`wake` and `leave` are open to the run that lives in that group, because the people in a group can reasonably ask the principal to quieten down or go. `create` and `invite` change who can reach the principal and whom the principal can reach, so only the owner decides them. A trusted run's request goes through the ADR-078 owner-approval path. All five actions are recorded in the tool ledger (ADR-079) and emit one attributed `tool.call` event.

The IPC handlers keep `gate_principal_operator`; the CLI is unchanged apart from dropping `--bind` from `peko channel create`, which D2 makes meaningless.

## 3. Alternatives considered

- **Infer the kind from the id prefix (`dm_`).** No new field, but the receiver would have to recompute the pair hash to trust it, and a legacy id with no prefix could not be classified. An explicit kind, checked against the id, is both cheap and unambiguous.
- **Let principals bind an existing session to a channel.** It would let a task session "own" a project channel. Rejected for now: it moves private context into a conversation whose members can drive it, or contradicts ADR-078's immutable compartment. Reconsider for a concrete case, limited to sessions created inside that channel's compartment.
- **Many-to-many bindings (one session listens on several channels).** A session can belong to only one compartment, so this would mix conversations in one context window. Posting to many channels from one session already works through `ChannelSend`.
- **Allow unbinding or leaving a DM.** It would break "a user's message always starts a turn". A fresh conversation for a DM (archiving the old session and binding a new one) is a possible later owner action; it is not needed for this ADR.
- **A wake policy that includes principal posts.** It reopens the loop ADR-072 D1 closed, and a hop limit only bounds the loop (ADR-072 §3).
- **Let principals add any human.** Simple, but turns a principal into a way to pull people into rooms they never chose to join. A consent flow (an invitation the human accepts) could replace the v1 refusal later.

## 4. Consequences

- One field means one thing: `kind` is the channel's type, `bindings` the link to sessions, `wake` what starts a turn there. Runtime defaults (DM always wakes; groups wake on mention) become values, not special cases in code.
- No local session path is signed or sent to another runtime.
- A principal can convene a group with its owner and contacts, step out of a group, mute it, or listen to every human post in it, without a human running CLI commands.
- Non-owner runs cannot grow the principal's set of reachable actors; they can only quieten or leave the conversation they are in.
- DMs can never gain members. Tests that invite into a DM channel must create a group instead.
- Cross-runtime invites from runtimes without this ADR are rejected (pre-launch; no compatibility rollout).

## 5. Implementation

1. **Data model.** `kind` on `MetaJson` / `CreateOpts` with the id check; the binding record with `session` + `wake`; the one-time load conversion; every F3 read site moved to `kind`; responder selection keyed on `(kind, wake)`; lookups through the binding record; the boot repair sweep. Delete `passive_binding` everywhere, including `peko channel create --bind`.
2. **Wire.** The tunnel invite and its signature carry `kind`; the receiver checks the pair id for `dm`; protocol tests updated.
3. **`Channel` tool.** The five actions, strict per-action schemas, the D6 trust table in the dispatcher, the invitee rule, ledger entries, and `ToolHarness` coverage (owner / trusted-approval / untrusted-refusal, DM refusal, leave keeping the session, `wake none` silencing a mention).
4. **Docs.** `builtin-tools.md` (new `Channel` section), `DATA_MODEL.md` (`meta.json`), `API_SURFACE.md` (`ChannelPort`), AGENTS.md §6.2 and the tool count, and CHANGELOG.
