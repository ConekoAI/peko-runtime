# `peko-runtime` Architectural Cleanup & CI Refactor — Plan

> **Status:** In progress.  
> **Branch:** `refactor/clippy-cleanup-rust196`  
> **PR:** #62  
> **Goal:** Establish clean domain boundaries, remove legacy/backward-compat code, and shrink CI to a fast feedback loop. We are at the dev stage and may break the public API.  

---

## 1. Approach

This plan originally split work into two horizons:

- **Horizon A:** Safe, mechanical, low-risk changes — dead-code deletion, CI restructuring, documentation alignment, and unused-script wiring.
- **Horizon B:** Larger structural moves that were initially deferred to keep the diff reviewable.

**PR #62 enacts both Horizon A and the bulk of Horizon B in a single branch.** The combined refactor was chosen to avoid a long chain of stacked module renames (`agent → agents`, `extension → extensions/framework`, `portable → registry/packaging`, etc.) that each conflict with the next. Horizon A items remain included; anything not yet done is listed in §9 as the remaining backlog.

The full target 9-domain layout is laid out in §4. The moves already performed are documented in §5.

---

## 2. Inventory Snapshot

### 2.1 Top-level `src/` directories after the refactor (16 total)

`agents`, `auth`, `commands`, `common`, `cron`, `daemon`, `engine`, `extensions`, `identity`, `ipc`, `observability`, `providers`, `registry`, `session`, `tools`, `tunnel` — plus `lib.rs` / `main.rs`.

Deleted/merged roots:
- `agent/` → `agents/`
- `compaction/` → `session/compaction/`
- `extension/` → `extensions/framework/`
- `portable/` → `registry/packaging/`
- `prompt/` → `agents/prompt/`
- `runtime/` → split into `identity/runtime.rs`, `engine/tool_runtime.rs`, `tunnel/known_runtimes.rs`
- `team/` → `common/types/team.rs` + `registry/packaging/`
- `types/` → `common/types/`

### 2.2 Public surface (`src/lib.rs`)

16 `pub mod` domain roots + `pub(crate) mod cron` + `pub mod daemon` (under `test-utils`). The crate root re-exports `Agent`, `AgenticEvent`, `LifecyclePhase`, and `VERSION`.

### 2.3 Dead code removed in PR #62

| Path / Symbol | Justification |
|---|---|
| `Cargo.toml:122` — `mcp = []` feature | Zero `cfg(feature = "mcp")` references anywhere; no workflow sets `--features mcp`. |
| `src/types/agent.rs:229-230` — `pub type BootstrapFileConfig = SystemFileConfig;` | Zero callers; comment marked it deprecated. |
| `e2e_tests_archive/**/*.ps1` (19 PowerShell scripts) and `reset.ps1` | No `cargo test`, Makefile, or CI workflow invokes them; only YAML/Python/JS fixtures under `e2e_tests_archive/extensions/**` are reached. |
| `tests/common/mock_configure.rs` — `configure_url` helper | Internal-only; consumed inside the same file. |
| `src/lib.rs:196` — commented-out `// pub mod hooks;` | Stale comment referencing removed Issue 001 module. |
| Stale references to nonexistent `runtime/migration.rs` | `src/auth/principal.rs`, `src/common/services/config_authority/implementation.rs` — dead doc comments. |
| `SubjectType` enum + `principal_from_wire` | Deprecated; removed in issue #30. |
| `Peer` type alias + `Principal::{id, peer_type, is_user, is_agent}` | Deprecated; removed in issues #25/#30. |
| `tests/principal_back_compat.rs` | Legacy wire-format test; removed alongside `SubjectType`. |
| `tests/scenarios/s6_revoke_principal_collapse_e2e.rs` | Never wired in `Cargo.toml`; coverage moved to unit tests in `src/ipc/packet.rs`. |

### 2.4 Legacy / backward-compat code still present (remaining backlog)

| Path / Symbol | Justification |
|---|---|
| `src/identity/storage.rs` — `LegacyStoredIdentity` + `migrate_legacy` | Pending coordinated removal. |
| `src/extensions/framework/types/manifest.rs` — `migrate_legacy_dependencies` | Pending coordinated removal. |
| `src/commands/credential.rs` — `peko credential migrate` | One-shot OS-keychain → vault migration; remove once migration window closes. |
| `src/tunnel/a2a_send_tool.rs` — `A2aSendArgs::target_agent` legacy field | Pending removal from tool schema. |
| `src/daemon/state.rs` — `AppState` | Still the daemon composition root; lift to a dedicated composition domain or keep in `daemon`. |

### 2.5 Circular dependency status

Strong cycles identified by exploration:

1. **`portable ↔ identity`** — **Broken** by creating `src/identity/crypto.rs` and deleting `src/portable/`.
2. **`tools ↔ tunnel`** — **Broken** by moving `a2a_send` from `tools/builtin/messaging/` to `tunnel/a2a_send_tool.rs`.
3. **`extension::types ↔ engine`** — **Broken** by moving `tool_exec.rs` into `extensions/framework/types/` and removing the `crate::engine::AgenticEvent` import.
4. **`tools::core ↔ extension::types`** — **Broken**; execution primitives now live in `extensions::framework::types` and are re-exported by `tools::core`.
5. **`tunnel → tools → agent → session → engine`** indirect cycle — **Broken** as a side effect of the `a2a_send` move and `agent → agents` restructure.
6. **`commands::team → portable + registry + extension`** leak — **Partially addressed** by deleting `portable/` and moving packaging into `registry/`, but `commands::team` still orchestrates registry/packaging directly. Remaining work.

### 2.6 CI inventory

`.github/workflows/integration.yml` runs a path-aware pipeline:

- `smoke` — fmt (advisory), clippy `-D warnings`, `cargo test --lib`.
- `lint` — `scripts/check_module_boundaries.sh` (hard gate).
- `unit-linux` — `cargo test --lib`.
- `unit-windows` — `cargo test --lib` on Windows runner, gated by Windows paths/`[windows]`/schedule/manual.
- `integration` — Docker compose (PekoHub + mock-LLM), gated by test/docker/workflow changes.
- `integration-llm` — Real LLM keys, gated by `[llm]`/schedule/manual.

### 2.7 Scripts (`scripts/`)

- `check_module_boundaries.sh` — wired into CI `lint` job. Currently checks obsolete `src/extension/` paths; needs update to `src/extensions/framework/`.
- `check_module_boundaries.ps1` — Windows variant; same update needed.
- `check_service_layer.ps1` — not wired into CI.
- `code_quality_check.sh` — not wired into CI.

---

## 3. Public API Changes

### Already landed in PR #62

- **Removals:**
  - `Cargo.toml[features].mcp = []` — empty feature.
  - `crate::types::agent::BootstrapFileConfig` — deprecated alias.
  - `crate::extension::types::async_types::AsyncTaskStatus` — duplicate re-export.
  - `pub fn configure_url` from `tests/common/mock_configure.rs` — test-only helper.
  - PowerShell scripts under `e2e_tests_archive/`.
  - `crate::auth::ownership::SubjectType` enum.
  - `crate::auth::ownership::principal_from_wire`.
  - `crate::auth::principal::Peer` type alias.
  - `Principal::{id, peer_type, is_user, is_agent}` compat methods.
  - Public modules `agent`, `extension`, `portable`, `runtime`, `team`, `types` from `src/lib.rs`.

- **Renames:**
  - `crate::agent` → `crate::agents`.
  - `crate::extension` → `crate::extensions::framework`.
  - `crate::portable` → `crate::registry::packaging`.
  - `crate::types` → `crate::common::types`.
  - `crate::compaction` → `crate::session::compaction`.
  - `crate::prompt` → `crate::agents::prompt`.

### Remaining backlog

- Drop `LegacyStoredIdentity`, `migrate_legacy`, `migrate_legacy_dependencies`, `peko credential migrate`.
- Drop `A2aSendArgs::target_agent`.
- Decide final home for `AppState` and move if appropriate.

---

## 4. Target Module Layout (current state)

```
src/
├── common/                 # Common / infrastructure (services, vault, crypto, types, paths, process)
├── identity/               # Identity & auth (DIDs, keys, keychain, storage, resolver, runtime, crypto)
├── auth/                   # Identity & auth (api_key, caller, config, jwt, ownership, permissions, principal, rate_limit)
├── agents/                 # Agents & team (agent, agent_config, prompt, stateless manager/service, subagent logic)
├── engine/                 # Core runtime (agentic_loop, events, state, stream_*, tool_*, compaction_orchestrator, tool_runtime)
├── session/                # Core runtime (jsonl, manager, key, lock, message, overlay, spawn, compaction/)
├── cron/                   # Core runtime (scheduling)
├── daemon/                 # Core runtime (HTTP daemon, background_runtime, cron_engine, AppState composition root)
├── ipc/                    # Core runtime (client, server, packet, stream)
├── extensions/             # Extensions & tools
│   ├── framework/          # Generic extension framework (core, adapters, manager, types, transport, services, protocols, async_exec, scaffold)
│   ├── builtin/            # Built-in tool adapter
│   ├── gateway/            # Gateway adapter
│   ├── general/            # General extension adapter
│   ├── mcp/                # MCP adapter
│   ├── skill/              # Skill adapter
│   └── universal/          # Universal tool adapter
├── tools/                  # Tool trait surface and built-in tool implementations
├── providers/              # LLM provider integrations
├── registry/               # Registry / packaging (remote client + local packaging)
├── tunnel/                 # Tunnel / network (client, dispatcher, hub_directory, a2a_*, known_runtimes, a2a_send_tool)
├── commands/               # CLI / commands (argument parsing + service delegation)
├── observability/          # Observability (audit, metrics, tracer, performance, async_tool_metrics)
└── lib.rs / main.rs
```

---

## 5. Files / Modules Moved in PR #62

| From | To | Notes |
|---|---|---|
| `src/agent/` | `src/agents/` | Renamed; absorbed `prompt/`. |
| `src/prompt/` | `src/agents/prompt/` | Domain unification. |
| `src/types/agent.rs` (`AgentConfig`) | `src/agents/agent_config.rs` | Single source of truth. |
| `src/types/` | `src/common/types/` | Merged. |
| `src/compaction/` | `src/session/compaction/` | Domain unification. |
| `src/team/config.rs` | `src/common/types/team.rs` + `src/registry/packaging/` | Team metadata + packaging split. |
| `src/portable/` | `src/registry/packaging/` | Unified with registry. |
| `src/registry.rs` | `src/registry/mod.rs` + `src/registry/agent_registry.rs` | Registry surface reorganised. |
| `src/extension/` | `src/extensions/framework/` | Framework moved under plural `extensions/`. |
| `src/extensions/builtin/adapter.rs` validation logic | `src/extensions/validation.rs` | Break Issue-015 boundary leak. |
| `src/runtime/identity.rs` | `src/identity/runtime.rs` | Runtime identity. |
| `src/runtime/metadata.rs` | `src/identity/runtime_metadata.rs` | Runtime metadata. |
| `src/runtime/registry.rs` | `src/tunnel/known_runtimes.rs` | Known runtime registry. |
| `src/runtime/tool_runtime.rs` | `src/engine/tool_runtime.rs` | Tool runtime. |
| `src/tools/builtin/messaging/a2a_send.rs` | `src/tunnel/a2a_send_tool.rs` | Break `tools ↔ tunnel` cycle. |
| `src/portable/crypto.rs` | `src/identity/crypto.rs` | Break `portable ↔ identity` cycle. |
| `src/commands/team.rs` | `src/commands/team/mod.rs` + `src/commands/team/render.rs` | Structural split; further slim-down is backlog. |

---

## 6. Risk Areas

### High risk

- **Wire-format changes** — Dropping `SubjectType` fields from `RequestPacket` and removing `tests/principal_back_compat.rs` means the legacy `(subject_id, subject_type)` pair is gone. Old CLI binaries or persisted grant data using that shape will not deserialize correctly. The protocol version should be bumped and the change documented.
- **Public API breakage** — Downstream crates using `pekobot::agent`, `pekobot::extension`, `pekobot::portable`, etc. will break. Provide migration guidance or re-export aliases if backward compatibility is required.

### Medium risk

- **`commands::team` slim-down** — Still embeds packaging/registry orchestration. Centralising in `TeamService` is remaining work.
- **`AppState` extraction** — Still in `daemon::state`. Moving it incorrectly could create a god module.
- **Formatting debt** — 149 files fail `cargo fmt --check`. The CI smoke tier keeps fmt advisory until a one-time sweep lands.

### Low risk

- Remaining dead-code deletion (`LegacyStoredIdentity`, credential migrate).
- Final boundary-script update to new paths.

---

## 7. CI Tier Changes (current state)

### `smoke` — runs on every PR that touches `src/**` or `tests/**`

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib --no-fail-fast`. Target: **< 6 min warm**.

`cargo fmt --check` is advisory until the one-time fmt sweep.

### `lint` — runs on every PR that touches `src/**`

`bash scripts/check_module_boundaries.sh`. Hard gate once the script is updated to new paths; currently passes against obsolete rules.

### `unit-linux` — unchanged

Continues to run on every PR after `smoke`.

### `unit-windows` — gated

Existing keyword/branch/schedule gate plus path filter on Windows-specific code.

### `integration` — path-aware

Runs only when `tests/**`, docker assets, workflow, `Dockerfile*`, or `docker-compose*.yml` change, or on schedule/manual.

### `integration-llm` — opt-in

Unchanged; gated by `[llm]` keyword/schedule/manual plus path outputs.

### Caching

`Swatinem/rust-cache@v2` used consistently across all jobs.

---

## 8. Execution Order

PR #62 was delivered as a series of incremental commits rather than the original six-commit Horizon A plan. Key commit themes:

1. **Dead-code deletion** — `mcp` feature, `BootstrapFileConfig`, orphan `.ps1` scripts.
2. **CI restructuring** — smoke tier, lint tier, path-aware integration gate.
3. **Boundary fix** — break Issue-015 leak, promote lint job to hard gate.
4. **Auth cleanup** — inline `principal_from_string`, drop `SubjectType`/`principal_from_wire`, drop `Peer`/`Principal` compat methods.
5. **Module renames** — `agent → agents`, `extension → extensions/framework`, `types → common/types`, `compaction → session/compaction`, `portable → registry/packaging`, `runtime` split, `team` deletion.
6. **Cycle breaks** — `a2a_send` → tunnel, `crypto` → identity, `tool_exec` → extensions/framework.
7. **Clippy debt sweep** — clean up pre-existing warnings so `-D warnings` passes.
8. **Documentation** — update `PLAN.md`, `CHANGES.md`, `AGENTS.md` and stale doc comments.

Each intermediate commit was kept buildable; `cargo check`, `cargo test --lib`, and finally `cargo clippy --all-targets -- -D warnings` are green at the branch tip.

---

## 9. Definition of Done

### PR #62

- [x] Branch `refactor/clippy-cleanup-rust196` created from `master`.
- [x] Horizon A items landed (dead-code removal, CI restructure, docs).
- [x] Horizon B module moves landed (`agent → agents`, `extension → extensions/framework`, `portable → registry/packaging`, `types → common/types`, `compaction → session/compaction`, `prompt → agents/prompt`, `runtime` split, `team` deletion).
- [x] Cycles 1–5 broken.
- [x] `SubjectType`, `principal_from_wire`, `Peer` alias, and `Principal` compat methods removed.
- [x] `cargo test --lib` green: 1522 tests pass.
- [x] `cargo clippy --all-targets -- -D warnings` green.
- [x] `.github/workflows/integration.yml` adds `smoke` and `lint` jobs; path-aware tiers documented.
- [x] `AGENTS.md` and `CHANGES.md` updated to reflect actual scope.
- [x] Stale doc comments in `src/auth/ownership.rs`, `src/auth/principal.rs`, `src/extensions/framework/mod.rs` updated.
- [ ] Open PR #62 against `master`.

### Remaining backlog (follow-up PRs)

- [ ] Update `scripts/check_module_boundaries.sh` / `.ps1` to enforce new `extensions/framework/` boundaries.
- [x] Slim `commands::team` to argument parsing + `TeamService`/`TeamManagementService`
  delegation. Push/pull orchestration moved into `TeamService`; `commands::team/mod.rs`
  is now a thin dispatcher.
- [ ] Decide final home for `AppState` and move if appropriate.
- [ ] Move `daemon/cron_engine/` to `cron/engine.rs`.
- [ ] Move `tunnel/a2a_audit.rs` to `observability/a2a_audit.rs`.
- [ ] Drop `LegacyStoredIdentity`, `migrate_legacy`, `migrate_legacy_dependencies`, `peko credential migrate`.
- [ ] Drop `A2aSendArgs::target_agent` legacy field.
- [ ] Run one-time `cargo fmt` sweep and promote fmt check to hard gate.
