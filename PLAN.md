# `peko-runtime` Architectural Cleanup & CI Refactor — Plan

> **Status:** Draft. Authored by automated exploration on 2026-06-21.
> **Branch:** `refactor/runtime-cleanup-20260621`
> **Goal:** Establish clean domain boundaries, remove legacy/backward-compat code, and shrink CI to a fast feedback loop. We are at the dev stage and may break the public API.

---

## 1. Approach

This plan is split into **two horizons**:

- **Horizon A (this PR / draft):** Safe, mechanical, low-risk changes that ship today — dead-code deletion, CI restructuring, documentation alignment, and unused-script wiring. Each commit is independently buildable; `cargo check`, `cargo clippy -- -D warnings`, and `cargo test --lib` stay green throughout.
- **Horizon B (follow-up PRs):** Larger structural moves that are described in §6 but **deferred** to keep the initial diff small, reviewable, and reversible. Each Horizon B item will become its own task list once Horizon A is merged.

The full target 9-domain layout is laid out in §4. The Horizon A scope is the safer subset of changes that does not require renaming any module that is `pub`-re-exported or that ships CLI subcommands.

---

## 2. Inventory Snapshot (from exploration)

These come from three parallel reads of the tree on 2026-06-21.

### 2.1 Top-level `src/` directories (21 total)

`agent`, `auth`, `commands`, `common`, `compaction`, `cron`, `daemon`, `engine`, `extension`, `extensions`, `identity`, `ipc`, `observability`, `portable`, `prompt`, `providers`, `registry`, `runtime`, `session`, `team`, `tools`, `tunnel`, `types` — plus `lib.rs` / `main.rs`.

### 2.2 Public surface (`src/lib.rs:1-220`)

19 `pub mod`s + 1 `pub(crate) mod cron` + 1 `pub(crate) mod prompt` + 1 `pub(crate) mod compaction`. The crate root re-exports `Agent`, `AgenticEvent`, `LifecyclePhase`, and `VERSION`.

### 2.3 Confirmed dead code (Horizon A targets)

| Path / Symbol | Justification |
|---|---|
| `Cargo.toml:122` — `mcp = []` feature | Zero `cfg(feature = "mcp")` references anywhere in the tree; no workflow sets `--features mcp`. |
| `src/types/agent.rs:229-230` — `pub type BootstrapFileConfig = SystemFileConfig;` | Zero callers; comment marks it deprecated. |
| `e2e_tests_archive/**/*.ps1` (15 PowerShell scripts) and `reset.ps1` | No `cargo test`, Makefile, or CI workflow invokes them; only YAML/Python/JS fixtures under `e2e_tests_archive/extensions/**` are reached (by `cli_extensions.rs`). |
| `tests/common/mock_configure.rs` — `configure_url` helper | Internal-only; only consumed inside the same file by `configure_mock`. |
| `src/extension/types/async_types.rs:48` — `pub use crate::extension::async_exec::executor::AsyncTaskStatus;` | Duplicated by `extension/types/mod.rs:7`. |
| `src/lib.rs:196` — commented-out `// pub mod hooks;` | Stale comment referencing removed Issue 001 module. |
| Stale references to nonexistent `runtime/migration.rs` (3 places) | `src/auth/principal.rs:49`, `src/types/agent.rs:133`, `src/common/services/config_authority/implementation.rs:257` — dead doc comments. |

### 2.4 Legacy / backward-compat code (Horizon B targets)

Documented for future work, **not removed in this PR**:

- `src/auth/ownership.rs:46-103` — `SubjectType` enum + `principal_from_wire` (both `#[deprecated]`). Bounded by ADR-039 wire-field collapse.
- `src/auth/principal.rs:258-306` — `principal_from_string` / `principal_from_string_with_default_user` helpers. Bounded by `AgentConfig`/`TeamMetadata` direct-`owner: Principal` migration.
- `src/session/types.rs:19-21` — `pub type Peer = Principal;` + `PeerUser/PeerAgent` re-exports; compat methods on `Principal` (`id`, `peer_type`, `is_user`, `is_agent`).
- `src/identity/storage.rs:42-51, 207-216, 269-333, 349, 542-579` — `LegacyStoredIdentity` + `migrate_legacy`.
- `src/extension/types/manifest.rs:92-112` — `migrate_legacy_dependencies`.
- `src/commands/credential.rs:62-163` — `peko credential migrate` (one-shot OS-keychain → vault migration).
- `src/tools/builtin/messaging/a2a_send.rs:124-156` — `A2aSendArgs::target_agent` legacy field.
- `tests/principal_back_compat.rs` — regression-prevention suite for the above. Will be deleted when `SubjectType` and the legacy parser helpers are gone.

### 2.5 Circular dependency inventory (Horizon B targets)

Strong cycles identified by exploration:

1. **`portable ↔ identity`** via `src/identity/keychain.rs:13` (`use crate::portable::crypto`) ↔ `src/portable/{packager,unpackager,team_packager,signature}.rs` (`use crate::identity::*`).
2. **`tools ↔ tunnel`** via `src/tools/builtin/messaging/a2a_send.rs:42-48` ↔ `src/tunnel/dispatcher.rs:30`.
3. **`extension::types ↔ engine`** via `src/extension/types/tool_exec.rs:16` (`use crate::engine::AgenticEvent`) ↔ `src/engine/{compaction_orchestrator,tool_executor}.rs` (use `extension::core`).
4. **`tools::core ↔ extension::types`** via `src/tools/core/traits.rs:3-4` ↔ `src/extension/core/async_bridge.rs` (re-implements `Tool`).
5. **`tunnel → tools → agent → session → engine`** indirect cycle via `tunnel::dispatcher::use crate::tools::builtin::messaging::a2a_send`, `tools::builtin::messaging::a2a_send::use crate::agent::stateless_service`, `agent::stateless_service::use crate::session::manager::SessionManager`.
6. **`commands::team → portable + registry + extension`** leak. CLI doing portable-layer orchestration that should live in `TeamService`.

### 2.6 CI inventory (Horizon A scope)

`.github/workflows/integration.yml` runs 4 jobs:
- `unit-linux` — `cargo test --lib`, ~3 min warm.
- `unit-windows` — `cargo test --lib` on Windows runner, gated by `[windows]` keyword/branch. ~5 min cold.
- `integration` — Docker compose (PekoHub + mock-LLM), 23 integration test binaries serially, ~10-15 min.
- `integration-llm` — Same plus real `MINIMAX_API_KEY` / `KIMI_API_KEY`. Gated.

Issues: no fast smoke tier, integration always runs on PR (even for doc-only changes), no path filtering beyond `src/**`+`tests/**`+`Makefile`+`.github/docker/**`+workflow itself.

### 2.7 Scripts (`scripts/`) not wired into CI

Four local-only scripts (0 of 4 referenced from any workflow):
- `check_module_boundaries.sh` — enforces Issue 015 boundary rules (e.g. `src/extension/` must not import `src/extensions/`).
- `check_module_boundaries.ps1` — Windows variant with `-Strict` mode.
- `check_service_layer.ps1` — enforces Issue 020 service-layer rules (commands must not import low-level persistence).
- `code_quality_check.sh` — advisory clippy / fmt / dead-code reporter.

---

## 3. Public API Changes

### Horizon A (this PR)

- **Removals** (all of these are unused):
  - `Cargo.toml[features].mcp = []` — delete the empty feature.
  - `crate::types::agent::BootstrapFileConfig` (deprecated alias).
  - `crate::extension::types::async_types::AsyncTaskStatus` (duplicate re-export).
  - `pub fn configure_url` from `tests/common/mock_configure.rs` (test-only).
  - PowerShell scripts under `e2e_tests_archive/`.

- **Additions**: none.

- **Renames**: none.

### Horizon B (documented for follow-up)

- Drop `SubjectType` enum from `crate::auth::ownership` and the corresponding wire fields on `RequestPacket`.
- Drop `Peer` type alias and `Principal::{id, peer_type, is_user, is_agent}` compat methods.
- Drop `principal_from_string*` helpers; force `owner: Principal` on `AgentConfig` / `TeamMetadata`.
- Drop `LegacyStoredIdentity`, `migrate_legacy`, `migrate_legacy_dependencies`, and `peko credential migrate`.
- Drop `A2aSendArgs::target_agent` and the LLM-facing tool description that references it.

---

## 4. Target Module Layout (full 9-domain map)

Final tree after **both** horizons:

```
src/
├── common/                 # Common / infrastructure (paths, vault, crypto, kv, secret_store, time, types, windows)
├── identity/               # Identity & auth (DIDs, keys, keychain, storage, resolver, runtime, crypto)
├── auth/                   # Identity & auth (api_key, caller, config, jwt, ownership, permissions, principal, rate_limit)
├── agents/                 # Agents & team (renamed from agent/, absorbs runtime/tool_runtime + prompt/ + team/config)
├── engine/                 # Core runtime (agentic_loop, events, state, stream_*, tool_*, compaction/, app_state)
├── session/                # Core runtime (jsonl, manager, key, lock, message, overlay, spawn, …)
├── cron/                   # Core runtime (engine absorbed from daemon)
├── daemon/                 # Core runtime (thin: process lifecycle + background_runtime lifted to extensions)
├── ipc/                    # Core runtime (client, server, packet, stream)
├── extensions/             # Extensions & tools (framework: core, adapters, manager, types, transport, services, protocols, scaffold, async_exec)
├── extensions/impls/       # (renamed from extensions/) MCP, Gateway, Skill, Builtin, General, Universal
├── tools/                  # Extensions & tools (core trait, builtin/, skill/, factory)
├── providers/              # Providers (catalog, resolver, adapters, transport, mock, templates, types, registry)
├── registry/               # Registry / packaging (remote client + local store + .agent/.team packaging; absorbs portable/)
├── tunnel/                 # Tunnel / network (client, dispatcher, hub_directory, a2a_*, known_runtimes, a2a_send_tool)
├── commands/               # CLI / commands (thin: only argument parsing + service delegation)
├── observability/          # Observability (audit, metrics, tracer, performance, async_tool_metrics, a2a_audit)
├── compaction/             # (deleted; merged into session/)
├── prompt/                 # (deleted; merged into agents/)
├── runtime/                # (deleted; split into identity/, engine/, tunnel/)
├── team/                   # (deleted; merged into agents/team_config.rs)
├── portable/               # (deleted; merged into registry/packaging/)
└── types/                  # (deleted; merged into common/types/)
```

The rename `agent → agents`, `engine` (kept), and the consolidation of `portable/`+`registry/` into a single `registry/` with sub-modules are the headline moves in Horizon B.

---

## 5. Files / Modules to Move (Horizon B backlog)

| From | To | Notes |
|---|---|---|
| `src/extension/types/tool_exec.rs` | `src/extensions/framework/types/` (or split into a smaller event type that lives in the framework) | Drop the `use crate::engine::AgenticEvent` — break cycle 3. |
| `src/identity/keychain.rs` (`use crate::portable::crypto`) | `src/identity/crypto.rs` (or `src/common/crypto.rs`) | Move encryption helpers here to break cycle 1. |
| `src/tools/builtin/messaging/a2a_send.rs` | `src/tunnel/a2a_send_tool.rs` | Tool depends on tunnel anyway — invert the cycle 2. |
| `src/runtime/{identity,metadata,registry,tool_runtime}.rs` | split into `src/identity/runtime.rs`, `src/engine/tool_runtime.rs`, `src/tunnel/known_runtimes.rs` | `src/runtime/` becomes empty → delete. |
| `src/compaction/` | merge into `src/session/compaction/` | Domain unification. |
| `src/prompt/` | merge into `src/agents/prompt.rs` | Domain unification. |
| `src/team/config.rs` | merge into `src/agents/team_config.rs` + `src/common/types/team.rs` | Single source of truth. |
| `src/portable/` (renamed `packaging/`) + `src/registry/` | unified `src/registry/` (with `packaging/` + `local.rs`) | Resolve duplicate "registry" naming. |
| `src/commands/team.rs` (heavy) | keep in `src/commands/team.rs` but **slimmed** — only argument parsing + `TeamService` calls | Break leak 6. |
| `src/daemon/state.rs::AppState` | `src/engine/app_state.rs` (wiring lifted out of daemon) | Reduce `daemon` to process lifecycle. |
| `src/daemon/cron_engine/` | `src/cron/engine.rs` | Domain unification. |
| `src/tunnel/a2a_audit.rs` | `src/observability/a2a_audit.rs` | Domain unification. |
| `src/types/agent.rs` (`AgentConfig`) | `src/agents/agent_config.rs` | Single source of truth (engine has `AgentState`). |

---

## 6. Risk Areas

### High risk
- **Cycle breaks** (cycles 1-5) require renaming types or introducing trait objects. Each needs its own task list with buildable steps.
- **Wire-format changes** (dropping `SubjectType` fields from `RequestPacket`) require coordinated updates across `src/ipc/packet.rs`, `src/auth/ownership.rs`, `src/session/*`, and CLI serialization. The `tests/principal_back_compat.rs` suite is the safety net.

### Medium risk
- **Unifying `portable` + `registry`** doubles the number of imports touched (`src/registry/*` already imports from `portable::*`).
- **`commands::team.rs` slim-down** requires `TeamService` to be feature-complete (today `TeamService::update_owner` already calls the same helpers we want to centralize, so the work is mostly deletion plus delegating one or two more cases).
- **`AppState` extraction** touches `src/daemon/state.rs` and every test that constructs `AppState` directly (search `tests/` for `AppState::new`).

### Low risk (Horizon A)
- Dead-code deletion (no callers).
- CI YAML edits.
- Doc edits.
- Wiring `scripts/check_module_boundaries.sh` into a new `lint` job.

---

## 7. CI Tier Changes (Horizon A)

### New tier: `smoke` — runs on every PR

`cargo fmt --check`, `cargo check --all-targets`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib --no-fail-fast`. Target: **< 8 min warm**.

Path filter: triggers on any change to `src/**`, `tests/**`, `Cargo.*`, `Makefile`, `clippy.toml`, `.github/docker/**`, `scripts/check_*.sh`, `scripts/check_*.ps1`, `.github/workflows/integration.yml`.

### Existing `unit-linux` job

Unchanged; continues to run on every PR after `smoke`.

### Existing `unit-windows` job

Keep the existing keyword/branch/schedule gate; additionally gate on paths that touch Windows-specific code (`src/common/process/job_object.rs`, `src/ipc/pipe_security.rs`, `src/common/process/*` on Windows, etc.).

### Existing `integration` job

Keep, but **add a `changes` job** upstream that detects whether the diff actually requires the Docker stack:
- Run if any of: `src/**`, `tests/**` (excluding `principal_back_compat.rs`, `cli_agent_signature.rs`, `extension_packaging.rs`, `team_integration.rs` which are pure-Rust), `Cargo.*`, `Makefile`, `.github/docker/**`, `.github/workflows/integration.yml`.
- Skip if the PR is docs-only (`*.md`, `PLAN.md`, `CHANGES.md`, `docs/**`).

### Existing `integration-llm` job

Unchanged; stays opt-in (`[llm]` keyword / nightly / manual). No path filtering change.

### New `lint` job

Runs `scripts/check_module_boundaries.sh` on every PR that touches `src/**`. Fails the PR if any Issue-015 boundary rule is violated.

### Caching

Use `Swatinem/rust-cache@v2` (already in `unit-linux`) consistently across all jobs; replace the `actions/cache@v4` in `integration` (line 113).

### Cost / timing targets after Horizon A

| Tier | Trigger | Wall-clock (warm cache) |
|---|---|---|
| `smoke` (new) | every PR | < 8 min |
| `lint` (new) | every PR with `src/**` change | < 1 min |
| `unit-linux` | every PR | ~3 min |
| `unit-windows` | `[windows]` / nightly / manual | ~5 min |
| `integration` | PR touching integration-relevant paths | ~10-15 min |
| `integration-llm` | `[llm]` / nightly / manual | ~5 min extra |

For pure doc-only or refactor PRs: only `smoke` + `lint` + `unit-linux` run.

---

## 8. Execution Order

Horizon A is structured as six small commits:

1. **`chore: delete dead code (mcp feature, BootstrapFileConfig, orphan ps1 scripts)`** — touches only `Cargo.toml`, `src/types/agent.rs`, `e2e_tests_archive/**/*.ps1`, `tests/common/mock_configure.rs`, `src/extension/types/async_types.rs`. `cargo check` green.
2. **`docs: align AGENTS.md and README.md with current commands`** — doc-only.
3. **`ci: add smoke tier and path-aware integration gate`** — `.github/workflows/integration.yml` only.
4. **`ci: add lint job running scripts/check_module_boundaries.sh`** — `.github/workflows/integration.yml` + workflow permissions.
5. **`chore: add CHANGES.md describing the refactor roadmap`** — new file.
6. **`docs: PLAN.md`** — new file (this document).

Each commit ends with `cargo check && cargo fmt --check && cargo clippy -- -D warnings && cargo test --lib` green.

---

## 9. Definition of Done

### Horizon A (this PR)

- [x] Branch `refactor/runtime-cleanup-20260621` created from `master`.
- [ ] All Horizon A commits pushed.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib` all green.
- [ ] `.github/workflows/integration.yml` adds the `smoke` and `lint` jobs; documents each tier in the file.
- [ ] `AGENTS.md` reflects current CI commands and the dead-code removal.
- [ ] `CHANGES.md` summarises the cleanup and points at the Horizon B backlog.
- [ ] Draft PR opened against `main` (not merged).

### Horizon B (per-item follow-up PRs)

Each major module move or cycle break is its own branch + PR:

- [ ] Break cycle 1 (`portable ↔ identity`).
- [ ] Break cycle 3 (`extension::types ↔ engine`).
- [ ] Break cycle 2 (`tools ↔ tunnel`).
- [ ] Drop `SubjectType` + `principal_from_string*` + `Peer` alias + `Principal` compat methods.
- [ ] Delete `tests/principal_back_compat.rs`.
- [ ] Unify `portable` + `registry` into a single `src/registry/` tree.
- [ ] Slim `commands::team.rs` to call only `TeamService`.
- [ ] Lift `daemon::state::AppState` into `engine::app_state`.
- [ ] Merge `compaction/`, `prompt/`, `team/`, `runtime/` into their target domains.
- [ ] Reorganize tools/extensions: move `builtin/` and `skill/` to live next to the framework that owns them.
