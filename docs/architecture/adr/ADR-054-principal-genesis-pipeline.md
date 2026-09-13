# ADR-054: Principal Genesis Pipeline — Phased Creation and Booting

**Status:** Proposed
**Date:** 2026-09-13
**Author:** rlsn (with WorkBuddy)
**Related:** [PEKO](../PEKO.md) (§K keepalive contract),
[AGENT_SESSION_PARADIGM](../AGENT_SESSION_PARADIGM.md) (§4 wake budget,
§6 supervision gaps), [ADR-041](ADR-041-principal-as-container.md)
(principal-as-container), [ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(workspace as trust boundary), [ADR-050](ADR-050-capabilities-as-workspace-files.md)
(presence = visibility), [ADR-052](ADR-052-tiered-system-prompt.md)
(per-turn identity render).

---

## 1. Context

Principal creation today is one undifferentiated procedure:

- `peko principal create <name> --model` (`cli/commands/principal.rs`)
  guards against overwrite, pins the model, writes a default agent
  prompt, and calls `PrincipalManager::create` — which does everything
  else in one pass: tier layout, DID generation, config persist, memory
  store, `/tmp` + `/trash` seeding, quota meter, plan port, router.
- `[identity]` / `[intent]` exist in `principal.toml` and already
  render per-turn into every agent's prompt (`identity_prompt.rs`,
  ADR-052) — but nothing ever asks the creator for them. The desktop
  onboarding walkthrough stops at "create principal"; persona, purpose,
  and values have no acquisition surface at all.
- The trunk's JSONL record comes into existence at its **first
  self-turn** — and nothing schedules one. No keepalive cron job is
  provisioned at creation or boot, so a freshly created principal is
  exactly the passive request handler PEKO §K warns about, and stays
  that way until someone (or the model, unprompted) creates a
  trunk-targeted job. The chicken-and-egg is structural: the trunk can
  only self-organize after it has been woken, and only cron wakes it.
- There is no boot-state marker: the runtime cannot distinguish
  "created but never defined" from "defined but never woken" from
  "steady state", so `create` and `load` cannot share one pipeline and
  the keepalive gap cannot even be detected.

Three proposals surfaced for cleaning this up, and they compose:
categorize the procedure into **abstract phases** with clean boundaries
of concern; make **identity/intent/persona definition** an explicit
phase with designed UX channels; and add a bounded **self-organization
iteration** that satisfies the minimal PEKO model requirements
(workspace structure, session tree, initial keepalive, memory
conventions) at boot.

## 2. Decision

The principal creation/booting procedure is a **five-phase genesis
pipeline**. Two principles govern it: *files are the only contract*
(every UX channel writes the same workspace files; the runtime never
needs to know which channel was used), and *models enter only at P2*
(P0 and P1 are deterministic, model-free, and fully scriptable).

| Phase | Name | Model? | What happens |
|---|---|---|---|
| P0 | Provision | no | Tier layout, `principal.toml` skeleton, DID → keychain, quota meter, plan store, seed `/tmp` + `/trash` (create-once, dangling trunk tolerated). |
| P1 | Definition | no | Creator-supplied `[identity]`, `[intent]`, root `AGENT.md`, capabilities land in the workspace via any channel: interactive CLI, desktop onboarding step, direct file edit, or `.principal` import. |
| P2 | Genesis | yes | The trunk's FIRST self-turn, driven by a runtime-authored brief. The daemon guarantees it happens by scheduling it. |
| P3 | Induction | yes, budgeted | Bounded post-genesis turns: memory conventions, standing structure, cadence tuning. |
| P4 | Steady state | yes | The existing keepalive/supervision rhythm; daemon boots enter here. |

### D1: Boot state is a persisted config field

`principal.toml` gains `boot_state: Option<BootState>` with values
`provisioned | defined | genesis_pending | organized`
(`principal/config.rs`). Legacy configs without the field infer their
effective state (`has_definition()` ⇒ `defined`, else `provisioned`)
until the runtime first stamps it — so no migration is needed and
existing files parse unchanged. `PrincipalManager::create` stamps the
initial state (a `.principal` package import with definition content
enters at `defined`; a bare CLI create enters at `provisioned`).

The state machine is monotone under runtime writes:
`provisioned → defined → genesis_pending → organized`. A creator may
always edit files to re-define a principal (presence = visibility,
ADR-050 — picked up on the next turn), but the runtime only advances
states; it never rewinds them.

### D2: P1 definition — one command, template file, or file edit

`peko principal create` is ONE command covering P0 + P1 + P2. The
phases are an internal state machine, not a wizard — the CLI must not
leak them as separate verbs (an earlier iteration of this ADR shipped
`peko principal define`; it was removed as an implementation leak).

- **Bare `peko principal create <name> --model <id>`** provisions with
  defaults. Empty `[identity]`/`[intent]` is the honest on-disk state;
  the genesis turn adopts a working self-description (the brief
  instructs it to) — the model drafts its own identity, the creator
  refines files later.
- **`peko principal create <name> -f <template.toml>`** seeds the full
  definition from a file that IS a `principal.toml` (so
  `peko principal export` → edit → `create -f` round-trips):
  `[identity]`, `[intent]`, `preferred_model_id`, `[capabilities]`,
  `[[permissions]]`, `exposure`, `quota`, plus one tolerated extra —
  an inline `persona` (Markdown body for the root agent prompt).
  Unspecified fields take runtime defaults; `id`/`did`/`boot_state`
  in the template are ignored (a fresh identity is always generated);
  the CLI `name` argument wins over a template `name`. No separate
  interactive wizard: the template file is the interactive surface.
- **Blocking to alive.** After provisioning, create ensures the daemon
  knows the principal (spawns the sidecar when none is running; sends
  the idempotent `PrincipalReload` IPC when one is), seeds the genesis
  + keepalive jobs immediately (not at next boot), and **blocks until
  the one-shot genesis run finalizes successfully** — polling the
  schedule for the job's `run_count >= 1 && last_status == "success"`
  or its post-run self-deletion. `--detach` opts out (scripts/CI);
  `--wait-timeout` (default 300s) bounds the wait; a timeout or a
  failed run exits non-zero with the principal left fully seeded (it
  will genesis at the next daemon boot). Direct file edits remain
  first-class for re-definition (ADR-050 — picked up next turn).

### D3: P2 genesis — the runtime guarantees the first self-turn

At daemon boot, `principal::genesis::seed_boot_defaults` runs for every
loaded principal whose boot state is not `organized`:

1. Ensure a **recurring trunk-targeted `Send` job exists** (any enabled
   recurring `Send` counts — the trunk may already have made its own;
   the default carries id `keepalive`, every 10 minutes — well above
   the 60s `TRUNK_MIN_INTERVAL_MS` floor). This closes PEKO §K's "no
   cron firing into the root at all" anti-pattern at the runtime level:
   a heartbeat is guaranteed to EXIST. Jobs are keyed on the
   principal's **runtime `PrincipalId`** — the convention the cron
   tools themselves use (`CronCreate` stamps `ctx.principal_id`,
   `CronList` filters on it) — so the trunk can SEE its seeded
   heartbeat; keying on the DID instead makes the jobs invisible to
   the cron tool surface (found and fixed in live e2e). The engine's
   `resolve_principal` accepts either on dispatch. The engine fires on
   the stored `next_run` verbatim with no recompute at add time, so
   the seeded jobs set it explicitly (`at` for the one-shot; one full
   cadence out for the keepalive).
2. Ensure the **one-shot genesis job** exists for principals that have
   not had their genesis turn seeded yet (`provisioned`/`defined`
   states): id `genesis`, `At now + 60s`, `delete_after_run`, message
   = the runtime-authored **genesis brief** — pointing the trunk at
   its own definition and workspace and instructing it to verify,
   survey, and organize (bounded: a setup turn, not a work sprint).
3. Stamp `boot_state = genesis_pending` and persist.

Seeding is idempotent, per-principal fail-soft (warn-and-continue, the
`default_nodes` posture), and stops touching a principal's cron
schedule entirely once it is `organized`.

### D4: P4 handoff — the trunk owns its rhythm

`organized` is a promise in both directions: the runtime will never
re-seed or "fix" the principal's cron schedule at boot, and the trunk
is expected to maintain its own heartbeat (it holds the cron tools —
self-regulating keepalive, PEKO §K). The runtime's guarantee is
existential (a heartbeat exists until the principal says otherwise),
not cadential.

### D5: Memory/dream is a convention, not machinery (yet)

P2/P3 plant memory conventions through the trunk's own organization —
the genesis brief asks it to decide what standing structure and memory
habits it needs. The runtime does NOT build consolidation/"dream"
machinery in this ADR: the recorded gaps (agent-facing memory tool,
offline/headless compaction, per-subtree budget attribution) are its
prerequisites and remain tracked in AGENT_SESSION_PARADIGM §6.

### D6: No new extension surface

The phases are runtime-internal. No principal-layer hooks, no router
strategy field, no `PrincipalGenesis*` hook family. If a real need
surfaces, it gets its own design pass (same rule as ADR-041 §3.2).

## 3. What ships in this change

- `principal/config.rs` — `BootState` enum, `boot_state` field,
  `boot_state()` / `set_boot_state()` / `has_definition()`, tests.
- `principal/genesis.rs` (new) — `genesis_brief`, `keepalive_tick_message`,
  `genesis_job`, `keepalive_job`, `has_recurring_trunk_send`,
  `SeedReport`, `seed_boot_defaults`, tests.
- `principal/manager.rs` — `create` stamps the initial boot state.
- `daemon/mod.rs` — boot seeding pass after cron-engine construction.
- `ipc` — `PrincipalReload` / `PrincipalLoaded` packets: hand a
  locally-created principal to a running daemon's manager (the cron
  engine enumerates loaded principals only; without this, jobs for a
  principal created mid-run would never fire until restart).
- `cli` — `create` is one blocking command (template via `-f`,
  `--detach` / `--wait-timeout` escape hatches, daemon ensure + reload,
  immediate seeding, genesis-completion wait); `peko principal define`
  was removed (unshipped experiment); the default config no longer
  fabricates an identity.
- `scripts/e2e/flows/genesis-pipeline-llm.sh` (new) — live-LLM flow
  (MiniMax) driving all phases end-to-end: P0/P1 stamps, boot seeding,
  the real genesis self-turn, an ingress round-trip, and restart
  idempotence.

## 4. Consequences

### Positive

- Every principal — fresh or legacy — is guaranteed a heartbeat and a
  genesis turn; the passive-request-handler failure mode is closed.
- `create` and `load` share one state machine; the daemon can tell
  which phase a principal is in and act (or abstain) accordingly.
- Identity/intent acquisition has a CLI surface, and all channels
  converge on the same files.
- P0/P1 remain model-free and scriptable; `.principal` import lands in
  the same pipeline with its definition intact.

### Negative / costs

- A legacy principal's next boot schedules a genesis-style first turn
  (state inference + seeding). For a long-lived principal this is a
  supervision turn it would not otherwise have had — judged acceptable
  (it is the fix, not a regression), but operators will see it in the
  cron surface.
- The recurring default job carries the tick message on every fire
  until the trunk retunes it — tokens burn on the default cadence
  (10 min) until P3 budgeting wires `budget_per_cycle` (below).
- Two more config fields on `PrincipalConfig` (boot_state), and one
  more CLI subcommand.

## 5. Deferred (tracked follow-ups, not promised)

- **`organized` flip at the engine**: after the first successful
  trunk-targeted turn, the engine stamps `organized` (needs cron
  engine → manager config-write plumbing). Until then the boot pass
  re-checks idempotently every boot.
- **P3 induction budgeting**: wire `budget_per_cycle` /
  `cost_per_call_max` as the genesis/induction turn ceiling
  (AGENT_SESSION_PARADIGM §4's standing recommendation).
- **Desktop onboarding create step**: same single-command create over
  IPC (needs the template payload on the wire, or a desktop-written
  template file).
- **`boot_state` in `principal show` / list summaries.**
- **Memory/dream machinery** (see D5).

## 6. References

- [PEKO](../PEKO.md) — §K keepalive, §K anti-patterns.
- [AGENT_SESSION_PARADIGM](../AGENT_SESSION_PARADIGM.md) — §4 cron
  heartbeat + wake budget, §6 gap table.
- `peko-rs/core/src/principal/genesis.rs` — this ADR's P2 primitives.
- `peko-rs/core/src/principal/default_nodes.rs` — the create-once
  seeding precedent this module follows.

---

*Version 0.1.0 · Principal Genesis Pipeline · 2026-09-13*
