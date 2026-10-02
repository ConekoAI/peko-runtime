# ADR-066 Implementation Handover

**Date:** 2026-10-02
**Branch:** `adr-066-pure-workspace-tooling` (off `master`)
**Status:** P1 + P2 landed; P3 implemented using the saved WIP and
subsequent fixes, with all standard gates green. Docker integration is
unverified because the local Docker server did not respond within a
15-second preflight. P4–P6 are not started.

The original [`adr-066-p3-wip.patch`](adr-066-p3-wip.patch) is retained as
historical recovery material for the P2 base; do not apply it over P3.


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
P3        This phase: tooling catalog / dispatcher / explicit runtime
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
`AGENTS.md` are updated for P3. P4 is the next implementation phase.

## 6. P4 — not started

ADR §2 D3/D5, §3 P4. `WorkspaceHookDispatcher` (~6 fired points:
PreToolUse/PostToolUse observe-only 2 s soft-fail, Stop, AfterAgent,
PromptSection, SessionContextBuild — a `Vec` fired in registration
order; no priorities/wildcards/companion codegen) replaces
`HookRegistry` for workspace hooks; rewire `workspace_hooks.rs` +
`command_handler.rs`. Delete `HookRegistry` and the 790-LOC `HookPoint`
zoo (companion-hook codegen was removed with P3's execution path). Delete
`ToolExposure` (all tools are Direct), the exposure filter in the
catalog, `AgentConfig::enable_tool_search`, `tools/builtin/tool_search.rs`,
`framework/core/scoring.rs`. Verify hook integration tests (observe-only
fires, soft-fail, PromptSection bind renders a tail section).

## 7. P5 — not started

ADR §2 D6–D8, §3 P5. Keep ADR-056 snapshot semantics (collect rules,
wake/seed, cron rebinding, `path_safety` — test-pinned, do not regress);
replace the container: flat `manifest.toml` inventory +
`path → sha256` map; delete the OCI layer model (`LayerType` media
types, `PrincipalLayers` digests), `registry/client.rs`,
`registry/manifest.rs`, `registry/config.rs`, `agent_registry.rs`,
`packaging/trust_store.rs` (~3.6k LOC), the `push`/`pull`/`search`/
`registry` CLI subcommands, and the 3 `RegistryClient` IPC sites.
D8: import emits a Security audit event + operator-visible inventory of
executable content (hooks + binds + commands, MCP servers, skills).
Old OCI-manifest snapshots rejected with "re-export from source".
P1 note: `SkillFixture.requires/provides` in
`tests/common/package_builder.rs` are write-only — prune here.
Verify the ADR-056 round-trip property (create → self-organize → export
→ remove → import ⇒ sessions, rebound cron, verbatim boot_state, intact
tooling) and `tar -tf` inspectability.

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
fields (the deprecation window P2 opened).

## 9. Operational notes

- **`AGENTS.md` is gitignored** — P1/P2 updated it on disk (§3.2, §5,
  §6.4); those edits are real but never appear in commits.
- **Integration tier has not been run on this branch** — P1 changed the
  daemon composition root; run `make docker-up && make test-integration
  && make docker-down` before merging anything. `#[ignore]`d real-LLM
  tests were edited in P2 but not executed.
- **Doc discipline per phase** (repo rule): when code is deleted or
  renamed, update the docs that name it in the same commit.
- The original demolition map (subystem LOC, call-site counts, coupling
  inventory) was produced by an explore subagent; its findings are
  distilled into ADR-066 §1 — trust the ADR, re-verify against the
  compiler when it disagrees.
