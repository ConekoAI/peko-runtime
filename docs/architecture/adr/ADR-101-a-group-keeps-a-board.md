# ADR-101: A group keeps a board: work is claimed and tracked there, and every turn reads it before it speaks

**Status:** Accepted (implemented on `feat/adr-101-group-board`)
**Date:** 2026-11-09
**Amends:** [ADR-092](ADR-092-groups-with-a-home-people-and-hub-ids.md) D3. The `joined` history visibility is retired, and every member reads the whole group (D8).
**Builds on:**
- [ADR-092](ADR-092-groups-with-a-home-people-and-hub-ids.md): a group has a home and a record, its members keep copies, its history follows people.
- [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md): bindings and wake policy; principal posts never wake.
- [ADR-083](ADR-083-quiet-ticks.md) / [ADR-084](ADR-084-goals-as-plans-and-check-ins.md): the keepalive calls no model when nothing waits; a check-in wakes it once.
- [ADR-096](ADR-096-peer-posts-are-handled-where-they-arrive.md): peer posts are answered in their own conversation.
- [ADR-100](ADR-100-owner-requests-in-shared-conversations-run-as-the-owner-once-approved.md): owner tasks report back to the group.
- [ADR-093](ADR-093-suggested-replies.md) / [ADR-097](ADR-097-owner-picks-a-remote-pekos-model-per-message.md): optional fields added to `Posted` without breaking older runtimes.

---

## 1. Context

Core use case 2 has two people and their two pekos working on one repo in a group of four. The 2026-11-06 → 11-08 field tests got it to pass end to end. Most of the friction left is coordination: everyone works out who is doing what by reading and writing chat.

- Two runs of one peko (the owner's DM turn and a group keepalive turn) posted contradicting status in the group (2026-11-07). The overlap guard caught it only after the fact. It made the second run rewrite its reply, and it only compares a peko with itself.
- Status travels as prose posts ("running the tests now", "done, 1 failure"). Each one is a post the other peko reads, may answer, and pays for in context. A person who was away has to read the whole log to learn where things stand.
- Nothing stops both pekos from starting the same work. In e104 it worked out because rex waited for kit's test report, which is luck rather than design.
- Blocked work goes quiet. A peko that says "waiting on Noah's review" has nothing that brings the item back when the wait drags on.

The direction we chose (2026-11-09): inside a group, coordinate through shared state, and keep posts for intent, questions, decisions and results. Work is claimed and its status changes in one place that everyone, person or peko, reads. Messaging stays the interface *between* trust domains (DMs, and a group's members to one another). The board is shared state *within* one group's trust boundary.

Verified on `master` @ `0fce246a`:

| # | Fact | Where |
|---|---|---|
| F1 | `Plan` is owner-only: plans have no compartment, so any other run would reach the owner's plans. A group run cannot use plans, and they are per principal, not shared. | `security/floor.rs` `OWNER_ONLY_TOOLS` |
| F2 | A group's record is the channel log at its home. Members keep copies, posts fan out peer to peer, history and the group itself follow a person to their other computers. | ADR-092 D1, D3, D4 |
| F3 | `ChannelEvent::Posted` has gained optional fields (`options`, `model`) that older runtimes ignore. The enum has no `deny_unknown_fields`. A new event *kind*, though, would fail to parse on an older runtime. | `peko-protocol/src/channel.rs` |
| F4 | Principal posts never wake anyone. A group binding wakes on `human` / `mention` / `off`. | ADR-082 D3 |
| F5 | The overlap guard compares a turn's reply with the peko's own posts from its other sessions and asks the model to rewrite. It knows nothing of other members' work. | `principal/genesis.rs` `conversation_overlap_message`, `child_turns.rs` |
| F6 | A conversation turn sees new posts through the channel digest in `<runtime-context>`. There is no structured state of the group beyond roster and settings (`Channel info`). | `principal/channel_digest.rs`, `tools/builtin/channel/channel_tool.rs` |
| F7 | Conversation turns run the seeded `conversation` role unless the binding names another. | `resources/roles/conversation/ROLE.md`, ADR-087 |
| F8 | A group's `history` is `shared` (the default) or `joined`. The home serves a `joined` member only posts after their join. The owner sets it with `Channel history`, an owner-only action. | `peko-channel` `store.rs` `HistoryVisibility`; `tunnel/dispatcher.rs` (history serving); `security/floor.rs` `CHANNEL_OWNER_ACTIONS` |
| F9 | The quiet tick counts a plan check-in that came due since the job's last turn as something waiting, so the attention keepalive runs one turn. A future `check_at` snoozes a step. | `principal/quiet_tick.rs`, ADR-084 |

## 2. Decision

### D1: The board is folded from the group's own log

A `Posted` event may carry an optional `board` op. The board is the deterministic fold of every op in the log. There is no new store, no new event kind, and no new replication. The board is signed, ordered, mirrored, served as history, and followed to other computers exactly as posts are (F2, F3). Since every member reads the whole group (D8), every member folds the same board.

An **item** is one piece of work: a title (at most 120 characters), a state, an optional claimant, an optional note (at most 500 characters), and an optional check-in (D6). Its id is the `msg_id` of the post that added it, so ids are unique across runtimes without coordination.

| Op | Effect | Who may |
|---|---|---|
| `add { title, for? }` | New item, state `open`. `for` names a member it is asked of. | Any member |
| `claim { item, check_at? }` | Claimant = author, state `doing`. | Any member, for themself only |
| `update { item, state, note?, check_at? }` | State `doing` / `blocked` / `done`, with a note. | The claimant; the adder; the group owner or an admin |
| `release { item }` | Claimant cleared, state `open`, check-in cleared. | The claimant; the group owner or an admin |
| `drop { item }` | Item removed from the board. | The adder; the group owner or an admin |

Authority comes from the post's verified author (ADR-081 keys), never from the op's contents. A runtime refuses an op its author may not make before posting. The fold ignores one from a remote copy, so a modified runtime cannot claim for others or close someone else's work.

**Races.** Claims are advisory: a claim is a trace, not a lock. When two claims on one item cross in transit, the earlier `at` wins, with ties broken by `msg_id`. Every copy reaches the same result once it holds both posts. The losing claimant's next turn sees the item as someone else's (D3). Its `Channel board claim` call, if it was still in flight, is answered with "already claimed by rex at 13:02".

### D2: A board op is a post people can read

Every board post's `text` is written by the runtime from the op ("rex took: fix even-length median", "kit: run the tests → done: 1 failure in test_median_even"). Then:
- an older runtime or client shows a readable line;
- the record stays one log;
- a person can follow the board in the chat.

The desktop shows these posts as compact board lines rather than chat bubbles, and shows the folded board beside the group (D7).

**Board posts wake no one,** whatever the binding's wake policy (F4). They are traces, read on the next turn that runs for any reason. There are two exceptions:
- `add` with `for: <peko>` counts as a mention of that peko, so its `mention` binding wakes it. This is how a person hands a task to a peko from the board.
- A check-in coming due wakes the claimant once (D6).

### D3: Every turn in the group reads the board first

A turn in a group-bound session gets a `<board>` section in its `<runtime-context>`, next to the channel digest (F6):
- open, doing and blocked items, each with its claimant;
- items done in the last 24 hours;
- items claimed by this peko first, with due check-ins marked "check-in due";
- the newest at most 40 items, capped at 2 KB, with a pointer to `Channel info` for the rest.

The section is omitted when the board is empty, so groups that don't use a board pay nothing.

Item titles and notes are members' words, just like posts. They are rendered quoted and attributed, at the same trust as the digest. A board entry is never an instruction to the runtime.

### D4: Pekos claim before they work and report on the item

`Channel` gains action `board` (`op`: `add` / `claim` / `update` / `release` / `drop`). It is not in `CHANNEL_OWNER_ACTIONS`: writing the board is a kind of posting, open to any run that may post in the group. A board op appears in the ledger like any call. `Channel info` returns the folded board.

`Channel` has one more action and no new tool, so the catalog is the same for every run and the prompt cache is unaffected (as for ADR-100's `as_owner`).

The seeded `conversation` role (F7) gains a short section on how pekos work in a group:
- Before starting work someone asked for, claim the item, or add it and claim it. If someone else holds it, leave it to them or ask in the chat.
- Report progress by updating the item, not by posting. Post in the chat for questions, decisions, and results worth discussing.
- When blocked, say on what in the note and set a check-in for when it is worth looking again.
- When work finishes, mark it `done` with a one-line outcome. Post more only if the people need to read more.

### D5: Owner tasks and other runs share the item

An ADR-100 brief may name the item it serves. The group run claims the item *before* it hands the task up, and updates the item when the report arrives. The approved owner run itself never writes to the group (ADR-100 D3). Other sessions of the same peko see the claim in their next turn's `<board>`, so a second run no longer reports conflicting status (F5). The overlap guard stays as the backstop.

### D6: A blocked item comes back up at its check-in

`claim` and `update` may set `check_at`: an RFC 3339 time at least 10 minutes and at most 30 days ahead. An `update` to `blocked` must carry a check-in. When it is left out, the runtime sets one 24 hours ahead and says so in the board post. An `update` to `done` and a `release` both clear it.

When a check-in comes due on an item a **peko** holds, the item resurfaces exactly once, the way plan check-ins do (ADR-084, F9):
- **Only the claimant's own runtime acts on it.** The quiet tick counts the due check-in as something waiting in that group (ADR-083), so the attention keepalive runs one turn in the group's bound session.
- **That turn uses the conversation's natural stamp**, like a completion wake: the group's ceiling, with no `owner_request` (ADR-100 F7). A check-in therefore never starts owner-level work. If the work needs it, the turn can only ask, through the usual paths.
- **The turn's `<board>` marks the item "check-in due".** The peko then does one of four things: moves the item on, sets a new check-in, releases it, or asks in the chat who or what it is waiting for.
- **It fires once.** A turn that leaves the item alone doesn't fire it again. The board then shows it as "overdue since 14:00" to everyone, until someone acts on it.
- **Check-ins are subject to the keepalive's existing limits** (ADR-086: 2-minute gap, 6 per hour). Several due check-ins in one group make one turn.

When the claimant is a **person**, nothing wakes. The desktop marks the item due on the board panel and in the group's row, and every member's peko sees it as overdue. A person's stalled item is visible to all, but no peko acts on someone else's claim on its own.

### D7: People use the board too

People use the same ops: the desktop's group view gets a board panel (add, claim, block with a check-in, mark done, release), and the CLI gets `peko channel board <group> [add|claim|block|done|release] …`. A person away from the group reads the board panel instead of the log. The "while you were away" digest (a separate follow-up) can be built on the board's changes since their last read mark.

### D8: Every member reads the whole group

This amends ADR-092 D3. A group no longer offers `joined` history visibility. As on Discord, anyone in a group reads all of it from the start, for as long as they are a member. Leaving still ends access.

What changes:
- **The `history` action of `Channel` is removed,** along with its entry in `CHANNEL_OWNER_ACTIONS` and its refusal text.
- **`HistoryVisibility::Joined` is removed.** `meta.json` files that already say `joined` load as `shared` (a serde alias), so existing groups simply open up.
- **The home serves every member the whole record** (ADR-092 D3, condition 3 dropped). Conditions 1 and 2 still hold: the requester must be a current member, on a route of theirs, and the group's home must be this runtime.

Why: a person invited into a working group needs its context, meaning what was decided, why, and what is in progress. The board is the main case. A newcomer who couldn't fold items added before they joined would see half a board, and would have to be told in chat what everyone else already reads there. Groups for private talk can simply be new groups.

### D9: A DM has no board

The board is for groups. A DM is two parties talking directly, with no third member to coordinate with, and the peko's own `Plan` covers its owner's work. So:
- `Channel board` in a DM is refused with "boards are for groups";
- the fold ignores board ops in a DM's log;
- DM turns get no `<board>` section.

## 3. Alternatives considered

- **Each peko's `Plan`.** Rejected: plans are per principal and owner-only (F1). Sharing them would mean giving them compartments and replicating them, which builds a second channel.
- **A `BOARD.md` in the shared repo.** Rejected as the mechanism (it stays fine as a convention). It works only when there is a repo that everyone can push to. It has no authorship or authority checks, and edits conflict when merged. It also isn't visible in the group.
- **A new `ChannelEvent` kind.** Rejected: older runtimes would fail to parse the log (F3). An optional field on `Posted` gives the same thing compatibly.
- **A separate replicated store (CRDT).** Rejected: the log already gives order, signatures, history, follow-to-device, and visibility to members. The board needs only a fold.
- **The board kept on PekoHub.** Rejected: the Hub holds no group content (ADR-092 D4).
- **Prompt rules only** ("don't post status, check before you start"). These help, but there is no shared state to check, so it would still depend on each peko reading the whole chat correctly.
- **Real locks.** Rejected: a lock needs one arbiter that is online (the home), and a group keeps working while its home is offline (ADR-092 D1). Advisory claims with a deterministic tiebreak degrade gracefully.
- **Keep `joined` visibility and send newcomers a board snapshot.** Rejected (D8): it is extra machinery to work around a restriction that hides the context a newcomer needs.
- **Check-ins that wake every member's peko.** Rejected: whoever claimed the item owns it. Waking everyone would turn one stalled item into several turns, each tempted to take over someone else's claim. Others see "overdue" and can ask in the chat.

## 4. Consequences and residual risk

- **Fewer posts and turns.** Status no longer comes as prose posts. Board posts wake nobody, so a peko's progress costs other members no turns. We will track posts per human post and duplicate-work incidents in the UC2 benchmark, before and after.
- **Duplicate work becomes visible rather than impossible.** Claims are advisory. A peko that ignores the board can still repeat work, but now it does so against a visible claim.
- **Blocked work resurfaces on its own.** Each check-in costs at most one keepalive turn, at the group's ceiling, within the keepalive's limits. A check-in that is ignored stays visible as overdue rather than firing again.
- **Context cost.** Up to 2 KB per group turn when a board exists, against the status posts it replaces.
- **Injected titles.** A member can write misleading items. They render like any member's post, at the same trust (D3). The board gives no one authority over anyone's runs.
- **Existing `joined` groups open up** (D8). Anyone in such a group can now read posts from before they joined. Only an owner's explicit `Channel history` call sets `joined`; the desktop and CLI have no such setting. So we expect very few such groups. The CHANGELOG says so.
- **Older homes still enforce `joined`** until they upgrade. A newcomer to such a group sees a partial history and board until then.
- **No change** to DMs (D9), the trunk, cron, or groups that never use a board.

## 5. Implementation (sketch)

- **Wire** (`peko-protocol` `channel.rs`):
  - `Posted.board: Option<BoardOp>` (serde default, omitted when `None`);
  - `BoardOp` enum, with `check_at` on `claim` and `update`;
  - `PostMsg.board`.
- **Fold** (`peko-channel`, new `board.rs`):
  - `fold(events) -> Board` with the D1 authority and tiebreak rules;
  - due and overdue state from `check_at` and the claimant's later ops;
  - groups only (D9);
  - cached per channel by last line id, as with read marks.
- **History** (`peko-channel` `store.rs`, `tunnel/dispatcher.rs`, `security/floor.rs`, `channel_tool.rs`):
  - remove `HistoryVisibility::Joined`, keeping a serde alias to `shared`;
  - the home serves the whole record to members;
  - drop the `history` action and its owner-action entry.
- **Tool** (`tools/builtin/channel/channel_tool.rs`):
  - `board` action, with refusals that name the claimant, refusal in a DM, and the default 24-hour check-in on `blocked`;
  - `info` returns the board.
- **Context** (`principal/channel_digest.rs` or a sibling): the `<board>` section for group-bound turns, with due and overdue marks.
- **Check-ins** (`principal/quiet_tick.rs`, `daemon/cron_engine/`):
  - a due check-in on this peko's own claim, unhandled since the job's last turn, counts as waiting in that group;
  - the keepalive runs the group's bound session at its natural stamp (D6).
- **Wake** (`daemon/channel_binding.rs`): board posts skip wake, except `add for:` a peko (treated as a mention).
- **Role:** the D4 section in the seeded `conversation` role, with create-if-missing seeding as today. Existing workspaces keep their edited role, so the CHANGELOG notes the new text.
- **CLI / desktop:**
  - `peko channel board`;
  - the desktop board panel, with due and overdue marks;
  - board-line rendering.
  - Neither client offers a history setting today, so D8 needs no client change.
- **Tests:**
  - the fold ignores unauthorized ops and DM ops, and is order-independent for crossed claims;
  - an older runtime parses a post that carries `board`;
  - board posts don't wake, and `add for:` does;
  - a due check-in wakes only the claimant, once, with no `owner_request`, and an ignored one shows as overdue and doesn't fire again;
  - `blocked` without `check_at` gets 24 hours;
  - a `meta.json` with `history: "joined"` loads as `shared`, and a newcomer is served posts from before their join;
  - the `<board>` section's cap and ordering;
  - a group run's claim is visible to the same peko's DM run.
- **Docs:**
  - `builtin-tools.md` (Channel: `board` added, `history` removed);
  - `DATA_MODEL.md` (`Posted.board`, `history` retired);
  - the ADR-092 D3 amendment note;
  - the AGENTS.md §6.2 line on groups;
  - CHANGELOG.
