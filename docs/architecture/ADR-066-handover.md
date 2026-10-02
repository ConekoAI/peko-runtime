# ADR-066 Implementation Handover

**Date:** 2026-10-02
**Branch:** `adr-066-pure-workspace-tooling` (off `master`)
**Status:** P1 + P2 landed and green; P3 designed and ~80% implemented but
reverted to keep the tree green — full WIP preserved in
[`adr-066-p3-wip.patch`](adr-066-p3-wip.patch). P4–P6 not started.

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
23ba4c26  P2 (ADR-066): delete the capability gate        ← HEAD, all gates green
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

## 5. P3 — IN PROGRESS, reverted (the big one)

**Goal (ADR §2 D2):** split the daemon-global `ExtensionCore` singleton
(`global_core()`, ~300 mentions) into named pieces; engine seam
`ToolFunnel` shrinks 12 methods → 3; single-point tool-call audit.

The quota cut hit mid-refactor at 20 lib + 279 lib-test compile errors
(remaining: engine test doubles and un-converted call sites). The WIP
was reverted to keep HEAD green; **the complete implementation-in-flight
is in `adr-066-p3-wip.patch`** (7,670 lines, applies with
`git apply docs/architecture/adr-066-p3-wip.patch` onto 23ba4c26).

### The design in the patch (validated by how far it compiled — finish it, don't redesign)

- **`tools/catalog.rs`** — `ToolCatalog`: `(name, PrincipalId) →
  (Arc<dyn Tool>, ToolMetadata)` map reusing
  `framework::registry::SharedRegistry`. Built-ins/MCP proxies register
  under `PrincipalId::system`; per-agent tools register under the owning
  principal and shadow system entries on read. No capability filter;
  the F34 `ToolExposure` filter survives until P4.
- **`tools/dispatcher.rs`** — `ToolDispatcher`: the single execution
  point. Composes `ToolCatalog` + `HookRegistry` (now used ONLY for the
  observe-only Pre/PostToolUse points until P4) + `AsyncExecutionRouter`
  (timeout/panic-isolation/detach) + optional `Observability` audit
  sink. Emits the `tool.call` audit event here (ADR §4 requirement:
  attribution consolidates at this one point).
- **`tools/runtime.rs`** — `ToolingRuntime`: the ExtensionCore
  replacement composition (catalog + dispatcher + hooks + `SessionKeys`
  + `ExtensionServices` + `prompt_providers` + a `tool_bag_installed`
  guard so `ensure_principal_tool_bag` installs once per daemon, not
  per run). Built once in `daemon::state`, threaded explicitly
  `PrincipalManager → PrincipalContext → Agent →` engine seams.
  **No process-global accessor** — tests construct their own.
- **`tools/prompt_sections.rs`** — `PromptSectionProvider` trait
  (`section()` / `priority()` / `render(PromptSectionInput)`); built-in
  sections: `identity`, `roles`, `skills`, `workflows`,
  `session_context`. Workspace-hook `PromptSection` binds keep riding
  the hook registry inside the same aggregation until P4 unifies.
- **`tools/session_keys.rs`** — `SessionKeys`: per-agent-DID session-key
  table (the issue-#68 side table, extracted from ExtensionCore).
- **`extension-api/src/tool_funnel.rs`** — the 3-method seam:
  `execute(ToolCallSpec)`, `list_tool_definitions`,
  `render_prompt_sections(PromptSectionRequest) → PromptSections`.
  `ToolCallSpec` carries identity + abort (no grants). The non-execution
  surface (lifecycle hook firing, session keys, F33/F35 probes) moved to
  a new **`EngineHooks`** trait in the same file. Dep-graph rule stands:
  engine must not depend on root.

### To finish P3

1. Apply the patch; `cargo check --workspace --all-targets` and burn
   down the errors. The bulk (279) are test doubles —
   `engine/src/compaction_driver.rs` (~1119) and
   `engine/src/prompt/renderer.rs` (~1248) funnel doubles, engine
   `funnel.rs`/`tool_executor.rs`/`agentic_loop.rs` call sites — plus
   root call sites still naming `ExtensionCore`/`global_core()`.
2. Delete `ExtensionCore`, `global_core()`, `init_global_core`
   (`daemon/state.rs`) and the `Arc<ExtensionCore>` threading
   (`agents/agent.rs`, `daemon/cron_engine/`, IPC handlers, subagent
   executor). Hollow out `framework/core/registry.rs`; park whatever
   hook-dispatch remnants P4 still needs.
3. Keep PreToolUse/PostToolUse firing through `HookRegistry` (P4
   replaces it — do not delete HookRegistry/HookPoint in P3).
4. Optional tail: the inert capabilities shell chain (§4 above).
5. Full gate + commit.

## 6. P4 — not started

ADR §2 D3/D5, §3 P4. `WorkspaceHookDispatcher` (~6 fired points:
PreToolUse/PostToolUse observe-only 2 s soft-fail, Stop, AfterAgent,
PromptSection, SessionContextBuild — a `Vec` fired in registration
order; no priorities/wildcards/companion codegen) replaces
`HookRegistry` for workspace hooks; rewire `workspace_hooks.rs` +
`command_handler.rs`. Delete `HookRegistry`, the 790-LOC `HookPoint`
zoo, `tool_registration.rs` companion-hook codegen. Delete
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
- P2's resume agent (`agent-15`) holds P1–P3 working context if this
  session is still alive — resuming it with "continue" is cheaper than
  a cold start.
