# ADR-064: `agents/` → `roles/` — finishing the role/agent terminology split

**Status:** Proposed
**Date:** 2026-09-29
**Related:** [ADR-052](ADR-052-tiered-system-prompt.md) (T0/T1/T2 tiers — T1 is
per-role), [ADR-055 rev 2](ADR-055-principal-kb.md) (kb/agents → kb/roles
rename, the precedent), [ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(workspace layout), [ADR-056](ADR-056-full-existence-principal-snapshot.md)
(packaging layers).

---

## 1. Context

"Agent" conflates two meanings in the framework:

1. A **live agent** — the actor driving a session (what the `Agent` tool
   spawns, what `AgentConfig` configures, what a subagent is).
2. A **role template** — the Markdown definition used to *initiate* an
   agent: today `<workspace>/agents/<name>/AGENT.md` or the flat
   `<workspace>/agents/<name>.md`.

ADR-052 fixed the prompt tiering around the second meaning: the T1
system-prompt tier is **per-role**, not per-agent (T2 is per-agent — the live actor of a session).
The code itself carries the conflation — `TurnPromptContext.agent_name`
holds the role name; the `Agent` tool's `agent` parameter selects a role;
`agent_catalog` lists role templates.

ADR-055 rev 2 already renamed the durable-note side to `kb/roles/<name>.md`
(paired with the role file). The definition side still says `agents/`.
This ADR finishes the split so that:

- **agent** = a live actor of a session (runtime concept),
- **role** = the template a session is initiated from (workspace file, T1),
- `roles/<name>.md` (definition) pairs with `kb/roles/<name>.md` (state).

## 2. Decision

### D1 — Workspace directory and layouts

- `<workspace>/agents/` → `<workspace>/roles/`.
- Two accepted layouts, mirroring today's:
  - `roles/<name>.md` (flat — **preferred**, symmetric with `kb/roles/`),
  - `roles/<name>/ROLE.md` (directory form, for sidecar assets).
- Root resolution order becomes: `[routing].root_prompt` →
  `roles/root.md` | `roles/root/ROLE.md` → compiled-in default
  (`resources/roles/root/ROLE.md`).
  *(Superseded 2026-09-29: the compiled-in **render-time fallback** is
  removed — the resource file is now a SEED source stamped at provision
  and backfilled by the genesis boot pass; a missing root role file
  fails loudly. Authored prompts are a default start, never a hidden
  default value.)*
- A legacy workspace with `agents/` is migrated at boot (D5).

### D2 — Tool surface

- `agent_catalog` tool → **`role_catalog`** (same output shape: id,
  name, enabled flag).
- `Agent` tool: parameter `agent` → **`role`** (the role id selecting the
  template). The tool **name stays `Agent`** — it runs live agents; the
  word "agent" belongs to the actor side.
- `TurnPromptContext.agent_name` → `role_name` (internal, but the field
  feeds `{{agent_name}}` → **`{{role_name}}`** and the role-note loader).

### D3 — Capability namespace

`extension-api` capability `agent:*` (workspace role-template grants) →
`role:*`. Legacy `agent:*` grants accepted and mapped on read.

### D4 — Packaging

- Manifest layer key `agents` → `roles`; packager emits `roles/`.
- Unpackager **accepts the legacy `agents` layer** and restores it into
  `roles/` (same back-compat stance as legacy `extensions/*.ext` layers).
- `with_agents_dir` builders → `with_roles_dir`.

### D5 — Boot migration

`seed_boot_defaults` gains a third idempotent move (next to the D4 memory
and roles-dir-kb migrations): workspace `agents/` → `roles/` when `agents/`
exists and `roles/` does not. Existing `roles/` wins; the legacy directory
is left untouched (no silent destructive merges).

### D6 — Adapter

`extensions/agent/` (the `AGENT.md` adapter: role discovery + catalog
section + capability validation) is **renamed** `extensions/role/`, with
`AgentAdapter::discover_agents` → `RoleAdapter::discover_roles` and the
tail section rendered as the role catalog. *Correction to the record: no
prior decision retires this adapter — it is the scan, and the scan is
load-bearing.*

## 3. Consequences

**Positive:** one word per concept; the T1 tier, its files, its notes, and
its catalog all say "role"; `roles/<name>.md` ↔ `kb/roles/<name>.md` is a
visible, teachable pairing; the `{{agent_name}}` placeholder stops lying.

**Negative / costs:** wire-visible changes (tool param `role`, layer key
`roles`, capability `role:*`) — acceptable at v0.1.0, with legacy-read
compatibility on the packaging side; sweep of docs (ADR-047, -050, -052,
PRINCIPAL_WORKSPACE.md, PEKO.md) and tests (`cli_subagent.rs`,
`tiered_prompt.rs`, packaging tests, harness helpers).

**Migration:** boot-pass move for workspaces; import alias for packages.
No runtime re-reads of `agents/` after the slice.

## 4. Non-goals

- Renaming the `Agent` tool, `AgentConfig`, or the session-tree concept —
  "agent" stays the word for live actors.
- Renaming the industry-facing `AGENTS.md` project-instructions convention
  (ADR-052 D5) — that file belongs to repos, not to the principal
  workspace.

---

*Version 0.1.0 · agents/ → roles/ · 2026-09-29*
