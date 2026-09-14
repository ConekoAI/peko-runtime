# ADR-056: Full-Existence Principal Snapshot — Two Groundings, One DID

**Status:** Proposed (prototype implemented on branch
`adr-056-full-existence-snapshot`; rev. 2 — definition mode removed,
registry recast as template distribution)
**Date:** 2026-09-14
**Author:** rlsn (with WorkBuddy)
**Related:** [ADR-041](ADR-041-principal-as-container.md) §2.7 (packaging),
[ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md) §4/§5/§7
(snapshot = `tar workspace/`; `plugins/` layer),
[ADR-050](ADR-050-capabilities-as-workspace-files.md) (presence = visibility),
[ADR-052](ADR-052-tiered-system-prompt.md) (per-turn identity render),
[ADR-054](ADR-054-principal-genesis-pipeline.md) (genesis pipeline, D1/D3/D4,
boot-state entry rules), [ADR-055](ADR-055-principal-kb.md) (`kb/` tree),
[ADR-032](ADR-032-runtime-identity-and-multi-host-awareness.md) (runtime
identity), [ADR-046](ADR-046-trust-and-audit.md) (trust + audit),
[PRINCIPAL_WORKSPACE](../PRINCIPAL_WORKSPACE.md) (workspace layout),
`peko-rs/core/src/common/paths.rs` (three-tier storage contract).

---

## 1. Context

The `.principal` round-trip did not preserve a live principal.

What the pre-ADR-056 packager exported: `config/`, `identity/` (DID doc
+ keys), `agents/` (Shared tier), and `sessions/` only behind the
opt-in `--include-sessions` flag (default **false**). What it silently
discarded:

- **`local/cron/`** — the trunk's schedule, including the jobs the
  trunk authored for itself after genesis.
- **`local/plans/`** — the Plan DAG state the trunk is executing.
- **`local/memory_index.json`** — session metadata index.
- **Workspace tooling** — `tools/`, `skills/`, `mcp/`, `hooks/`, `kb/`
  were never collected. The `plugins/` layer was emitted but always
  empty, despite ADR-047 §4 declaring "Snapshot is `tar workspace/`".
  ADR-050's *presence = visibility* rule meant the imported principal's
  per-turn rendered catalog was silently gutted.

The consequences compounded through the genesis pipeline (ADR-054):
imports entered at `defined` and got a **runtime-authored generic**
genesis brief and a default keepalive — a self-organized principal came
back as a stranger wearing its own clothes. D4 promised an `organized`
principal "the runtime will never re-seed its cron schedule" — then
export threw that rhythm away. And the config import carried the
source's `boot_state` verbatim, so a definition-only export of an
`organized` principal imported as `organized` **with no schedule at
all** — a principal the runtime is now forbidden from helping, holding
no heartbeat. A live bug, not a hypothetical.

### 1.1 The category error

The pinned contract in `common/paths.rs` — "Local tier contents are
runtime-only state — never packaged" — conflated two orthogonal axes:

- **Access semantics** — who writes and reads the data. Shared tier is
  *explicit* storage the principal touches directly; Local tier is
  *implicit* storage managed by the runtime and tool calls. This
  distinction is load-bearing for the authority/actor-gate model and
  does not change.
- **Portability** — what constitutes the principal's existence in a
  snapshot. Sessions, cron, and plans are not "runtime state" in the
  ephemeral sense; they are the experiential record and self-authored
  structure — the working context that, together with `kb/`, forms the
  principal's continuity.

### 1.2 The second error: definition mode forked DIDs

The original draft of this ADR kept a `definition` export mode alongside
the snapshot. On reflection that mode is removed, because it violated
the identity model:

- **A DID is a singular, persistent identity.** Importing a
  definition package carried the source DID into a second, independent
  runtime with divergent state from turn zero — two actors wearing one
  ID card. The audit log is per-runtime (ADR-046): a forked DID yields
  two uncorrelatable audit streams under one identity. Trust pinning
  (TOFU), ownership (ADR-033/034), and runtime identity (ADR-032) all
  lean on DID singularity; definition mode quietly broke it.
- **Cloning was already solved correctly elsewhere.** ADR-054 D2's
  `create -f <template.toml>` is the clone path: it ignores
  `id`/`did`/`boot_state` and always mints a fresh identity, then runs
  genesis. A template carries the DNA; the creature that grows from it
  is new.
- **Transport is the only legitimate same-DID operation.** Moving a
  principal between runtimes (export → remove → import) keeps the
  creature *the same creature* — same keys, same memories, same
  rhythm. Copies may exist only as cold backups, never as
  simultaneously-live principals; the network-level commitment is that
  **only one principal with a given DID may be publicly exposed on
  Pekohub** (enforcement lives pekohub-side; see §5). Export/import is
  therefore cryogenic *transport*, not cloning.

With cloning (`create -f`) and transport (snapshot import) covering the
grounding space, definition mode had no honest story left — its hybrid
`--include-sessions` flag (definition + transcripts, no tooling, no
schedule) least of all.

---

## 2. Decision

**The tier boundary is an access boundary, not a packaging boundary.**
Packaging is a separate axis, and there is exactly **one export
shape**: the full-existence snapshot. Two grounding paths cover every
need, and the manifest carries no mode field.

### D0: The two groundings (vocabulary)

1. **Grow** — `peko principal create [-f template.toml]` provisions,
   defines, and runs genesis (ADR-054). Fresh DID, fresh boot state,
   runtime-guaranteed first self-turn. This is how a principal comes
   into existence, including as a clone of an existing one's DNA.
2. **Wake** — `peko principal import <snapshot.principal>` grounds a
   *transported* principal: same DID and keys, sessions/`kb/`/plans/
   tooling restored, boot state carried verbatim, authored cron
   schedule intact and rebound to the runtime id, **no genesis
   re-seed** for `organized` principals.

The import path decides *wake vs. seed* from what the package
**carries**, not from a flag (D4 below) — keyless packages are
templates and mint a fresh identity; packages with keys and local
state are transports.

### D1: One export shape — the full-existence snapshot (`.peko`)

`peko principal export` (no flags beyond `-o`) packages:

- `config/`, `identity/` (DID doc + keys), `agents/` (Shared tier);
- `sessions/` (including `sessions.json` + `peers.json` — the
  peer→session routing index travels with the transcripts it indexes),
  `cron/`, `plans/` (Local tier, authored);
- `tools/`, `skills/`, `mcp/`, `hooks/`, `kb/` (workspace tooling —
  closing ADR-047 §4's "snapshot is `tar workspace/`" gap).

`cache/`, `locks/`, and `memory_index.json` are never packaged —
derived state, rebuilt at import. `PrincipalLayers` carries digests for
`cron`, `plans`, `tools`, `hooks`, `kb` (reusing the legacy
`skills`/`mcp` fields); `LayerType` gains `Cron`, `Plans`, `Tools`,
`Hooks`, `Kb` with OCI media types. The `export_mode` manifest field of
the first draft is removed — one shape needs no declaration — and the
removed wire fields (`include_sessions`, `full_snapshot`,
`with_extensions`) are simply ignored if an old CLI sends them.

**Extension rename:** the snapshot artifact is now **`.peko`**, not
`.principal` — short, product-named, and no longer confusable with the
principal *type*. Pre-ADR-056 ADRs that say `.principal` describe the
same artifact under the old name.

### D2: Import restores what the package carries

The unpackager is layer-driven: workspace tooling →
`<shared_root>/{tools,skills,mcp,hooks,kb}/`; `cron/` → `local/cron/`,
`plans/` → `local/plans/` (gated by `import_local_state`); sessions
gated by `import_sessions`. `PrincipalImportResult` reports
`carried_local_state` so callers can tell a wake from a seed.

### D3: Cron rebinding

Cron jobs are keyed on the owning principal's runtime `PrincipalId`
(ADR-054 D3). At import, every job's `principal_id` is rebound to the
imported principal's effective id (`config.id` → DID → name — the same
resolution the cron tools and `genesis.rs` use). Per-principal schedule
files contain only jobs owned by that principal, so the rewrite is
unconditional, correct, and idempotent. A malformed file passes through
unchanged with a warning; the cron engine surfaces it on next load.

Format note: `local/cron/schedule.toml` carries a legacy `.toml` name
but is serialized as **JSON** (`CronDatabase`, `serde_json`) — the
rebinding tries JSON first and keeps a TOML fallback. The live e2e
verifies the JSON path against a real daemon-written schedule.

### D4: Wake vs. seed is decided by package contents

- Package carries Local-tier state (`sessions/`, `cron/`, or
  `plans/`) ⇒ **wake**: `boot_state` carries through verbatim. An
  `organized` principal imports as `organized` WITH its authored
  schedule, and the boot seeding pass abstains (D4 of ADR-054). A
  `defined` snapshot re-genesis's on next boot, which is exactly what
  the source was.
- Package carries no Local state (template, or a legacy
  definition-shaped package) ⇒ **seed**: `boot_state` is reset and
  ADR-054's inference applies. The pre-draft bug (an `organized`
  source landing as `organized` with no schedule) stays closed without
  needing a mode field.

### D5: The tier contract stays — only its comment changes

`paths.rs` keeps the three-tier layout and typed-path discipline. The
pinned comment states the real rule: the tier boundary is an access
boundary; packaging is a per-category policy; derived Local state is
never packaged.

### D6: The registry distributes DNA — as a plain TOML file

The template is **not a package at all**. `export_for_registry` emits a
single **`.template.toml`** file: the principal's `principal.toml` with
`id`, `did`, and `boot_state` stripped — byte-for-byte the shape
`principal create -f` already consumes. No package wrapper, no manifest,
no layers, no keys, no sessions, no cron/plans ever leave the host
through the registry. The pre-ADR-056 push path shipped the principal's
private key inside a package; that is now structurally impossible.

Why a bare TOML file rather than a package:

- **Inspectability** — `cat`, `diff`, and edit a template directly;
  no tar extraction, no manifest to decode.
- **Distribution** — one small text file; paste it into a gist, a
  repo, or a chat message. The OCI wrapper that carries it to the
  registry is transport plumbing (the TOML is the config blob, zero
  content layers), invisible to users.
- **Honesty about what DNA is** — agent prompts and installed tooling
  are *workspace content* the trunk acquires as it grows (presence =
  visibility, ADR-050); they are not heritable config. A template that
  claimed to carry them would be a snapshot pretending to be a seed.

The source principal's public DID document rides along in the OCI
descriptor as publisher provenance, established at push time by the
registry credential. A pulled template is ground with
`peko principal create <name> -f <file>` — fresh identity, genesis —
never by importing the publisher's identity. Keyless *packages* are
rejected with that guidance; TOFU pinning applies only to identity
transport (snapshots), where the DID genuinely travels.

### D7: Transport, not copy — one DID, one public principal

The full-snapshot round-trip is cryogenic transport: export → (retire
the source copy) → import elsewhere. The runtime does not police
simultaneous local copies, but the network-level commitment — recorded
here as the design intent the Pekohub work must enforce — is that only
one principal with a given DID may be publicly exposed at a time.
Rotation (`--rotate-keys`) remains an operator escape hatch that
*changes* the identity (a new card, not a shared one).

---

## 3. What ships in this change (prototype)

- `registry/packaging/principal_manifest.rs` — six new
  `PrincipalLayers` digest fields (`cron`, `plans`, `tools`, `hooks`,
  `kb`); the `ExportMode` enum and `export_mode` manifest field are
  removed (legacy manifests carrying the field still parse).
- `registry/packaging/types.rs` — `LayerType::{Cron, Plans, Tools,
  Hooks, Kb}` with OCI media types; Skills/Mcp doc comments updated
  (reused for principal workspace tooling).
- `registry/packaging/principal_packager.rs` — snapshot-only
  `collect_files` (always sessions + cron + plans + tooling, ephemeral
  excluded); `template_toml()` + reworked `export_for_registry`
  (D6: a bare stripped-`principal.toml` file as the registry
  artifact — no package, no keys, no layers).
- `registry/packaging/principal_unpackager.rs` — layer-driven import;
  `carried_local_state` on the result; wake/seed boot-state rule (D4);
  keyless packages rejected with `create -f` guidance (D6);
  `remap_cron_principal_ids` (JSON-first per D3's format note).
- `registry/client.rs` — `push_principal` pushes the template TOML
  (no package-signature pre-check; publisher = registry credential);
  `pull_principal` writes template artifacts as TOML files.
- `ipc` — `PrincipalExport` wire packet slimmed to `{name, output}`;
  handler threads the workspace/local roots into the packager; pull
  preview/import recognize template artifacts and direct the user to
  `create -f`.
- `cli` — `peko principal export` carries no mode flags.
- `common/paths.rs` — tier-contract comment rewritten (D5).
- Tests: snapshot collection (incl. ephemeral-exclusion), registry
  artifact is a plain TOML template (+ keyless-package rejection with
  `create -f` guidance), snapshot round-trip (every layer in its tier,
  `organized` verbatim, stale-id rebinding), seed-rule boot reset,
  cron remap (JSON shape + TOML fallback + idempotence + malformed
  pass-through), manifest legacy-field tolerance.
- e2e: `scripts/e2e/flows/snapshot-roundtrip-llm.sh` (real LLM,
  MiniMax) — genesis turn via `create -f`, trunk-authored state
  (CronCreate attempt with a schema-safe clone fallback, skill, kb
  note, plan, hand-stamped `organized`), snapshot tar inspection,
  remove → import → per-tier restore with stale-id rebinding,
  organized reboot abstaining from re-seed, a real `peko send`
  round-trip on the imported principal. Verified green end-to-end;
  the `genesis-pipeline-llm` regression flow is also green.

**Round-trip property pinned by tests and e2e:** create → self-organize
(authored cron job, plan, installed tool, kb notes) → export → remove →
import ⇒ sessions present, schedule restored with rebound ids, plans
restored, tooling catalog intact, `boot_state = organized`, no genesis
re-seed.

## 4. Consequences

### Positive

- **The round-trip preserves existence, not just definition.** Working
  context, self-authored cadence, plan state, and installed tooling
  survive transport; ADR-054 D4's promise holds across hosts.
- **DID singularity is structurally enforced.** There is no surface
  left that forks a DID across runtimes: clones mint fresh identities,
  transports move one creature, registry templates carry no identity.
  The audit stream per DID stays singular and correlatable.
- **The registry is safe by construction.** Keys and transcripts
  cannot leak through push — the artifact no longer contains them.
- **Templates are the sharing story.** Persona distribution =
  `create -f`-compatible DNA signed by its source; sharing a principal
  never implies sharing its diary.

### Negative / costs

- **Full snapshots are big and sensitive — by design.** The export
  surface now always includes transcripts; the consent boundary moved
  from export flags to *who you hand the file to*. Registry push is
  safe (D6); human-to-human snapshot sharing is explicitly a
  trust-the-recipient operation.
- **The push/pull UX needs its pekohub story.** Single-public-exposure
  enforcement and a source-retirement convention on transport live
  network-side (§5); until then the invariant is convention, not law.
- **Hooks travel in snapshots.** A malicious snapshot can install
  hooks that fire on every turn. Mitigations: the ADR-046 audit canary
  covers `tools/`/`hooks/`/`mcp/` drift on next boot; the production
  pass should gate tooling import alongside `principal:write_agents`
  (the prototype relies on the canary).
- **Rebinding is id-based, not semantic.** Jobs referencing model ids
  or channel ids absent on the target host degrade at dispatch, not
  import.

## 5. Deferred (tracked follow-ups, not promised)

- **Single-public-exposure enforcement on Pekohub** (D7): DID
  uniqueness at the exposure layer; a retirement convention for the
  source runtime on transport (e.g. export records the intent; `remove`
  completes it).
- **Template pull UX:** complete the pull flow for template artifacts
  (currently the preview/import steps direct the user to
  `peko principal create <name> -f <file>` with the artifact kept in
  the cache dir); surface the publisher provenance in the CLI output.
- **Detached endorsement signatures:** sign template TOML bytes with
  the source DID (sidecar `.sig` or registry-recorded attestation) so
  provenance is cryptographic, not just the push credential.
- **Import preview surfaces restored layers:** extend
  `PrincipalImportPreview` with the layer inventory (sessions count,
  cron job names, hook ids) so `peko principal import` shows what a
  snapshot will bring back.
- **Model-id validation on import:** warn when the snapshot's
  `preferred_model_id` or job payloads reference models absent from
  the target runtime.
- **Frozen-time semantics for cron:** decide fire-catch-up vs.
  re-anchor-to-wake-time for overdue jobs on wake.
- **`kb_dir` on `SharedLayout`:** the `kb/` tree (ADR-055) is scanned
  via `workspace_path` joins; add the typed field for layout
  completeness.
- **Genesis-job hygiene on import:** a snapshot taken mid-genesis
  carries the one-shot `genesis` job; verify idempotence against
  `seed_boot_defaults` for non-organized imports.
- **CronCreate-authored job in e2e:** the flow's trunk-authored cron
  job currently falls back to a schema-safe schedule clone when the
  model skips the tool call; revisit MiniMax tool-call reliability
  before tightening the flow to the real path only.

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
- [ADR-032](ADR-032-runtime-identity-and-multi-host-awareness.md) —
  runtime identity; the DID-singularity backdrop for D7.
- `peko-rs/core/src/common/paths.rs` — the three-tier contract (D5).
- `peko-rs/core/src/registry/packaging/` — this ADR's implementation.

---

*Version 0.2.0 · Full-Existence Principal Snapshot · 2026-09-14*
