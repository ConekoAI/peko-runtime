# ADR-087: Conversations run a conversation role, chosen per binding; the root role supervises

**Status:** Accepted (runtime: `feature/conversation-role`)
**Date:** 2026-10-28
**Counterpart:** none. Roles are internal to the principal; no client surface (D6).

**Amends:**
- [ADR-052](ADR-052-tiered-system-prompt.md) D3 — "persona inheritance" (peer children run the root role) is no longer the default. The per-binding role it anticipated ("a channel binding may name a T1 role file") becomes the mechanism.
- [ADR-082](ADR-082-explicit-channel-bindings-and-principal-group-management.md) D2/D4/D5 — a binding gains `role`; `Channel info` shows it; `Channel` gains a `role` action.

**Preserves:** [ADR-064](ADR-064-agents-to-roles-terminology.md) (roles are Markdown templates; the root role seed is create-if-missing, never overwritten), ADR-052 D1 tiers (T0 principal / T1 role / T2 agent), [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (the trust floor follows the run's stamp, not its role).

---

## 1. Context

A user DMing a peko asked it what it was, and it answered that it runs as the **root** role. That is the current design, not a bug. Verified on `master` @ `38e79b38`:

| # | Fact | Where |
|---|---|---|
| F1 | Every conversation turn (DM and group) runs the principal's root role body. `routing.peer_agent` can swap the body principal-wide; nothing selects a role per channel. | `principal/child_turns.rs` `try_peer_child_agent_config` |
| F2 | The seeded root role mixes three jobs: trunk supervision ("a keepalive tick … does not call for a greeting"), conversation ("In a peer or group conversation, understand the request …"), and the tool manual. | `resources/roles/root/ROLE.md` |
| F3 | A conversation turn's `AgentConfig.name` is forced to `root`, so `{{role_name}}` reads `root` and the role note injected is `kb/roles/root.md`. | `child_turns.rs` `turn_executor` |
| F4 | Session stamps and run admission do not depend on that name: sessions take the session manager's `agent_name`, and the registry key is passed to `SubagentExecutor::new` separately. | `child_turns.rs` `build`, `peer_child_session_manager` |
| F5 | The principal's voice and shared rules already have a T0 home that reaches every agent: `[identity]` / `[intent]` in `principal.toml` and `kb/CONVENTIONS.md`. Only the root role duplicates any of it. | ADR-052 D4, `genesis.rs` `genesis_brief` |
| F6 | A binding (`meta.json` `bindings[principal] = { session, wake }`) is already the per-principal, per-channel record the runtime reads on every group post. | ADR-082 D2, `channel/src/port.rs` `Binding` |

So the one file that defines how the trunk supervises is also what every conversation runs. Tuning one tunes the other, and the agent misdescribes itself to the people it talks to.

## 2. Decision

### D1 — Roles describe jobs; the principal is T0

A **role** (T1) says what an agent's job is: supervising, conversing, coding, researching. Who the peko *is* — its name, voice, values, style, and the rules every agent keeps — is the **principal** (T0): `[identity]` / `[intent]` in `principal.toml` and `kb/CONVENTIONS.md`, rendered into every agent's context whatever its role. A role never restates the persona, and a persona edit never requires editing a role.

Roles are internal. A peko speaks as itself (the principal), not as its role; the seeded roles say so.

### D2 — The root role supervises

The root role is the trunk's role: keepalive and cron ticks, standing commitments, plans and their check-ins, delegated work, knowledge upkeep. The seeded `roles/root.md` keeps the supervision guidance and the tool manual and drops the conversation guidance.

The seed stays create-if-missing (ADR-064): existing principals keep the root role they have, including its conversation paragraph, which simply stops being read in conversations.

### D3 — Conversations run the `conversation` role by default

A new seeded role, `roles/conversation.md`, is optimized for talking with people and peers in a channel: reply directly, keep the conversation's context, ask when a request is ambiguous, delegate long work and say so, use `ChannelSend` only to reach someone else. It is seeded create-if-missing at provision and on every boot, so existing principals gain it on their next daemon start without any edit to their files.

### D4 — A binding may name its role

`meta.json` bindings gain an optional `role`:

```json
"bindings": {
  "prin_ab12": { "session": "<session id>", "wake": "mention", "role": "support" }
}
```

Absent means the default. Each conversation turn resolves its role afresh, first match wins:

1. the binding's `role`;
2. `routing.peer_agent` in `principal.toml` (kept as the principal-wide default for conversations);
3. `conversation`;
4. `root` (only when `roles/conversation.md` is missing, e.g. the owner deleted it).

A named role that does not resolve (no such file, unreadable) logs a warning and falls to the next step, so a bad value never breaks ingress. Because the role is re-read per turn, editing a role file or a binding takes effect on the next turn without a restart (as the root role already did).

Both kinds of channel can carry a role. A DM's `wake` is fixed (ADR-082 D3); its `role` is not.

### D5 — The prompt reads the role, not the agent's name

A run is addressed by its session's slug path; it needs no name of its own. `AgentConfig.name` is an identity key: the agent DID minted from it (a `by-name/<name>` alias in the runtime's identity store) also files the run's tool session state. Renaming runs after their roles would mint a DID per role name, shared by every principal on the runtime. So the name stays as it is, and `AgentConfig` gains a runtime-set `role` that the prompt reads instead: `{{role_name}}` and the injected role note (`kb/roles/<role>.md`) follow `role` when set, else the name.

- a conversation turn's `role` is its resolved role (D4);
- an `Agent`-tool spawn with a `role` sets that role. Before, it ran the role's body but rendered as `root` and got root's role note; a spawn without a role keeps its parent's prompt and role.

Session stamps, run admission, and agent identity keep the principal's root key (F4).

### D6 — The principal sets its roles (`Channel role`)

`Channel` gains a sixth action:

| Action | Parameters | Effect |
|---|---|---|
| `role` | `channel`, optional `role` | Sets the principal's own binding role (a role id from `RoleCatalog`); omitting `role` clears it back to the default. Refuses a role that does not resolve. Provisions the group session first if needed, as `wake` does. |

`Channel info`'s `you` gains `role` (the stored value, or `null`).

Who may set it — the role decides what the principal does in that conversation, so it is the owner's decision, like `create` / `invite` (ADR-082 D6):

| Owner run | Trusted run | Untrusted run |
|---|---|---|
| any channel it belongs to | waits for the owner's approval | refused |

A role changes instructions, not authority: the trust floor, the policy reviewer, and the ledger all key on the run's `ActingFor` stamp, which no role can raise.

There is no IPC or client surface for roles. A role is the principal's own instruction to itself, a workspace file like the rest of its capabilities (ADR-050), and the principal manages it. An owner who wants a different role in a channel says so in conversation; that is an owner run, so the principal sets it directly. IPC `channel_info` keeps reporting only `wake`.

### D7 — Genesis puts the persona in T0

The genesis brief tells a new principal that its voice, style, and shared rules belong in `[identity]` / `[intent]` and `kb/CONVENTIONS.md`, and that `roles/` holds jobs (`root` for supervision, `conversation` for channels, more it or its owner adds for specific work).

## 3. Alternatives considered

- **Keep persona inheritance; split the root role with headings.** One file would still serve two audiences, and the trunk's "finish quietly" guidance would keep leaking into conversations.
- **Role per session instead of per binding.** Sessions outlive bindings and move between parents; a binding is already the per-channel record the runtime consults (F6), and the owner thinks in channels, not session paths.
- **Retire `routing.peer_agent`.** It would break principals that set it. It now means "the default conversation role", which is what it was used for.
- **Let any run in a group choose its role, like `wake`.** A role rewrites the instructions the principal follows there; group members should not be able to pick that, any more than they can invite people.
- **Name each run after its role.** It fixes the role note too, but `name` keys the agent DID and its tool session slot: every role name would mint an identity shared across principals. A run needs no name; its session path is its address.
- **A client role picker (IPC `channel_set_role`).** It turns an internal instruction into a user setting the principal does not own, and duplicates what the owner can already ask for in conversation.

## 4. Consequences

- The trunk's role can be tuned for supervision without changing how the peko talks, and vice versa.
- A peko no longer tells people it runs as `root`. It can be a support desk in one group, a coding partner in another, and a companion in its owner's DM, all with the same identity, memory, and rules.
- Existing principals: `conversation.md` appears on the next boot and their conversations switch to it; their `root.md` is untouched. An owner who customized `root.md` to change how the peko *talks* should move that into `kb/CONVENTIONS.md` (or into `roles/conversation.md`).
- `kb/roles/root.md` notes no longer reach conversations; a note for conversations belongs in `kb/roles/conversation.md`.
- `meta.json` written by this runtime may carry `bindings.*.role`. An older runtime reads such a file (its `Binding` ignores unknown fields) and drops the role on its next save. Bindings never leave the runtime, so nothing changes on the wire.

## 5. Implementation

1. **Data model.** `Binding.role: Option<String>` (serde default, skipped when `None`); `ChannelPort::set_role` (default refuses) implemented by `ChannelStore` and delegated by the tunnel port.
2. **Seeds.** `resources/roles/conversation/ROLE.md`; `seed_default_conversation_role` beside `seed_default_root_role` at every call site; the root seed trimmed to supervision; the genesis brief's T0 guidance.
3. **Resolution.** `child_turns.rs` resolves the role per turn from the session's conversation channel binding (D4); `AgentConfig.role` carries it, and `subagent_executor.rs` sets it for a role spawn. The engine's `AgentView::role` feeds the prompt's role name (D5).
4. **Surface.** `Channel role` and `info.you.role` with the D6 trust table.
5. **Docs.** `builtin-tools.md` (`Channel`), `DATA_MODEL.md` (`meta.json` binding), `API_SURFACE.md` (`ChannelPort`), AGENTS.md §6.2 and §9, CHANGELOG.
