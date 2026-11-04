# ADR-093: Suggested replies — a peko may offer a few short answers; a tap is just the person's message

**Status:** Accepted (runtime, PekoHub, and peko-desktop: `feat/suggested-replies`; group channels open, see §5)
**Date:** 2026-11-04
**Counterpart:** peko-desktop — reply chips under the peko's latest message. PekoHub webchat may render them later; until then it ignores them.

**Preserves:** [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (owner decisions are buttons and CLI verbs, never messages), [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) (the channel is the conversation record), [ADR-088](ADR-088-attachments-in-direct-messages.md) (extra data rides on a post and is omitted when empty).

---

## 1. Context

Chat products for agents let the agent show a card with choices. Peko already has one card: the approval request. The question is whether to turn that into a general card that agents can send, without turning conversations into forms and without making the CLI harder to use.

Verified on `master` @ `906d449a`:

| # | Fact | Where |
|---|---|---|
| F1 | An approval is created by the **runtime**, not the agent: it is stored in `approvals/<id>.json`, tied to the digest of one call, and decided only by the owner through `approval_decide` (desktop Approve/Deny, `peko approve` / `peko deny`). No message can decide it. | `principal/policy_gate.rs` |
| F2 | A transfer offer is another decision the runtime waits on from a person (pending on the Hub until accepted). | #558, `ipc/packet.rs` `PrincipalTransferOffered` |
| F3 | A conversation reply is the run's final text. It is posted to the channel after the run as a plain post (`PostMsg::root(text)`) by every reply path: peer turns, continuations, completion wakes, cron. | `principal/peer_dm.rs` `post_peer_dm_reply`, `daemon/{continuations,completion_wake}.rs`, `daemon/cron_engine/mod.rs` |
| F4 | `ChannelSend` posts explicit messages (`to`, `text`, `parent`, `label`). It is not how a peko answers the person it is talking to. | `tools/builtin/channel/channel_send.rs` |
| F5 | `ChannelEvent::Posted` has grown optional fields (`via`, `msg_id`, `reply_to`, `attachments`), each omitted when empty. The protocol does not deny unknown fields, so older readers ignore new ones. | `peko-protocol/src/channel.rs` |
| F6 | `Attach` finds the run's conversation from its stamp's compartment and refuses runs that are not in a conversation. | `tools/builtin/channel/attach.rs` |
| F7 | People answer a peko only with messages. Runtime keyword commands (`stop`, `approve <id>`) were removed so that the model interprets what people say. | 2026-10-31 natural-chat change |

So there are two different things that could be called a "card":

- **Owner decisions** (F1, F2) come from the runtime and carry authority. Their value is that a run cannot forge or argue its way past them.
- **Choices an agent offers** carry no authority. They are a quicker way for a person to say something they could also type.

Merging the two would let agents draw the UI that people trust for permissions, and it would give every agent question a pending state, an answer path, and a CLI verb.

## 2. Decision

### D1 — Owner decisions and suggested replies stay separate

This ADR adds only suggested replies. Approvals and transfer offers stay as they are: created by the runtime, decided only by the owner, with a CLI verb for each. There is no general agent-authored card, form, required field, or blocking question.

If owner decisions are ever unified (one "waiting on you" list for approvals, transfer offers, invites), that list holds only items the runtime created. It is a separate decision.

### D2 — A suggested reply is text on a post

`ChannelEvent::Posted`, `PostMsg`, and `PrincipalLogMessage` (with `principal_log_watch` rows) gain `options: [string]`, omitted when empty. Old lines and posts without options stay byte-identical.

Limits are checked by the posting store (`peko_channel::validate_options`): 2–4 options, each 1–60 characters on one line after trimming, no duplicates. A local post with invalid options is refused as a whole, so the agent sees the error. A peer runtime's post with invalid options keeps its text and loses the options, so another runtime cannot hand our clients an arbitrary number of buttons.

The field mirrors to other runtimes like any other part of the post (F5), and served DM history keeps it. A reader that does not know the field shows the message without options and loses nothing.

### D3 — Agents add options two ways, one for each way they post

- **An explicit post:** `ChannelSend` gains `options`. Notes to users are delivered into a conversation session rather than posted, so they refuse it.
- **The run's reply:** a new built-in, `Suggest { replies: [string] }`. It posts nothing. It stages the list in memory for the run's conversation, keyed by channel and principal, and the reply post takes it (F3: every reply path ends in `post_peer_dm_reply`). If `Suggest` is called more than once, the last call wins, and `[]` clears. The staged list is dropped when the reply is empty, when the policy reviewer withholds the reply, and when anyone else has posted in the conversation since the call. That way a run that never replied cannot hand its options to a later reply. Like `Attach` (F6), `Suggest` is refused for a run that is not in a conversation (cron, internal work). It is also refused for a run that is not the conversation's bound session: an `Agent` worker shares the conversation's compartment but reports back, so it has no reply of its own there.

There is no trust gate: an option is only text that a person may choose to send, so `Suggest` is exempt from the policy reviewer. Calls go to the tool ledger like every other call (ADR-079).

### D4 — A tap is the person's message

Tapping an option posts its text exactly as if the person had typed it, through the usual send path. There is no option id, no structured answer, and no "answered" record. If the peko is busy, the tap becomes steering like any other message. The text box stays available, and a free-text answer ("the second, but rename it first") is always fine. The model knows what it offered because the `Suggest` call or `ChannelSend` options are in its own session.

A tap that reads "Approve" or "Allow" decides nothing. Only `approval_decide` decides an approval (F1).

### D5 — Options show only while they are the latest thing said

Clients show options only on the conversation's last post, and only when the peko wrote it. Once anyone posts after it, the options are gone. The runtime keeps no pending state and no expiry for them. Any indicator such as "the peko asked you something" is worked out the same way, on the client.

The desktop puts them under the peko's bubble as reply chips, styled differently from the approval card, which keeps its own look, its requester, and its reviewer verdict.

### D6 — The CLI shows a hint and needs nothing new

`peko send` prints the options after the streamed reply, and `peko log` prints them under the message:

```
↳ Ship it · Hold off
```

`peko send` writes the hint to stderr, so stdout stays the answer text alone; the streamed answer's final packet (`principal_sent_done`) carries `options`. `--json` output includes `options`. To answer, the person runs `peko send` with the text, as with any other reply. There is no new verb and no number-picking protocol.

### D7 — Principals read options as data

`ChannelRead` shows a post's `options`. A peko that reads them answers in text, the same way a person typing would.

### D8 — Guidance keeps them rare

The `Suggest` and `ChannelSend` descriptions, plus one line in the seeded `conversation` role, say to offer options only when the next message is likely to be one of a few short answers ("Ship it" / "Hold off", "Tuesday" / "Thursday"). Not for open questions, and never as a way to get permission: that is the runtime's approval request. Seeded roles are created only if missing, so existing principals get the guidance from the tool descriptions only.

## 3. Alternatives considered

- **Generalize the approval card into agent-authored cards** (choices, forms, required answers, blocking until answered). Agents could draw UI that people trust for permissions. Conversations would turn into wizards. Each card would need a CLI verb and agents would need a second way to receive answers. This is the feature creep the decision avoids.
- **Options only on `ChannelSend`.** This misses the main case: in a conversation, the reply is the run's final text, not a `ChannelSend` (F3).
- **Mark options in the reply text** (for example, a trailing fenced block that the runtime strips out). The desktop streams the reply before the block can be parsed, the markup would leak to the CLI, to other runtimes, and to older clients, and parsing model output by convention is brittle.
- **Taps carry an option id or a structured value.** That adds a second input path for a case that free text already covers. The model can read what it offered.
- **Pending-question records with a runtime "waiting on you" list.** Not needed for now: clients can work it out (D5). If owner decisions get a list, it holds runtime-created items only (D1).

## 4. Consequences

- On the desktop, people can answer a simple choice with one tap. Typing still works everywhere.
- On the CLI, nothing changes except a hint line.
- Agents learn one optional tool and one optional parameter. Answers still arrive as ordinary messages.
- Options show to anyone in the conversation, including group members and people on other runtimes when their client supports them. They carry no authority, so a misleading option from a hostile peer can at worst lead someone to send text they chose themselves.
- Built-ins go from 24 to 25.

## 5. Implementation

Steps 1–8 are built on `feat/suggested-replies` (runtime, PekoHub's tunnel types, peko-desktop). The desktop shows chips in direct messages, local and remote pekos alike. Group channels do not show them yet: in a group a peko wakes on mentions by default, so a tapped reply from a person would not reach it. Whether a tap should mention the peko or thread onto its post is still open.

1. **Wire.** `options` on `Posted`, `PostMsg`, `PrincipalLogMessage`, and watch rows. Validate in the channel store's `post`. Update DATA_MODEL. Add a round-trip test showing lines without options stay byte-identical.
2. **`ChannelSend`.** Add the `options` parameter, the description line, and validation errors.
3. **`Suggest`.** Add `tools/builtin/channel/suggest.rs`. Keep a run-scoped slot keyed by the run's session. Every reply path in F3 reads the slot and puts the options on the reply post. Follow the new-built-in checklist (ADR-079): ledger, `builtin-tools.md` entry under Channels, and the AGENTS.md tool count.
4. **Role.** Add one line to the seeded `conversation` role (D8).
5. **CLI.** Add the hint line to `peko send` and `peko log`, and `options` to `--json`.
6. **`ChannelRead`.** Show `options` on posts.
7. **Desktop (counterpart).** Chips under the peko's latest bubble. A tap sends the text through `principal_send`. Hide the chips once anything is posted after the message. Keep them visually distinct from the approval card.
8. **Tests.** `ToolHarness` tests for `Suggest` and `ChannelSend` validation. A reply carries the run's last `Suggest` and an empty reply drops it. A refusal outside a conversation. A cross-runtime mirror keeps `options`.
