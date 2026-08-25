# Peko Test Targets — see docs/integration/TESTING.md
#
# Four canonical targets:
#   test                  — fast unit tests, no Docker, no LLM
#   test-integration      — PR gate, Docker + PekoHub + mock LLM
#   test-integration-llm  — tier above + tests that need a real model
#   test-all              — everything
#
# Granular per-file targets are kept as slices of test-integration for
# change-isolated dev loops. All mock-tier targets unset MINIMAX_API_KEY
# so they cannot silently leak to the real provider.

.PHONY: help test test-integration test-integration-llm test-all \
        docker-build docker-up docker-down \
        test-lib test-subagent \
        test-pekohub test-tunnel test-tunnel-e2e test-packaging test-registry \
        test-cli-send test-cli-session test-cli-basics test-cli-cron \
        test-cli-subagent test-cli-tools test-cli-agent-signature \
        test-cli-providers \
        test-scenarios-s4 test-scenarios-s6 \
        test-mock-llm-sequence \
        ci

# All integration test crates (live in peko-rs/core/tests/*.rs and
# peko-rs/core/tests/scenarios/*.rs after Phase 0.Z-D moved tests/).
# Kept in sync with `cargo metadata` (targets of kind = ["test"]); the
# Principal migration dropped the cli_compaction / cli_a2a /
# s3_agent_registry_roundtrip suites, the parity branch added
# cli_agent_signature for issue #14 (manifest signature verification),
# and ADR-047 Phases 5d/5e retired the extension_packaging and
# cli_extensions suites along with the extension framework.
INTEGRATION_TESTS := pekohub_integration tunnel_integration \
                     packaging_integration registry_integration \
                     cli_send cli_basics cli_cron cli_subagent \
                     cli_tools cli_agent_signature \
                     cli_providers \
                     s4_publish_running_agent_with_permission \
                     s6_principal_grant_revoke_roundtrip \
                     mock_llm_sequence
CARGO_TEST_FLAGS  := $(addprefix --test ,$(INTEGRATION_TESTS))

# Inline lib tests gated by `--features test-utils` (F9.4 moved tunnel_e2e
# inline so `AppState` / `daemon::*` could narrow to `pub(crate)` without a
# top-level integration harness needing public visibility). These run
# inside the lib test target, not as their own `--test` bin, so they're
# passed as filter paths appended after `--`.
INTEGRATION_LIB_TESTS := daemon::e2e_tests::tunnel_e2e

# Default ports exposed by docker-compose.integration.yml; CI overrides
# these for in-container runs (e.g. PEKOHUB_URL=http://pekohub-test:3000).
#
# Uses `docker compose` (v2 plugin) rather than the standalone `docker-compose`
# (v1) binary, which is not present on GitHub-hosted Linux runners and is
# being deprecated by Docker Desktop.
PEKOHUB_URL  ?= http://localhost:3000
MOCK_LLM_URL ?= http://localhost:8080

help:
	@echo "Peko Test Targets (see docs/integration/TESTING.md)"
	@echo ""
	@echo "  test                      Fast unit tests (cargo test --lib, no Docker, no LLM)"
	@echo "  test-integration          PR gate: all tests/*.rs against PekoHub + mock LLM"
	@echo "  test-integration-llm      Tier above + tests that need a real model"
	@echo "  test-all                  Everything (unit + mock-LLM + real-LLM)"
	@echo ""
	@echo "  docker-build              Build pekohub-test and mock-llm images"
	@echo "  docker-up                 Start the test stack (pekohub + mock LLM)"
	@echo "  docker-down               Stop and remove the test stack"
	@echo ""
	@echo "  ci                        Layered run used in GitHub Actions"
	@echo ""
	@echo "  Granular slices of test-integration (one file at a time):"
	@echo "    test-pekohub / test-tunnel / test-tunnel-e2e"
	@echo "    test-packaging / test-registry / test-subagent"
	@echo "    test-cli-send / test-cli-session / test-cli-basics / test-cli-cron"
	@echo "    test-cli-subagent / test-cli-tools / test-cli-agent-signature"
	@echo "    test-cli-providers (real-LLM tier — needs MINIMAX_API_KEY; kimi skipped while KIMI_API_KEY is suspended)"
	@echo "    test-scenarios-s4 (Phase D — publish running agent behind tunnel, mock-LLM)"
	@echo "    test-mock-llm-sequence"

# ── Tier 0: Fast unit tests ──────────────────────────────────────────────

test:
	cargo test --lib

test-lib: test   ## deprecated alias for `test`

test-subagent:
	cargo test --lib subagent_integration

# ── Docker stack lifecycle ───────────────────────────────────────────────
# Images are built via `docker build` (not compose), so the context
# + dockerfile path semantics are clear and identical in local and
# CI layouts. Compose just orchestrates the pre-built images.
# - pekohub-test context is ../pekohub (sibling of peko-runtime).
# - mock-llm context + dockerfile are both inside peko-runtime.
# Both paths are relative to the Makefile's CWD, which is peko-runtime/.

docker-build:
	docker build -t peko/pekohub-test:latest \
	    -f .github/docker/pekohub-test/Dockerfile ../pekohub
	docker build -t peko/mock-llm:latest \
	    -f .github/docker/mock-llm/Dockerfile .github/docker/mock-llm

docker-up: docker-build
	docker compose -f peko-rs/core/tests/docker/docker-compose.integration.yml up -d

docker-down:
	docker compose -f peko-rs/core/tests/docker/docker-compose.integration.yml down -v

# ── Tier 1: PR gate — Docker + PekoHub + mock LLM ────────────────────────
# MINIMAX_API_KEY is unset so a leaking env doesn't silently switch the
# dual-mode tests to the real provider.
#
# --include-ignored runs BOTH the hub-gated #[ignore] tests AND the
# always-on pure-Rust tests in cli_basics (6). Plain --ignored would
# skip those offline tests entirely.

test-integration: docker-up
	# Phase 0.Z-B: `peko` bin lives in the `peko-cli` satellite. Cargo
	# doesn't auto-build it for peko's integration tests because peko
	# can't list peko-cli as a dev-dep (circular — peko-cli depends on
	# peko_core). Pre-build here so peko-rs/core/tests/common/cli.rs's
	# CARGO_BIN_EXE_peko fallback resolves to a real binary.
	cargo build -p peko-cli --bin peko
	# Phase 12 (PR #264): `peko-daemon` was lifted into
	# peko-rs/peko-daemon/. cli_cron (and any test that spawns `peko
	# daemon start --foreground`) resolves the daemon via
	# `DaemonProcessService::peko_daemon_binary()`, which looks for a
	# `peko-daemon` sibling of the CLI binary. Without this pre-build,
	# the rust-cache restore of an older `target/debug/peko-daemon` is
	# the only thing keeping the daemon around — and that restore is
	# invalidated whenever Cargo.lock changes (e.g. cargo-machete dead
	# deps removal), which silently breaks every test that uses
	# CronDaemonGuard until the cache catches up.
	cargo build -p peko-daemon --bin peko-daemon
	@env -u MINIMAX_API_KEY \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test $(CARGO_TEST_FLAGS) -- --include-ignored
	@env -u MINIMAX_API_KEY \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --lib --features test-utils $(INTEGRATION_LIB_TESTS) -- --include-ignored

# ── Tier 2: nightly + [llm] commit tag — adds real-LLM tests ─────────────
# MOCK_LLM_URL is unset so the dual-mode rule in the inline
# `daemon::e2e_tests::tunnel_e2e` (`peko-rs/core/src/daemon/e2e_tests/tunnel_e2e.rs`,
# the `seed_mock_provider_catalog` vs `seed_minimax_catalog_entry`
# branch) falls through to the real provider.
#
# KIMI_API_KEY is stripped (env -u) because the Kimi API key is
# temporarily suspended at the provider. Removing the `-u KIMI_API_KEY`
# clauses here and in `test-cli-providers` re-enables the kimi smoke
# test once the key is resumed — the test's existing early-return on
# an unset KIMI_API_KEY will then promote it back to a live run.

test-integration-llm: docker-up
	# See test-integration for the rationale (Phase 0.Z-B pre-build +
	# Phase 12 pre-build).
	cargo build -p peko-cli --bin peko
	cargo build -p peko-daemon --bin peko-daemon
	@if [ -z "$$MINIMAX_API_KEY" ]; then \
	    echo "ERROR: MINIMAX_API_KEY must be set for test-integration-llm"; exit 1; \
	fi
	@env -u MOCK_LLM_URL -u KIMI_API_KEY \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    cargo test $(CARGO_TEST_FLAGS) -- --include-ignored
	@env -u MOCK_LLM_URL -u KIMI_API_KEY \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    cargo test --lib --features test-utils $(INTEGRATION_LIB_TESTS) -- --include-ignored

# ── Everything ───────────────────────────────────────────────────────────

test-all: test test-integration test-integration-llm

# ── Granular slices ──────────────────────────────────────────────────────
# Run one integration test file at a time. Same env rules as test-integration.

test-pekohub: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test pekohub_integration -- --ignored

test-tunnel: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test tunnel_integration -- --ignored

test-tunnel-e2e: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --lib --features test-utils daemon::e2e_tests::tunnel_e2e -- --ignored

test-packaging: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test packaging_integration -- --ignored

test-registry: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test registry_integration -- --ignored

# ── Phase B CLI tests (mock-LLM tier) ──────────────────────────────────────

test-cli-send: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_send -- --ignored

test-cli-basics: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_basics -- --include-ignored

# `peko cron` slice. Uses --interval 1 in the test harness so the poll
# cycle is fast enough to wait for jobs to fire under 30s/test.
test-cli-cron: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_cron -- --include-ignored

# `peko subagent` / `agent_spawn` slice. Uses plain `DaemonGuard::spawn`
# (no `--interval`) — subagent tests don't poll. All multi-turn tests
# in this file are `#[serial]` because they share the mock LLM's
# per-substring counter (see docs/integration/TESTING.md §3 Sequence).
test-cli-subagent: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_subagent -- --include-ignored

# Built-in tools (shell / read_file / write_file / glob / grep /
# str_replace_file) slice. All single-turn tests, all `#[serial]`
# because they share the mock LLM's per-substring counter. The
# 6 `e2e_tests/tools/built-in/*.ps1` scripts that this slice
# replaced were deleted in Phase E (see
# docs/integration/TESTING.md §7). The 4 deferred
# `e2e_tests/tools/tool_{async,timeout,update_mid_session,all}.ps1`
# scripts stay in place — see TESTING.md for the deferred list.
test-cli-tools: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_tools -- --include-ignored

# Issue #14 — manifest signature verification on import (Principal-era).
# Pure unit/integration tests against the in-memory `PrincipalPackager`
# and `PrincipalUnpackager`: no daemon, no registry, no LLM. Does NOT
# gate on `docker-up`. Kept in the mock-LLM env shape for consistency
# with sibling CLI slices so a stray `#[ignore]` would still resolve.
test-cli-agent-signature:
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_agent_signature

# `peko ext install/list/info/enable/disable/uninstall` slice. All
# `#[ignore]` (daemon required) but NOT `#[serial]` — none of these
# tests drive the mock LLM. The L1 (lifecycle-only) PS scripts
# that this slice replaced were deleted in Phase E. The L2
# (start/stop/status) and L3 (LLM-driven tool execution) scripts
# stay in `e2e_tests/extensions/{mcp,skill,universal,gateway}/` —
# they need Python and/or Node runtimes in the test environment.
# See docs/integration/TESTING.md §7 for the extensions migration
# context.
# Removed in ADR-047 Phase 5e along with `peko ext *` and the
# extension framework; the workspace tooling model replaces it.

# Mock LLM sequence feature (Phase C, see docs/integration/TESTING.md §3).
# Exercises the per-substring counter in the list-value branch of
# MOCK_LLM_SCRIPT. Each test starts by POSTing to `/_test/configure` to
# install its script and reset counters, so the shared mock state is
# deterministic regardless of test order.
test-mock-llm-sequence: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test mock_llm_sequence -- --include-ignored

# `peko send` against a real LLM provider (minimax, kimi). Real-LLM
# tier — each test early-returns if its env var is unset, so a bare
# `cargo test` still passes. The GitHub Actions `Integration (real
# LLM)` job fires on nightly cron / `[llm]` commit tag / manual
# dispatch, with `MINIMAX_API_KEY` and `KIMI_API_KEY` passed as
# `secrets.*` env. See docs/integration/TESTING.md §7 for the
# providers migration context.
#
# KIMI_API_KEY is stripped (env -u) because the Kimi API key is
# temporarily suspended. The kimi smoke test's existing early-return
# on an unset KIMI_API_KEY turns into a clean skip in CI logs.
# To re-enable once the key is back: drop the `-u KIMI_API_KEY` and
# restore `KIMI_API_KEY=$(KIMI_API_KEY)` on the next line.
test-cli-providers: docker-up
	@env -u MOCK_LLM_URL -u KIMI_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) \
	    MINIMAX_API_KEY=$(MINIMAX_API_KEY) \
	    cargo test --test cli_providers -- --include-ignored

# ── Phase D — user-journey scenarios (mock-LLM tier) ──────────────────────
# The D4-D6 scenarios live under peko-rs/core/tests/scenarios/. Each
# `sN_*.rs` file is its own integration test binary (registered via
# [[test]] entries in peko-rs/core/Cargo.toml — cargo's auto-discovery
# only finds tests/*.rs directly, not nested subdirs). The mock LLM
# provides the chat payload; what
# we test is the runtime↔registry↔tunnel↔PekoHub-relay orchestration
# plumbing, not LLM decision-making.
#
# D1/D2 (`s1_local_agent_with_extensions`, `s2_extension_registry_roundtrip`)
# were retired in ADR-047 Phase 5 — they exercised the deleted
# `peko ext` CLI and on-disk extension store.

# D4: Publish running agent behind tunnel with permission (flow 6).
# Lands in D4's PR.
test-scenarios-s4: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test s4_publish_running_agent_with_permission -- --include-ignored

# D6: Inline `Principal` grant/revoke round-trips via IPC (ADR-039,
# post issue #30). Replaces the removed s6_revoke_principal_collapse_e2e.
test-scenarios-s6: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test s6_principal_grant_revoke_roundtrip -- --include-ignored

# ── CI entry ─────────────────────────────────────────────────────────────

ci: test test-integration
	@echo "All required tests passed."
