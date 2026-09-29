# roles/

One file per NAMED role that deserves durable memory of its own:
`<role-name>.md`. Standing context for that role — what it has
learned across runs, commitments it holds, current working state.
Revise in place.

The name pairs with the role file `roles/<name>/ROLE.md` (ADR-052
D3): the name selects a role, and both its body and its durable note
key on that role. The two files are deliberately DIFFERENT things:

- `roles/<name>/ROLE.md` is the role's **definition** — who it is,
  its remit and process. Frozen into the system prompt; changes are
  design decisions, made rarely, by the principal or its creator.
- `kb/roles/<name>.md` is the role's **state** — accumulated across
  runs. It rides the tail and re-injects whenever it changes, so the
  agent can update it freely without touching its own identity.

Never accumulate working notes in the role body: editing your own
definition is self-modification; editing your notes is just memory.

Cold for everyone else, hot for its owner (ADR-055 D8): when a run
starts for role `<name>` and this directory holds `<name>.md`, that
file is injected into that agent's prompt. Ephemeral, unnamed spawns
get no note — their learnings flow back into the principal's kb
through the spawn result. A role that needs no durable state simply
has no file here — absence renders absent.
