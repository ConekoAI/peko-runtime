# ADR-066 Implementation Handover

**Date:** 2026-10-02
**Branch:** `adr-066-pure-workspace-tooling` (off `master`)
**Status:** P1–P5 landed, with all standard gates green. P5's final unit
run passed 2,768 tests with zero failures and three existing ignored tests.
All 114 CLI unit tests pass with `PEKO_UNLOCK_METHOD=passphrase`. The full
Docker mock-LLM integration tier passed 51 tests, and the stack was torn down.
P6 is not started.


The landed P3 recovery patch was removed in P4; its history remains in Git.

This document is the complete context needed to resume the work. Read
[ADR-066](adr/ADR-066-pure-workspace-tooling.md) first — it is the decision
record and the phase plan (§3).

---

## 1. What ADR-066 is

Retire the extension framework, capability gate, and remote-registry
machinery in favor of pure workspace/filesystem tooling: a principal gets
everything the runtime offers (all built-in tools, filesystem, internet)
under trust-and-audit (ADR-046); packaging is a runtime-local tar snapshot;
pekohub distributes seeds hub-side (`push`/`pull`/`search` deleted).
Ten decisions (D1–D10), six phases (P1–P6), each phase a tree-green commit.

## 2. Commit state

```
P5        This phase: flat local snapshots / registry retirement
1c97cd78  P4: workspace hook dispatcher / exposure deletion
49720c79  P3: tooling catalog / dispatcher / explicit runtime
74578be3  docs: ADR-066 implementation handover + P3 WIP patch
23ba4c26  P2 (ADR-066): delete the capability gate
7d6dbe04  P1 (ADR-066): re-home async_exec, delete inert extension framework
336ffd55  docs: ADR-066 pure workspace tooling
```

Standard gate (run from repo root, all must pass before every commit):

```bash
cargo fmt --all && cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib
bash scripts/check_module_boundaries.sh
python3 scripts/check_workspace_deps.py
```

## 3. P1 — landed (net −4,045 LOC)

- `extensions/framework/async_exec/` → `core/src/async_exec/` (+
  `inbox.rs`); ~90 import sites rewritten. It is the background-task
  runtime (Bash bg, AsyncSpawn, cron, messaging), not extension logic.
- Deleted inert framework: `ExtensionStore`, `discovery.rs`,
  `extension_storage.rs`, `store_trait.rs`, `adapters/`
  (`ExtensionTypeAdapter` — zero registered implementors),
  `services/config_service.rs`, `services/tool_execution.rs`.
- Live stragglers re-homed, not deleted: `adapters/builtin_tools.rs` →
  `principal/runtime/builtin_tools.rs`; `GlobalExtensionItem` →
  `principal/catalog.rs`; `ToolExecutionConfig` inlined into
  `async_router.rs`; YAML frontmatter parsing inlined into
  `role/adapter.rs`.
- `daemon/state.rs` no longer constructs `ExtensionStore`/`Services`;
  `ToolHost`/`PrincipalHost` IPC traits shed the store plumbing.

## 4. P2 — landed (net −2,477 LOC)

- Capability gate fully deleted: execution gate in
  `framework/core/registry.rs::invoke_hook`, wire-catalog grant filter,
  `role:`/`skill:`/`agent:<id>` spawn/invoke gates,
  `principal/catalog.rs` projection, import-time capability negotiation.
- `principal.toml [capabilities].grants` is parsed-then-dropped with a
  one-time `tracing::warn!` deprecation; never persisted.
  `CapabilityEvaluator`, `starter_bundle`, `Capability::matches` deleted.
- Grant threading removed from `HookInput::ToolCall`, `ToolContext`,
  `execute_tool_via_hook` (now 9 args + abort), IPC attribution. Wire
  `capabilities` fields are parsed-and-ignored (do not remove the serde
  fields until P6).
- **D9 ownership rewrite** (`common/authority.rs`): write accessors take
  no caps and are `async`. `Subject::User` writes anything;
  `Subject::Principal(did)` writes only its own principal's tiers
  (compared against the target's on-disk DID — `PrincipalId` is
  dual-form, `prin_<uuid>` vs bare DID); crossings fail closed with
  `AuthorityError::OwnershipDenied` + a
  `principal.cross_principal_write_denied` Security audit event
  (opt-in sink: `RuntimeAuthority::with_audit_sink`, wired in
  `PrincipalHost::authority_for` and the unpackager).
- New pinned tests: fresh principal (no grants) sees full wire catalog +
  Bash executes (`engine/tool_runtime.rs`); cross-principal denial +
  durable Security audit event (`common/authority.rs`).

### P2 leftovers for later phases

- `Capabilities` survives as a wire-tolerance shell (data ops only) in
  `extension-api/src/capabilities.rs` — P6 folds the crate.
- Inert shell chain (renders nothing from an always-empty set):
  `PrincipalConfig.capabilities` → `RouterContext.capabilities` →
  `PrincipalContext.capabilities` → `Agent::with_principal_capabilities`
  → `AgentView::principal_capabilities`, feeding the `capability_diff`
  prompt tracker and compaction `permission_policy_summary`. Delete in
  P3 (optional) or P6.
- `ModelCall` genuinely needs `principal_id` (meter/model resolution) —
  unknown-session-key IPC calls now execute *unattributed* (pinned test).
- `RuntimeMetadataResponse.capabilities` and MCP `ClientCapabilities`
  are unrelated namesakes — do not touch.

## 5. P3 — implemented (ADR §2 D2)

The saved WIP was applied and completed without changing its catalog /
dispatcher / runtime / prompt-provider / session-key design:

- `tools/catalog.rs`: executable tool + metadata map keyed by `(name,
  PrincipalId)`, with principal entries shadowing the system catalog;
  stable sorted wire definitions. Exposure/search survive until P4.
- `tools/dispatcher.rs`: the single execution point. Preserves schema
  validation, workspace path injection, abort bridging, interrupt notices,
  timeout/detach, and panic isolation. Observe-only Pre/Post hooks fire
  even for unknown tools; one durable `tool.call` event covers success,
  tool errors, unknown tools, validation failures, and panics. Agent DID,
  caller, principal, and session attribution are retained; params are
  represented by a full SHA-256 digest rather than raw content.
- `tools/runtime.rs`: explicit composition built in `daemon/state.rs`,
  passed through `PrincipalManager` / `RouterContext` / `PrincipalContext`
  to root, peer, completion-wake, cron, and recursive subagent turns.
  Tests construct isolated runtimes; no process-global tool accessor.
- `ToolFunnel` has three methods: `execute(ToolCallSpec)`,
  `list_tool_definitions`, and `render_prompt_sections`. `EngineHooks`
  holds lifecycle/compaction hooks, session keys, and catalog probes;
  `ToolingSeam` combines both without an engine → root edge.
- Built-in prompt handlers are plain `PromptSectionProvider`s. Independent
  sections still render concurrently, with the 2-second soft-fail budget,
  stable aggregation, and custom-section retraction preserved.
- Workspace tools install once **per principal** under a serialized guard;
  prompt providers install once per runtime. This fixes the WIP's runtime-
  wide install flag that would have skipped the second principal's tools.
  Daemon-configured built-ins are preserved during workspace installation.
- Surviving workspace hooks are owner-scoped for tool, prompt, and
  Stop/AfterAgent dispatch. Lifecycle payloads carry principal/workspace
  context and the runtime supplies the agent's current session key.
- Deleted `ExtensionCore`, its global accessors, the unused async bridge,
  tool registry, and companion-hook registration/codegen. `HookRegistry`
  / `HookPoint` remain until P4; their hook-dispatch tests are retained.
- The inert capabilities shell chain remains for P6. MCP
  `ClientCapabilities` / `RuntimeMetadataResponse.capabilities` are unrelated.

Verification: full workspace/all-target compile and all standard gates
passed: formatting, clippy, 2,908 unit tests (zero failures; three existing
ignored tests), module boundaries, and workspace dependencies. Dedicated regressions cover
durable single-event audit (including errors/panics), two principals'
workspace-tool and prompt isolation, scoped tool/lifecycle observers, and
existing renderer/mock-agent tool round trips. Docker integration could
not start: the local server timed out in the bounded preflight. Run
`make test-integration` and `make docker-down` when Docker is available,
before merging.

`API_SURFACE.md`, `CHANGELOG.md`, source docs, and the ignored local
`AGENTS.md` are updated for P3. P4 follows below.

## 6. P4 — landed

- `extensions/workspace_dispatcher.rs` replaces HookRegistry with a vector
  of handlers at six points. Principal ownership is typed; no ExtensionId /
  HookId bookkeeping, wildcard matching, or hook priorities remain.
- Each handler runs in registration order with a two-second soft-fail budget;
  handled/error/panic/timeout results cannot veto execution or skip observers.
- The scanner loads sorted directories, preserves manifest bind order, and
  validates the whole manifest before registering. Tool selectors are exact
  or absent (all tools); old wildcard manifests are rejected with guidance.
  Legacy priority fields are tolerated and ignored. SessionContextBuild can
  now be bound in a workspace manifest.
- Command hooks use explicit principal/workspace/session/agent context and
  kill subprocesses on drop. Prompt aggregation, built-in augmentation,
  custom tail sections, and retraction remain intact. A session_context
  PromptSection binding augments SessionContextBuild output in registration order.
- Deleted HookRegistry, HookPoint, handler/context/binding scaffolding, hook
  telemetry, ToolExposure, catalog filters, enable_tool_search, __tool_search,
  discovery metadata, and scoring. Removed inert AgentInit/Shutdown and
  compaction/session-state seams; the compaction backend/cache behavior stays.
- Removed the landed P3 WIP patch. Updated API_SURFACE, DATA_MODEL, workspace
  and tool docs, source comments, CHANGELOG, and local ignored AGENTS.md.
- Verification: formatting, all-target clippy, 2,845 workspace unit tests
  (zero failures; three existing ignored tests), module boundaries, and all
  81 forbidden dependency-edge rules pass. Hook regressions cover observe-only
  execution despite handled/error/panic/timeout results, owner identity,
  registration order, all six command points, prompt aggregation (including
  session_context augmentation), and existing renderer retraction behavior.
  Docker again failed the bounded preflight; integration remains unverified
  before merge. P5 is the next implementation phase.

## 7. P5 — landed (ADR §2 D6–D8)

- `.peko` remains tar.gz with ordinary file entries. `PrincipalManifest` is
  flat: `format = "peko-snapshot-v1"`, name, DID, created-at, peko version,
  optional description, and a sorted `path → sha256` inventory. No layer
  tarballs, signatures, TOFU pins, blob descriptors, or embedded archives.
  Legacy OCI manifests fail with “re-export from the source runtime” guidance.
- ADR-056 snapshot collect/restore semantics are preserved: roles, identity,
  sessions, cron (id rebinding), plans, workspace tooling and knowledge base;
  cache/locks/memory index remain excluded. The live-state test removes the
  source directories before import, then verifies DID, state and boot behavior.
  Keyless packages still direct the caller to `peko create -s`; seeds are plain
  TOML. A local `seed_toml` helper survives without registry transport.
- Validation always fails before writes on corrupt/missing/undeclared files,
  unsafe paths, duplicate archive entries, non-file tar entries, malformed
  config/keys, or DID/key mismatches. `--force` only permits overwrites.
- `ExecutableInventory` captures DID, payload file count, sorted skill ids,
  and complete hook/MCP manifest definitions with ids and paths. Verbatim
  definitions retain binds/commands/args and malformed manifests. The local
  CLI prints it before import; a durable `principal.snapshot_import` Security
  event records the same inventory and caller before writes. The CLI carries
  the preview's manifest checksum into import to reject intervening changes.
  No confirmation UI remains; `--yes` is a hidden, ignored compatibility flag.
- Deleted remote registry client/config/cache and OCI models, trust store,
  daemon registry state, inert extension push/pull DTOs, CLI push/pull/search/
  registry/global registry flag, and all three registry IPC paths + packets.
  PekoHub login retains explicit host selection; signed peer transport stays.
- Replaced obsolete OCI/signature/registry test suites with local tar inspection
  and CLI import coverage. `SkillFixture.requires/provides` and registry fixture
  descriptors are gone; fixtures now install actual workspace skills. Updated
  Makefile's integration targets, API_SURFACE, DATA_MODEL, README, workspace
  docs, config example, CHANGELOG and local ignored AGENTS.md.
- Verification: final full workspace unit run: 2,768 passed, zero failed, three
  existing ignored, 19 suites; all-target clippy, fmt, module boundaries, and
  all 81 dependency rules pass. The 114 CLI unit tests also pass with the
  headless test vault forced to passphrase mode (the default macOS mode made
  the existing model/vault test fail). `make test-integration` passed all 51
  tests, including CLI import/tar inspection, send, recursive subagents, all
  filesystem/Bash round trips, mock sequences, permission flows, signed tunnel
  transport, and the daemon tunnel chat with the mock LLM. `make docker-down`
  completed successfully.

## 8. P6 — not started

ADR §2 D10, §3 P6. Fold `extension-api`: survivors (`default_*_dir`,
completion/inbox contracts, session/subagent types) into
`peko-tools-core`/`peko-session`/root; `reserved_params.rs` → beside
MCP; delete the crate and update the 81-entry
`scripts/check_workspace_deps.py` table. Doc sweep: `AGENTS.md` (§3.2,
§5, §6.4 were touched in P1/P2 — finish §6), `API_SURFACE.md`,
`DATA_MODEL.md`, `PRINCIPAL_WORKSPACE.md`, `builtin-tools.md`,
`config.example.toml`; CHANGELOG entries per landed phase (P1/P2 entries
still owed). Remove the parsed-and-ignored IPC `capabilities` wire
fields (the deprecation window P2 opened). The local import preview also
retains an always-empty `extensions` field from the deleted embedded archive
model; remove it alongside the remaining wire tolerance shells.

## 9. Operational notes

- **`AGENTS.md` is gitignored** — P1/P2 updated it on disk (§3.2, §5,
  §6.4); those edits are real but never appear in commits.
- **Docker mock integration passed for P5** — 51 tests, zero failures. This
  also resolves the P3/P4 pre-merge integration gap recorded in their historical
  notes above. `make docker-down` completed and removed the test stack.
  Real-LLM tests remain unexecuted.
- **Doc discipline per phase** (repo rule): when code is deleted or
  renamed, update the docs that name it in the same commit.
- The original demolition map (subystem LOC, call-site counts, coupling
  inventory) was produced by an explore subagent; its findings are
  distilled into ADR-066 §1 — trust the ADR, re-verify against the
  compiler when it disagrees.
