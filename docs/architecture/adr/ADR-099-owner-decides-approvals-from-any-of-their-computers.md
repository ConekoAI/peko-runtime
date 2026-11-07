# ADR-099: The owner decides their peko's approval requests from any of their computers

**Status:** Accepted (2026-11-07)
**Date:** 2026-11-07
**Builds on:** [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) (D7: a non-owner run's unclear call waits for the owner's own action), [ADR-097](ADR-097-owner-picks-a-remote-pekos-model-per-message.md) (D1: a signed request from the owner's other computer to the peko's host, answered only for the owner), [ADR-095](ADR-095-symmetric-dms-that-follow-devices-and-moves.md) (the owner's DMs with their pekos follow them to every runtime of their account), [ADR-081](ADR-081-actor-identity-ownership-and-single-online-copy.md) (the local user and their Hub account are one actor).
**Counterpart:** PekoHub (relay of four new tunnel messages). Peko desktop (Approve / Deny for a peko on another computer).

---

## 1. Context

A 2026-11-07 field test: an owner on their second computer saw their peko's approval notice in its DM and could do nothing with it. Verified on `master` @ `98d04d28`:

| # | Fact | Where |
|---|---|---|
| F1 | A non-owner run's sensitive call the policy reviewer finds unclear is stored as a pending request under `<peko local>/approvals/<id>.json` and announced in the owner's conversation. | `principal/policy_gate.rs` `ask_owner`, `notify_owner` |
| F2 | The owner decides only with their own action: IPC `approval_decide` (the desktop's buttons, `peko approve` / `peko deny`), which checks `ManageSettings` on the peko — the owner or the operator — and runs `decide_request`. Never a message: no model reading the conversation can approve. | `ipc/handlers/principal.rs` `owner_principal`; `policy_gate::decide_request` |
| F3 | `approval_list` / `approval_decide` reach only the daemon they are sent to, so they work only on the peko's own computer. | `ipc/handlers/principal.rs` |
| F4 | The owner's other computer holds the DM with the peko (ADR-095) and can already ask the host something on the owner's behalf with a runtime-signed tunnel request, which the host answers only when the asker is the peko's owner (ADR-097 D1). | `tunnel/peko_models.rs`; `daemon/state.rs` `owner_models` |
| F5 | PekoHub relays a person's request only from a runtime registered to that person's account, to another runtime of the same account. A post relayed that way already reaches the host as the owner (`Owner` stamp). | PekoHub `speaksForItsOwnPerson`; `security/stamps.rs` |

## 2. Decision

### D1 — The owner's other computer lists the waiting requests

Two runtime-signed tunnel messages (`sign_history`), between two runtimes of one account, shaped like ADR-097 D1:

- `tunnel_peko_approvals_request { requestId, sourceRuntimeId, recipientRuntimeId, sourcePrincipalDid, principalDid, signature }` — the person (`sourcePrincipalDid = user:hub:<account>`) asks the runtime hosting the peko whose signing DID is `principalDid` for its pending requests. Payload `peko-peko-approvals-request/1`.
- `tunnel_peko_approvals { requestId, sourceRuntimeId, recipientRuntimeId, principalDid, approvals, signature }` — the host's answer, oldest first: `approvals` is `[{ id, actor, actorName?, trust, summary, reason, createdAt, expiresAt }]`, exactly what `approval_list` shows locally. Payload `peko-peko-approvals/1`; the list is signed as `approvals_b64u`.

The host answers with the list only when `principalDid` is a peko hosted there **whose owner is `sourcePrincipalDid`** (the same actor; ADR-081); anyone else, and any peko not hosted there, gets an empty list.

The desktop does not get the list pushed: it asks when the DM changes (the peko's notice lands there) and every 30 s, as for a local peko.

### D2 — The owner's decision travels to the host, which applies it as `approval_decide`

- `tunnel_peko_approval_decide { requestId, sourceRuntimeId, recipientRuntimeId, sourcePrincipalDid, principalDid, approvalId, approve, always, signature }` — the person's decision on request `approvalId`: approve (with `always`, also add the always-allow rule to the host's `POLICY.md`) or deny. Payload `peko-peko-approval-decide/1`.
- `tunnel_peko_approval_decided { requestId, sourceRuntimeId, recipientRuntimeId, principalDid, approvalId, decided, message, signature }` — what the host did: `decided` when it applied the decision, and `message` saying what happened ("Approved apr_… Running it for … now") or why not ("Request apr_… was already decided."). Payload `peko-peko-approval-decided/1`.

**The owner check, on the host.** A decision is applied only when all of these hold:

1. it is addressed to this runtime (`recipientRuntimeId`), and `sourcePrincipalDid` is a person (`user:hub:…`);
2. its signature verifies against the `did:key` of `sourceRuntimeId`, every field agrees with the signed payload, the version tag matches, and it is fresh (`iat`/`exp`, five minutes);
3. `principalDid` is a peko hosted here, and `sourcePrincipalDid` is the same actor as its owner.

Then the host runs `decide_request` — the same code as `approval_decide` (F2) — so the decision is applied once (a decided or expired request refuses), an approval replays the call for its requester, and the audit record `policy.owner_decision` names the runtime it came from (`via_runtime`). Anything else is refused with "Only the peko's owner decides its approval requests." or not answered at all (a request that fails 1–2 is dropped).

That `sourceRuntimeId` speaks for `sourcePrincipalDid` is PekoHub's check (F5), as for every person-scoped request and for the owner's posts, which already carry the owner's full authority to the peko. The host does not add a second binding of its own: a runtime able to pass off as the owner's here could already instruct the peko as its owner. A replayed decision (within its five minutes) changes nothing: the request is already decided.

The decision is still the owner's own action, never a message (ADR-078 D7): it is sent only by the desktop's buttons on the owner's other computer, through IPC that only the local user may call.

### D3 — IPC on the owner's other computer

- `channel_peko_approvals { channel }` → `channel_peko_approvals_result { approvals }` (`approval_list`'s `ApprovalInfo` shape).
- `channel_peko_approval_decide { channel, id, approve, always }` → `approval_decided { id, approved, message }`, or an error with the host's reason.

Local callers only, and `channel` must be this account's DM with one of its pekos on another runtime (as `channel_peko_models`). An unanswered request fails after 15 s ("The computer hosting this peko did not answer; it may be offline").

### D4 — The Hub only relays

The Hub relays `tunnel_peko_approvals_request` and `tunnel_peko_approval_decide` only when the source runtime speaks for `sourcePrincipalDid` and the recipient runtime is registered to the same account, and `tunnel_peko_approvals` / `tunnel_peko_approval_decided` only between two runtimes registered to the same account. Requests share the history request rate limit. It stores nothing.

### D5 — Desktop

For a peko elsewhere that follows the account (`fromAccount`), the chat shows its waiting requests with Approve / Always allow / Deny, like a local peko's, and the buttons go through `channel_peko_approval_decide`. Other people's pekos show none: their host would answer with an empty list.

## 3. Alternatives considered

- **Sync pending requests into the DM copy (or the account's state) on every runtime.** Each copy would need to learn of decisions made elsewhere and of expiries, and a stale copy would offer a request already decided. Asking the host keeps one source of truth, at the cost of needing the host online — which deciding needs anyway, since the host runs the approved call.
- **Proxy `approval_decide` itself over the tunnel.** IPC callers and permissions (`ManageSettings`, operator) are local notions; a tunnel request has a person, not an IPC caller. A dedicated message keeps the host's check about one thing: is this the peko's owner.
- **Decide by replying in the DM** ("approve apr_…"). Rejected by ADR-078 D7: a message is what a model reads and could write.
- **The host binds `sourceRuntimeId` to the account itself** (from its DM routes). Rejected for now: it would differ from every other person-scoped request and from the owner's posts (D2); a separate ADR can tighten all of them together.

## 4. Consequences

- The owner can act on an approval from whichever of their computers they are on; the peko's own computer must be online, as it runs the approved call.
- A request decided on one computer disappears from the others at their next refresh (on the DM's next row, or within 30 s); deciding it again there says it was already decided.
- `approval_list` / `approval_decide` are unchanged; the CLI still decides only on the peko's computer.

## 5. Implementation

- **Tunnel** — `TunnelPekoApprovalsRequest` / `TunnelPekoApprovals` / `TunnelPekoApprovalDecide` / `TunnelPekoApprovalDecided` and `PekoApproval` (`protocol.rs`), with contract samples; their payloads and verifiers (`tunnel_channel_signature.rs`); `tunnel/remote_approvals.rs`: the asker's `peko_approvals` / `decide_peko_approval` with a pending table, the host's verification and signed answers; the dispatcher routes them and asks the host through `TunnelHost::peko_approvals_for_owner` / `decide_peko_approval_for_owner`. The "this account's DM with its peko elsewhere" lookup is shared with ADR-097 (`own_dm_peko`).
- **Daemon** — `AppState::owned_principal` (the peko, when the person is its owner) backs both new host methods and `owner_models`; decisions go through `policy_gate::decide_request_via`, which records `via_runtime`.
- **IPC** — `channel_peko_approvals`, `channel_peko_approval_decide`.
- **PekoHub** — the four relays.
- **Desktop** — `HubRemoteClient::approvals` / `decide_approval`; `principal_approvals` and `principal_decide_approval` take the peko's `runtime_id`; the chat enables the cards for the owner's own pekos elsewhere.
