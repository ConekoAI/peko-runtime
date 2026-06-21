# Refactor: runtime cleanup & CI optimization

This document summarises the architectural cleanup of `peko-runtime`
on the `refactor/runtime-cleanup-20260621` branch. It accompanies
`PLAN.md` (full roadmap) and the per-tier CI table in `AGENTS.md`.

## Scope

This PR delivers **Horizon A** of `PLAN.md` — a small, buildable set of
mechanical cleanups that do not require renaming any `pub` module that
ships CLI subcommands. Horizon B (the larger module-graph rework) is
deferred to follow-up PRs and listed at the bottom of `PLAN.md`.

## What changed

### Dead code removed

- `Cargo.toml[features].mcp = []` — empty Cargo feature with zero
  `cfg(feature = "mcp")` consumers anywhere in the tree, no workflow
  flag, no docs reference.
- `crate::types::agent::BootstrapFileConfig` — deprecated type alias
  with no callers; the canonical name is `SystemFileConfig`.
- `src/extension/types/async_types::AsyncTaskStatus` re-export —
  duplicated by the canonical re-export at
  `src/extension/types/mod.rs`. Updated `mod.rs` to re-export directly
  from `crate::extension::async_exec::executor::AsyncTaskStatus`.
- 19 orphan PowerShell scripts under `e2e_tests_archive/**/*.ps1` —
  no Rust test, Makefile target, or CI workflow invokes them. Test
  fixtures under `e2e_tests_archive/extensions/**` (`manifest.yaml`,
  `*.py`, `*.js`) are retained because `cli_extensions.rs` and
  `cli_compaction.rs` read them as fixture data.
- `tests/common/mock_configure::configure_url` — internal helper with
  no external callers; inlined into its sole user `configure_mock`.
- Stale commented-out `// pub mod hooks;` in `src/lib.rs`.
- Three stale doc comments referencing nonexistent
  `runtime/migration.rs` / `runtime::migration_v3` modules (in
  `src/auth/principal.rs`, `src/types/agent.rs`, and
  `src/common/services/config_authority/implementation.rs`).

### CI restructured

`.github/workflows/integration.yml` is now a six-tier pipeline with a
`changes` detector job using `dorny/paths-filter` to decide per-job
which diffs need which tier. Key wins:

- **Smoke tier** runs `cargo fmt --check && cargo clippy --all-targets
  -- -D warnings && cargo test --lib` on every PR that touches
  `src/**` or `tests/**`, finishing in < 6 min on warm cache.
- **Lint tier** runs `scripts/check_module_boundaries.sh` on every PR
  that touches `src/**`. Currently **advisory** (`continue-on-error`)
  while known pre-existing violations remain in
  `src/extension/adapters/validation.rs` and
  `src/extension/manager/mod.rs` — see PLAN.md Horizon B.
- **Integration tier** is now path-aware: pure refactors (only
  `src/**` changed) skip the Docker stack entirely. The job runs only
  when `tests/**`, `docker/**`, `Dockerfile*`/`docker-compose*.yml`,
  or the workflow file itself has changed — or on schedule / manual.
- **Windows tier** gets an extra gate on Windows-specific code paths
  (`src/common/process/**`, `src/ipc/pipe_security.rs`,
  `tests/common/cli.rs`) so a Linux-only refactor can't accidentally
  trigger the expensive Windows runner.
- All cache steps now use `Swatinem/rust-cache@v2` (previously the
  integration job used `actions/cache@v4`, which doesn't expand
  `~/.cargo/...` on Windows and produces lower hit rates).
- Doc-only PRs (only `*.md`, `PLAN.md`, `CHANGES.md`, `docs/**`) do
  **not** trigger CI at all thanks to the workflow-level `paths` filter.

### Documentation aligned

- `PLAN.md` — new file. Full refactor roadmap with the 9-domain target
  layout, file move/rename map, circular dependency inventory, public
  API changes, CI tier design, and risk areas. Horizon B backlog lives
  at the bottom.
- `AGENTS.md` — added a "CI tiers" subsection with the trigger table
  for each tier and an updated local quick-feedback loop showing
  the `make`-based commands CI uses.
- `CHANGES.md` — this file.

## What did not change (yet)

These are documented in `PLAN.md` §3 (Horizon B) and tracked as
follow-up work:

- The 21 top-level `src/` directories are still in their pre-refactor
  positions. The 9-domain target layout is described but not enacted.
- The five strong circular dependencies identified by exploration
  (`portable ↔ identity`, `tools ↔ tunnel`,
  `extension::types ↔ engine`, `tools::core ↔ extension::types`,
  `tunnel → tools → agent → session → engine`) are not yet broken.
- `SubjectType` enum, `principal_from_string*` helpers, `Peer` type
  alias, and `Principal::{id, peer_type, is_user, is_agent}` compat
  methods are deprecated but still in place.
- `tests/principal_back_compat.rs` is retained as the safety net for
  the legacy IPC wire format.
- `src/extension/adapters/validation.rs` and
  `src/extension/manager/mod.rs` still import concrete extension types
  from the framework (Issue 015). Lint job is advisory until these
  violations clear.
- 34 `#[allow(dead_code)]` annotations across the codebase. Most
  pinpoint specific deletion candidates to evaluate in Horizon B;
  removing them in bulk is the right time only after each one has
  been audited.

## Verification

- `cargo check --lib --tests` passes locally.
- `cargo test --lib` passes: 1527 tests, 0 failures.
- `cargo clippy --all-targets` produces only pre-existing warnings
  (none new from this PR — the files I touched are clean).
- YAML schema for `.github/workflows/integration.yml` validates via
  `python3 -c "import yaml; yaml.safe_load(open(...).read())"`.
  `actionlint` is not installed locally but the structure follows
  the same template as the previous workflow with the addition of
  the standard `dorny/paths-filter@v3` action.
- `bash scripts/check_module_boundaries.sh` runs and reports the
  expected pre-existing violations (advisory, not blocking).
