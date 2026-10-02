# ADR-066: Pure Workspace Tooling — Extension Framework, Capability Gate, and Registry Client Retired

**Status:** Accepted
**Date:** 2026-10-01
**Author:** rlsn (with WorkBuddy)
**Related:** [ADR-046](ADR-046-trust-and-audit.md) (the audit-as-security-model
posture this ADR relies on and completes),
[ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(workspace trust boundary — superseded in remaining part),
[ADR-050](ADR-050-capabilities-as-workspace-files.md) (capabilities as
workspace files — completed here),
[ADR-056](ADR-056-full-existence-principal-snapshot.md) (snapshot
semantics — retained; container format simplified),
[ADR-060](ADR-060-seed-as-the-definition-artifact.md) (seed vocabulary;
pekohub distributes seeds),
[ADR-062](ADR-062-retire-universal-tools.md) (same trajectory),
[ADR-064](ADR-064-agents-to-roles-terminology.md) (`role:*` grants —
deleted here).

**Note:** This is a clean-slate pre-production design. Backward
compatibility with Peko 0.1.0 is intentionally discarded so the codebase
and UX remain coherent (same posture as ADR-050).

---

## 1. Context

ADR-047 declared the principal workspace the tooling trust boundary, and
ADR-050 made workspace files the only management surface. The
documentation already claims the end state —
[PRINCIPAL_WORKSPACE.md](../PRINCIPAL_WORKSPACE.md): *"there is no
extension registry, no canonical funnel, and no manifest validation
beyond presence."* The code never caught up. What still lives under
`peko-rs/core/src/extensions/` (~30k LOC) and `peko-rs/core/src/registry/`
(~7.5k LOC) is three things mixed together:

1. **Dead generality.** The generic extension framework —
   `ExtensionStore`, `discovery.rs`, `extension_storage.rs`,
   `store_trait.rs`, the `ExtensionTypeAdapter` trait (~3,000 LOC) —
   scans directories and finds nothing: **zero adapters are registered**
   in production (both `register_adapter` call sites were deleted in
   PR-C.5; `adapters/mod.rs` says so itself). `ToolExposure`
   (`Direct` / `DirectModelOnly` / `Deferred` / `Hidden`) plus the
   `__tool_search` machinery and its word-overlap ranker (~800 LOC)
   have **zero non-`Direct` producers** — every tool is `Direct`, and
   `AgentConfig::enable_tool_search` defaults to false.

2. **A capability gate that gates nothing.** The fail-closed
   `tool:<name>` / `role:<name>` / `skill:<name>` grant check fires on
   every tool call, but `Capabilities::starter_bundle()` ships
   `tool:*` / `role:*` / `skill:*` wildcards, so every principal already
   has everything. ADR-046 established the audit log as the security
   model and proved (for the self-modification gate) that permission
   layers above a shell tool are theater. The gate's *plumbing* is the
   real cost: `CapabilityEvaluator`, the `principal/catalog.rs`
   active-extensions projection, grant strings threaded per-call through
   `HookInput::ToolCall` → `ToolContext` → IPC attribution, and the
   11-argument `ExtensionCore::execute_tool_via_hook` signature — ~10
   files hold a check that never fires.

3. **Real machinery wearing a framework costume.** `async_exec/`
   (~4,500 LOC) is the background-task runtime for Bash background,
   `AsyncSpawn`, cron, and messaging — physically trapped inside
   `extensions/framework/`. `BuiltinExecuteHandler` +
   `transport/async_router.rs` is the *actual* tool execution path:
   `ToolContext` construction, timeout, panic isolation, abort-signal
   bridging, reserved-param injection. The MCP client stack (~8,100
   LOC) is ~90% framework-independent. The workspace scanners (roles,
   skills, hooks, MCP configs) are self-contained file walkers.

Meanwhile `ExtensionCore` is a daemon-global singleton (~335 mentions
across ~45 files) conflating five responsibilities — tool map, hook
dispatch, prompt-section rendering, per-agent session-key side table,
`Arc<dyn Tool>` side table — and `peko-engine` reaches all of it through
a 12-method `ToolFunnel` seam.

On the packaging axis, ADR-056 made export a full-existence `tar.gz`
snapshot but kept the OCI layer model (`LayerType` media types,
`PrincipalLayers` digest manifest, trust store) and the remote registry
client (`client.rs` + `manifest.rs` + `config.rs` + `agent_registry.rs`
≈ 3,300 LOC) for `peko push` / `peko pull` / `peko search`. Pekohub now
distributes seeds hub-side (ADR-060: a seed is a stripped
`principal.toml`; the hub carries DNA, never an existence). The
runtime's registry client is dead weight; packaging only needs to move a
workspace between runtimes the operator controls.

**Trust assumption, stated plainly:** the runtime runs inside an
operator-trusted environment. A principal gets everything the runtime
offers — all built-in tools, filesystem access, internet access — and
the security model is trust-and-audit (ADR-046): every action lands in
the JSONL audit log; awareness, not permission pop-ups.

## 2. Decision

### D1 — A principal gets everything the runtime offers

The capability gate is deleted, not relaxed:

- `CapabilityEvaluator`, `principal/catalog.rs` (the active-extensions
  projection), `extension-api/src/capabilities.rs`, and every
  `tool:*` / `role:*` / `skill:*` / `agent:*` grant check — execution
  gate, wire-catalog filter, per-agent registration filter, subagent
  spawn gate, `SkillTool`'s own prefix check — are removed.
- Presence = visibility = **executability** (completes ADR-050 D3): a
  role file in `roles/` is spawnable; a skill in `skills/` is
  invocable; every built-in tool is in the wire catalog and runs.
- `principal.toml` `[capabilities].grants` is ignored on load with a
  one-time deprecation warning; newly created principals carry no
  grants section.
- IPC wire fields carrying `capabilities: Vec<String>` are deprecated —
  parsed-and-ignored for one release window, then removed (pre-launch
  posture makes this short).

### D2 — The funnel collapses to a catalog + dispatcher

`ExtensionCore` is replaced by two small, explicit things:

- **`ToolCatalog`** (per-principal): a `name → (Arc<dyn Tool>,
  metadata)` map with `register` / `get` / `tool_definitions()`. This
  is what `ToolRuntime::register_builtins` and the MCP proxies register
  into, and what `AgenticLoop::build_tool_definitions` lists.
- **`ToolDispatcher`**: one function that executes a tool call —
  builds `ToolContext` (principal/session/agent identity, abort
  receiver), applies the execution timeout and panic isolation (the
  behavior worth keeping from `async_router.rs`), bridges abort signals,
  injects reserved params, and emits the audit event. Replaces
  `BuiltinExecuteHandler` + `HookRegistry` priority dispatch +
  companion-hook codegen for the execution path.

The engine seam (`ToolFunnel`, 12 methods) shrinks to three: `execute`,
`list_tool_definitions`, `render_prompt_sections`. The dep-graph rule
(engine must not depend on root) is preserved — the seam stays a trait
in the contract crate.

`HookRegistry`, the 790-LOC `HookPoint` zoo, priority sorting, wildcard
point matching, companion-hook auto-generation (`tool_registration.rs`),
`ExtensionId`/`HookId` bookkeeping, `services/tool_execution.rs`, and
`services/config_service.rs` are deleted.

### D3 — Workspace hooks survive on a minimal dispatcher

User-facing workspace hooks are a shipped feature (ADR-052 D6) and do
not silently die. A `WorkspaceHookDispatcher` (~6 fired points:
`PreToolUse`, `PostToolUse` observe-only with the existing 2 s
soft-fail, `Stop`, `AfterAgent`, `PromptSection`, plus the session-context
build point) replaces `HookRegistry` for workspace hooks only. No
priorities, no wildcards, no codegen — a `Vec` of handlers fired in
registration order. `workspace_hooks.rs` (scanner) and
`command_handler.rs` (spawns the hook command) are rewired to it.

The per-turn prompt handlers (`WorkspaceRolesPromptHandler`,
`WorkspaceSkillsPromptHandler`, `WorkspaceIdentityPromptHandler`,
`WorkspaceWorkflowsPromptHandler`, session-context handlers) convert
from `HookHandler` impls to plain prompt-section providers registered
on the catalog. Their file-walking and mtime-cache logic is untouched.

### D4 — `async_exec` is re-homed, not deleted

`extensions/framework/async_exec/` moves to
`peko-rs/core/src/async_exec/` (mechanical move + import rewrite). It
is the task runtime — Bash background, `AsyncSpawn`/`AsyncOutput`, cron
firing, messaging — and has nothing to do with extensions. Its dispatch
closures call the `ToolDispatcher` (D2) directly instead of
`execute_tool_via_hook`. `framework/inbox.rs` (`SessionInbox`) moves
with it.

### D5 — `ToolExposure` and tool search are deleted

`ToolExposure` keeps one effective value (`Direct`), so the enum, the
wire-catalog exposure filter, `AgentConfig::enable_tool_search`, the
`__tool_search` tool (~440 LOC), and `framework/core/scoring.rs` go.
Every registered tool appears in the wire catalog.

### D6 — Packaging is a plain tar of the principal's state

ADR-056's snapshot **semantics** are retained unchanged — the collect
rules (config + identity + roles + sessions + cron + plans + skills +
mcp + hooks + kb; `cache/`/`locks/`/`memory_index.json` never packaged),
the wake-vs-seed rule (D4), cron id rebinding (D3), and
`path_safety` — because they are correct and test-pinned. The
**container** is simplified:

- `manifest.toml` becomes a flat inventory: name, DID, created-at,
  peko version, and a `path → sha256` file map (cheap corruption
  detection on import). The OCI layer model — `LayerType` media types,
  `PrincipalLayers` digest fields, blob descriptors — is deleted.
- The artifact remains `.peko` (`tar.gz`). Snapshots written by the
  OCI-manifest packager are rejected with a clear "re-export from the
  source runtime" message (clean-slate posture).

### D7 — The registry client and `push`/`pull` are deleted

Pekohub distributes seeds hub-side; the runtime no longer speaks to a
remote registry at all:

- `peko push`, `peko pull`, `peko search`, and `peko registry` CLI
  subcommands are removed; the three `RegistryClient` IPC call sites and
  their packets are removed.
- `registry/client.rs`, `registry/manifest.rs`, `registry/config.rs`,
  `registry/agent_registry.rs`, and `packaging/trust_store.rs` are
  deleted (~3,600 LOC). No signatures: a seed from pekohub is grounded
  with `peko create -s <file>.seed.toml` exactly like a hand-written
  one; a snapshot is imported from a local path. Trust is the
  operator's, expressed by what they choose to download and import.
- The pekohub **tunnel** (peer DM transport, ADR-035) is unaffected —
  only the registry client dies.

### D8 — Import is an audited trust decision

A snapshot carries executable content: `hooks/*/hook.toml` commands run
on every matching tool call, `mcp/*/server.json` spawns processes. With
the trust store gone there is no signature check — so import **audits
loudly** instead:

- A `Security`-severity audit event records the import with the full
  executable inventory: hook ids + binds + commands, MCP server ids +
  commands, skill ids, plus file count and DID.
- The same inventory prints to the operator's terminal before the
  import completes.

This closes ADR-056 §4's deferred "gate tooling import" follow-up the
ADR-046 way: awareness, not permission. The boot-time drift canary
(`tools/`/`hooks/`/`mcp/` hashing) remains the standing net.

### D9 — The cross-principal filesystem boundary survives as ownership

Tier authority (`LocalPath` / `SharedPath` / `RuntimePath` discipline in
`RuntimeAuthority`) predates the gate and stays (ADR-046 §4). The
`principal:write_*` grant-string checks in `common/authority.rs` are
replaced by plain ownership comparison: a run belonging to principal A
may not write principal B's tiers; a crossing attempt fails closed and
emits a `Security` audit event. No grant language remains anywhere —
the boundary is *whose files these are*, not *which strings you hold*.

### D10 — `extension-api` folds away

`capabilities.rs`, `manifest.rs`, `tool_funnel.rs` (replaced by the
3-method seam), `hook_io.rs` (minus the compaction/inbox payloads with
live consumers), and `types.rs` (`ExtensionId`/`HookId`) die. The
genuine survivors — `default_*_dir` paths, completion/inbox contracts,
session/subagent types — fold into `peko-tools-core`, `peko-session`,
or root as their consumers dictate; `reserved_params.rs` moves beside
MCP (its only consumer). The crate is deleted and the 81-entry
`check_workspace_deps.py` forbidden-edge table is updated.

P6 places the live `ToolFunnel`/`EngineHooks` ports in `peko-engine`,
which consumes them; root implements them without an engine → root edge.
Async task statuses and paths live in `peko-tools-core`; completion,
inbox, session snapshots, and spawn cleanup policy live in `peko-session`.
Workspace observer payloads and catalog metadata live in root. Removing
the retired crate's 12 forbidden edges leaves 20 members and 69 rules.

## 3. Migration plan

Six phases, each a separate PR, each leaving the tree green. Standard
gate for every phase: `cargo fmt --all -- --check && cargo clippy
--all-targets -- -D warnings && cargo test --lib && python3
scripts/check_workspace_deps.py` (+ `scripts/check_module_boundaries.sh`
when `core/src/**` moves).

- **P1 — Re-home + sweep the inert framework.** Move `async_exec/` and
  `inbox.rs` out of `extensions/framework/` (D4). Delete
  `ExtensionStore`, `discovery.rs`, `extension_storage.rs`,
  `store_trait.rs`, `adapters/`, `services/config_service.rs`,
  `services/tool_execution.rs`. *Verification:* standard gate; no
  behavioral change (the deleted code has zero registered adapters).
- **P2 — Delete the capability gate (D1, D9).** Remove the evaluator,
  grant checks, catalog projection, grant threading through
  `HookInput`/`ToolContext`/IPC; `principal.toml` grants ignored with
  deprecation warning; `common/authority.rs` ownership rewrite.
  *Verification:* standard gate; fresh principal boots with the full
  wire catalog; cross-principal write attempt fails + audits.
- **P3 — Collapse the funnel (D2).** `ToolCatalog` + `ToolDispatcher`
  replace `ExtensionCore` as the execution path; `ToolFunnel` shrinks
  to 3 methods; prompt handlers converted to providers; the daemon-global
  `ExtensionCore` singleton is split and deleted. *Verification:*
  standard gate + integration tier (mock-LLM tool round-trips);
  `<runtime-context>` sections render identically (renderer tests).
- **P4 — Hook dispatcher survivor + exposure deletion (D3, D5).**
  `WorkspaceHookDispatcher` rewires `workspace_hooks.rs` /
  `command_handler.rs`; `HookRegistry`/`HookPoint` zoo/companion codegen
  deleted; `ToolExposure`/`__tool_search`/`scoring.rs` deleted.
  *Verification:* hook integration tests (PreToolUse observe-only fires,
  2 s soft-fail, `PromptSection` bind renders a tail section).
- **P5 — Packaging swap (D6, D7, D8).** Flat-manifest tar packager;
  OCI machinery + registry client + push/pull/search CLI + IPC removed;
  import-time executable-inventory audit event. *Verification:* the
  ADR-056 round-trip property still pinned — create → self-organize →
  export → remove → import ⇒ sessions present, cron rebound,
  `boot_state` verbatim, tooling intact; `tar -tf` inspectability;
  keyless-package → `create -s` guidance preserved.
- **P6 — Fold `extension-api` + doc sweep (D10).** Crate deleted;
  dep-graph table updated; `AGENTS.md`, `API_SURFACE.md`,
  `DATA_MODEL.md`, `PRINCIPAL_WORKSPACE.md`, `builtin-tools.md`,
  `config.example.toml` swept; CHANGELOG entries per landed phase.
  *Verification:* full CI including `lint-workspace`.

## 4. Consequences

**Positive:**

- **~15k LOC gross deletion, ~12k net** (inert framework ~3k; hook/funnel
  scaffolding ~4k; capability gate + threading ~2k; OCI + remote
  registry ~5k; exposure/tool-search ~0.9k; minus ~2k of slim
  replacements). The tool call path shortens from six layers (funnel →
  gate → hook registry → builtin handler → async router → tool) to two
  (dispatcher → tool).
- **One mental model.** The workspace is the truth; the runtime is
  tools + audit. No second surface to drift (the ADR-050 argument,
  completed). The docs already describe this state — the code finally
  matches.
- **The singleton is gone.** `ExtensionCore`'s five conflated
  responsibilities become named things: catalog, dispatcher, hook
  dispatcher, async runtime, prompt providers.
- **Packaging is inspectable.** `tar -tf snapshot.peko` and `cat
  manifest.toml` show everything; no OCI vocabulary to learn.
- **The runtime's network surface shrinks to the tunnel.** No registry
  client, no credentials for push, no remote manifest parsing.

**Negative:**

- **No per-principal tool restriction.** "This principal must not have
  Bash" is no longer expressible. Accepted: the wildcard default meant
  no principal was actually restricted, and operators who need isolation
  run separate runtimes. Recorded here so it is a decision, not an
  oversight.
- **Audit is now the *only* control.** ADR-046's deferred follow-ups
  gain priority: tamper-evident hash chain (§7.2) and the in-session
  drift watcher (§7.1). Tool-call audit must be emitted at the single
  `ToolDispatcher` point (today attribution is scattered) — part of P3.
- **Old snapshots are rejected.** OCI-manifest `.peko` files from
  before P5 do not import; pre-launch posture makes this acceptable
  (re-export from source).
- **P3 is the engineering risk.** Splitting the daemon-global
  `ExtensionCore` (~335 mentions) while every subsystem holds
  `Arc<ExtensionCore>` is the phase most likely to sprawl; P1/P2 exist
  partly to shrink its surface first.

## 5. References

- [PRINCIPAL_WORKSPACE.md](../PRINCIPAL_WORKSPACE.md) — the layout this
  ADR makes literally true.
- [ADR-046](ADR-046-trust-and-audit.md) — trust + audit; §7 follow-ups
  promoted by this ADR.
- [ADR-056](ADR-056-full-existence-principal-snapshot.md) — snapshot
  semantics retained; §4's tooling-import follow-up resolved by D8.
- [ADR-060](ADR-060-seed-as-the-definition-artifact.md) — seeds; the
  hub-side distribution that makes D7 possible.
- `peko-rs/core/src/extensions/` — the demolished subsystem
  (`framework/`, `builtin/`, `role/`, `skill/`, `mcp/`).
- `peko-rs/core/src/registry/` — packaging survivors (collect rules,
  wake/seed, cron rebind, `path_safety`) and the deleted OCI/remote
  machinery.
- `scripts/check_workspace_deps.py` — the forbidden-edge table updated
  in P6.
