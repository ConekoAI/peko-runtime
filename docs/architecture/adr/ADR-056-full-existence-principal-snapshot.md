# ADR-056: Full-Existence Principal Snapshot — Packaging Decoupled from Tiering

**Status:** Proposed (prototype implemented on branch `adr-056-full-existence-snapshot`)
**Date:** 2026-09-14
**Author:** rlsn (with WorkBuddy)
**Related:** [ADR-041](ADR-041-principal-as-container.md) §2.7 (packaging),
[ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md) §4/§5/§7
(snapshot = `tar workspace/`; `plugins/` layer),
[ADR-050](ADR-050-capabilities-as-workspace-files.md) (presence = visibility),
[ADR-052](ADR-052-tiered-system-prompt.md) (per-turn identity render),
[ADR-054](ADR-054-principal-genesis-pipeline.md) (genesis pipeline, D1/D3/D4,
boot-state entry rules), [ADR-055](ADR-055-principal-kb.md) (`kb/` tree),
[PRINCIPAL_WORKSPACE](../PRINCIPAL_WORKSPACE.md) (workspace layout),
`peko-rs/core/src/common/paths.rs` (three-tier storage contract).

---

## 1. Context

The `.principal` round-trip does not preserve a live principal.

What the packager exports today (`principal_packager.rs`): `config/`,
`identity/` (DID doc + keys), `agents/` (Shared tier), and `sessions/`
only behind the opt-in `--include-sessions` flag (default **false**).
What it silently discards:

- **`local/cron/`** — the trunk's schedule, including the jobs the
  trunk authored for itself after genesis.
- **`local/plans/`** — the Plan DAG state the trunk is executing.
- **`local/memory_index.json`** — session metadata index (explicitly
  "not part of the portable bundle" since Phase A).
- **Workspace tooling** — `tools/`, `skills/`, `mcp/`, `hooks/`, `kb/`
  are never collected. The `plugins/` layer is emitted but always
  empty, despite ADR-047 §4 declaring "Snapshot is `tar workspace/`"
  and PRINCIPAL_WORKSPACE.md claiming plugins travel in packages.
  (`SharedLayout` doesn't even carry a `kb_dir`.) ADR-050's
  *presence = visibility* rule means the imported principal's
  per-turn rendered catalog is silently gutted.

The consequences compound through the genesis pipeline (ADR-054):

1. An import enters at `defined` (D1), so the daemon re-seeds a
   **runtime-authored generic** genesis brief and a default 10-minute
   keepalive. A self-organized principal comes back as a stranger
   wearing its own clothes.
2. D4 promises an `organized` principal "the runtime will never
   re-seed or fix its cron schedule" — because *the trunk owns its
   rhythm*. Export then throws that rhythm away. The promise is
   existential ("a heartbeat exists"), yet the packaging layer
   destroys the heartbeat it promised to respect.
3. Worse, the pre-ADR-056 config import carried the source's
   `boot_state` verbatim: a definition-only export of an `organized`
   principal imported as `organized` **with no schedule at all** —
   a principal the runtime is now forbidden from helping, holding no
   heartbeat. That is a live bug, not a hypothetical.

### 1.1 The category error

The pinned contract in `common/paths.rs` reads: "Local tier contents
are runtime-only state — never packaged. Shared tier contents are …
packaged." This conflates two orthogonal axes:

- **Access semantics** — who writes and reads the data. Shared tier is
  *explicit* storage the principal touches directly (config, prompts,
  tooling); Local tier is *implicit* storage managed by the runtime
  and tool calls (the engine writes session JSONL, `CronCreate` writes
  `schedule.toml`, plan tools write the plan DAG). This distinction is
  load-bearing for the authority/actor-gate model and does not change.
- **Portability** — what constitutes the principal's existence in a
  snapshot. This is a packaging-policy question, and pinning it to the
  access axis is the mistake.

Sessions, cron, and plans are not "runtime state" in the ephemeral
sense. They are the principal's experiential record and
self-authored structure — the "working context" that, together with
long-term memory (`kb/`), forms its continuity. A snapshot that keeps
`kb/` but drops the last N sessions and the trunk's own cadence keeps
the principal's beliefs and loses its life.

### 1.2 The refinement: authored vs derived *within* Local

"Package everything local" is also too coarse. Local tier contains a
genuine ephemeral category:

- **Authored / identity-bearing (package it):** `sessions/`,
  `cron/` (schedule + run history), `plans/`.
- **Derived / rebuildable (exclude, rebuild on import):** `cache/`,
  `locks/`, `memory_index.json` (an index over sessions — re-derivable
  by rescan).

So the correct packaging cut is not the tier line but the
authored-vs-derived line, applied across tiers.

---

## 2. Decision

**The tier boundary is an access boundary, not a packaging boundary.**
Packaging policy becomes a separate axis with an explicit, declared
export mode, and a full snapshot packages every identity-bearing
category regardless of tier.

### D1: `ExportMode` on the manifest

`PrincipalManifest` gains `export_mode: ExportMode` with values
`definition` (default) and `full_snapshot`. The field is omitted from
the TOML when default, so pre-ADR-056 packages parse unchanged and
read as `definition`.

- **`definition`** — the historical surface: `config/`, `identity/`,
  `agents/` (plus `sessions/` if `include_sessions` is set). The right
  shape for sharing a principal as a template or pushing it to a
  registry: sessions and self-authored state do not travel, and the
  privacy posture (transcripts are sensitive) is carried by mode
  selection rather than silent inclusion.
- **`full_snapshot`** — definition layers **plus**:
  - `sessions/`, `cron/`, `plans/` (Local tier, authored);
  - `tools/`, `skills/`, `mcp/`, `hooks/`, `kb/` (workspace tooling,
    finally closing ADR-047 §4's "snapshot is `tar workspace/`" gap).

  `cache/`, `locks/`, and `memory_index.json` are never packaged in
  any mode.

`PrincipalLayers` gains `cron`, `plans`, `tools`, `hooks`, `kb`
digest fields (reusing the existing `skills`/`mcp` fields for
workspace tooling). `LayerType` gains `Cron`, `Plans`, `Tools`,
`Hooks`, `Kb` with matching OCI media types.

### D2: Import restores what the snapshot declares

The unpackager imports the layers the package actually carries:

- Workspace tooling → `<shared_root>/{tools,skills,mcp,hooks,kb}/`.
- `cron/` → `local/cron/`, `plans/` → `local/plans/`, gated by a new
  `import_local_state` option (default on, symmetric with
  `import_sessions`).
- `PrincipalImportResult` reports `import_mode` so the CLI/preview can
  state what was restored.

### D3: Cron rebinding

Cron jobs are keyed on the owning principal's runtime `PrincipalId`
(ADR-054 D3 — the convention the cron tools stamp and `CronList`
filters on). At import, every job's `principal_id` in the cron
database is rebound to the imported principal's effective id
(`config.id` → DID → name — the same resolution order the cron tools
and `genesis.rs` use). Per-principal schedule files contain only jobs
owned by that principal, so the rewrite is unconditional, correct, and
idempotent. A malformed file passes through unchanged with a warning;
the cron engine surfaces it on next load rather than the import
hard-failing on a side artifact.

Format note: `local/cron/schedule.toml` carries a legacy `.toml` name
but is serialized as **JSON** (`CronDatabase`, `serde_json`) — the
rebinding tries JSON first and keeps a TOML fallback in case the file
name ever becomes truthful. The live e2e verifies the JSON path
against a real daemon-written schedule.

### D4: Boot-state entry rules (extends ADR-054 D1)

- **`full_snapshot`** carries the source's `boot_state` verbatim. An
  `organized` principal imports as `organized` **with its authored
  schedule intact** — the daemon's boot seeding pass abstains (D4 of
  ADR-054: the trunk owns its rhythm, and the snapshot just delivered
  that rhythm across hosts). A `defined` snapshot re-genesis's on
  next boot, which is exactly what the source was.
- **`definition`** resets `boot_state` to `None` and lets ADR-054's
  inference apply (`has_definition()` ⇒ `defined`). This fixes the
  §1 live bug: a definition import can never again land `organized`.

### D5: The tier contract stays — only its comment changes

`paths.rs` keeps the three-tier layout and typed-path discipline
unchanged. The pinned comment is rewritten to state the real rule:
the tier boundary is an access boundary; packaging is a per-category
policy decided by export mode; derived Local state is never packaged.

---

## 3. What ships in this change (prototype)

- `registry/packaging/principal_manifest.rs` — `ExportMode`,
  `export_mode` field (+ legacy-parse tests), six new `PrincipalLayers`
  digest fields.
- `registry/packaging/types.rs` — `LayerType::{Cron, Plans, Tools,
  Hooks, Kb}` with OCI media types; Skills/Mcp doc comments updated
  (reused for principal workspace tooling).
- `registry/packaging/principal_packager.rs` — `mode` on
  `PrincipalExportOptions`; `with_workspace_dir` / `with_local_root`
  setters; `export_local_authored` + `export_workspace_tooling`
  collectors (full-snapshot only); layer computation extended.
- `registry/packaging/principal_unpackager.rs` —
  `import_local_state` option; `import_workspace_tooling` /
  `import_local_authored`; `remap_cron_principal_ids` (D3);
  boot-state entry rules (D4); `import_mode` on the result.
- `ipc` — `PrincipalExport.full_snapshot` wire field (serde default,
  back-compat with old CLIs); handler threads the workspace/local
  roots into the packager.
- `cli` — `peko principal export --full-snapshot`.
- `common/paths.rs` — tier-contract comment rewritten (D5).
- Tests: full-snapshot collection (incl. ephemeral-exclusion
  assertions), definition-mode exclusion, full-snapshot import
  round-trip asserting every layer lands in its tier directory and
  `organized` survives verbatim, definition-import boot-state reset,
  cron remap (JSON — the real on-disk format — plus TOML fallback,
  rewrite/idempotence/malformed pass-through), manifest mode
  round-trip.
- e2e: `scripts/e2e/flows/snapshot-roundtrip-llm.sh` (real LLM,
  MiniMax) — genesis turn via `create -f`, trunk-authored state
  (CronCreate attempt with a schema-safe clone fallback, skill, kb
  note, plan, hand-stamped `organized`), both export modes inspected
  as tars, remove → import → per-tier restore assertions with stale-id
  rebinding, organized reboot abstaining from re-seed, a real `peko
  send` round-trip on the imported principal, and the
  definition-import contrast. Verified green end-to-end.

**Round-trip property now pinned by tests:** create → self-organize
(authored cron job, plan, installed tool, kb notes) → export
`--full-snapshot` → remove → import ⇒ sessions present, schedule
restored with rebound ids, plans restored, tooling catalog intact,
`boot_state = organized`, no genesis re-seed. Under the pre-ADR-056
packager this fails at every step except config.

## 4. Consequences

### Positive

- **The round-trip preserves existence, not just definition.** Working
  context, self-authored cadence, plan state, and installed tooling
  survive export/import. ADR-054 D4's promise becomes true across
  host moves.
- **Templates stay templates.** Definition mode keeps the
  privacy-conservative default for registry push and sharing;
  consent is expressed by mode, not by silently bundling transcripts.
- **The tooling gap closes.** ADR-047 §4's "snapshot is `tar
  workspace/`" is finally literal; presence = visibility survives
  imports.
- **The `organized`-without-schedule bug is fixed** by D4's
  definition-mode reset.

### Negative / costs

- **Full snapshots are bigger and more sensitive.** Sessions and cron
  payloads (which can embed conversation content in tick messages)
  travel in one file. Mitigation: mode is explicit; registry push
  paths (`export_for_registry`) should refuse or warn on
  full-snapshot mode until per-layer encryption lands (below).
- **Hooks travel in packages now.** A malicious snapshot can install
  hooks that fire on every turn. Mitigations: ADR-046 audit canary
  covers `hooks/`/`tools/`/`mcp/` baseline drift on next boot;
  the import preview should surface the hook inventory (follow-up).
  The production pass should gate tooling import alongside
  `principal:write_agents` (prototype relies on the canary).
- **Rebinding is id-based, not semantic.** Jobs referencing model ids
  or channel ids that don't exist on the target host degrade at
  dispatch, not import. Model-id validation is deferred (below).

## 5. Deferred (tracked follow-ups, not promised)

- **Registry push policy for full snapshots:** `export_for_registry`
  currently accepts any mode; decide refuse-vs-warn and add per-layer
  encryption for the sessions layer before pekohub distribution.
- **Import preview surfaces restored layers:** extend
  `PrincipalImportPreview` with the layer inventory (sessions count,
  cron job names, hook ids) so `peko principal import` shows what a
  snapshot will bring back.
- **Model-id validation on import:** warn when the snapshot's
  `preferred_model_id` or job templates reference models absent from
  the target runtime.
- **`kb_dir` on `SharedLayout`:** the `kb/` tree (ADR-055) is scanned
  via `workspace_path` joins; add the typed field for layout
  completeness.
- **Genesis-job hygiene on import:** a snapshot taken mid-genesis
  carries the one-shot `genesis` job; verify idempotence against
  `seed_boot_defaults` for non-organized imports.
- **CronCreate-authored job in e2e:** the flow's trunk-authored cron
  job currently falls back to a schema-safe schedule clone when the
  model skips the tool call; keep an eye on MiniMax tool-call
  reliability before tightening the flow to the real path only.

## 6. References

- [ADR-054](ADR-054-principal-genesis-pipeline.md) — D1 boot states,
  D3 job keying, D4 the trunk owns its rhythm.
- [ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
  §4 — "Snapshot is `tar workspace/`"; §7 — packaging format.
- [ADR-050](ADR-050-capabilities-as-workspace-files.md) — presence =
  visibility (why tooling must travel).
- [ADR-046](ADR-046-trust-and-audit.md) — audit canary over
  `tools/`/`hooks/`/`mcp/` (the safety net for imported tooling).
- [ADR-055](ADR-055-principal-kb.md) — the `kb/` tree.
- `peko-rs/core/src/common/paths.rs` — the three-tier contract (D5).
- `peko-rs/core/src/registry/packaging/` — this ADR's implementation.

---

*Version 0.1.0 · Full-Existence Principal Snapshot · 2026-09-14*
