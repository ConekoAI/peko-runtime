# Peko Test Targets — see docs/testing/MANUAL_TEST_PLAN.md
#
# Four canonical targets:
#   test                  — fast unit tests, no Docker, no LLM
#   test-integration      — PR gate, Docker + PekoHub + mock LLM
#   test-integration-llm  — tier above + tests that need a real model (local only)
#   test-all              — everything
#
# Granular per-file targets are kept as slices of test-integration for
# change-isolated dev loops. All mock-tier targets unset LLM_API_KEY
# so they cannot silently leak to the real provider.

.PHONY: help test test-integration test-integration-llm test-all \
        docker-build docker-up docker-down \
        test-lib test-subagent \
        test-tunnel test-tunnel-e2e test-packaging \
        test-cli-send test-cli-session test-cli-basics \
        test-cli-subagent test-cli-tools \
        test-cli-providers \
        test-scenarios-s4 test-scenarios-s6 \
        test-mock-llm-sequence \
        coverage require-real-llm ci

# All integration test crates (live in peko-rs/core/tests/*.rs and
# peko-rs/core/tests/scenarios/*.rs after Phase 0.Z-D moved tests/).
# Kept in sync with `cargo metadata` (targets of kind = ["test"]); the
# Principal migration dropped the cli_compaction / cli_a2a /
# s3_agent_registry_roundtrip suites. ADR-066 P5 replaced OCI/signature
# suites with local snapshot tests.
INTEGRATION_TESTS := tunnel_integration \
                     packaging_integration \
                     cli_send cli_basics cli_subagent \
                     cli_tools cli_principal_import \
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
# Export so `docker compose` can interpolate them — the test stack forwards
# PEKOHUB_URL as the hub's PUBLIC_ORIGIN (ADR-057: the bridge token issuer,
# which must match the origin runtimes derive from their tunnel URL).
export PEKOHUB_URL
export MOCK_LLM_URL

help:
	@echo "Peko Test Targets (see docs/testing/MANUAL_TEST_PLAN.md)"
	@echo ""
	@echo "  test                      Fast unit tests (cargo test --lib, no Docker, no LLM)"
	@echo "  test-integration          PR gate: all tests/*.rs against PekoHub + mock LLM"
	@echo "  test-integration-llm      Tier above + real-LLM tests (local; needs LLM_API_KEY/LLM_BASE_URL/LLM_MODEL)"
	@echo "  test-all                  Everything (unit + mock-LLM + real-LLM)"
	@echo ""
	@echo "  docker-build              Build pekohub-test and mock-llm images"
	@echo "  docker-up                 Start the test stack (pekohub + mock LLM)"
	@echo "  docker-down               Stop and remove the test stack"
	@echo ""
	@echo "  coverage                  Unit-tier line coverage (needs cargo-llvm-cov)"
	@echo "  ci                        Layered run used in GitHub Actions"
	@echo ""
	@echo "  Granular slices of test-integration (one file at a time):"
	@echo "    test-tunnel / test-tunnel-e2e"
	@echo "    test-packaging / test-subagent"
	@echo "    test-cli-send / test-cli-session / test-cli-basics"
	@echo "    test-cli-subagent / test-cli-tools"
	@echo "    test-cli-providers (real-LLM tier — needs LLM_API_KEY/LLM_BASE_URL/LLM_MODEL)"
	@echo "    test-scenarios-s4 (Phase D — publish running agent behind tunnel, mock-LLM)"
	@echo "    test-mock-llm-sequence"

# ── Tier 0: Fast unit tests ──────────────────────────────────────────────

test:
	cargo test --lib

test-lib: test   ## deprecated alias for `test`

test-subagent:
	cargo test --lib subagent_integration

# Line coverage for the unit tier (no Docker). Writes an HTML report to
# target/llvm-cov/html and prints a per-file summary. COVERAGE_ARGS narrows
# the run, e.g. `make coverage COVERAGE_ARGS="-p peko-cron"`.
COVERAGE_ARGS ?= --workspace
coverage:
	@command -v cargo-llvm-cov >/dev/null || { \
	    echo "cargo-llvm-cov is not installed. Install it with:"; \
	    echo "  rustup component add llvm-tools-preview"; \
	    echo "  cargo install cargo-llvm-cov --locked"; \
	    exit 1; }
	cargo llvm-cov $(COVERAGE_ARGS) --lib --html
	cargo llvm-cov report --summary-only

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
# LLM_API_KEY is unset so a leaking env doesn't silently switch the
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
	@env -u LLM_API_KEY \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test $(CARGO_TEST_FLAGS) -- --include-ignored
	@env -u LLM_API_KEY \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --lib --features test-utils $(INTEGRATION_LIB_TESTS) -- --include-ignored

# ── Tier 2: real-LLM tests (local, opt-in; not run in CI) ────────────────
# MOCK_LLM_URL is unset so dual-mode tests (e.g. the inline
# `daemon::e2e_tests::tunnel_e2e`) fall through to the real endpoint
# described by LLM_API_KEY / LLM_BASE_URL / LLM_MODEL [/ LLM_API_FORMAT]
# (see peko-rs/core/tests/common/real_llm.rs). Mock-only tests skip.

test-integration-llm: docker-up
	# See test-integration for the rationale (Phase 0.Z-B pre-build +
	# Phase 12 pre-build).
	cargo build -p peko-cli --bin peko
	cargo build -p peko-daemon --bin peko-daemon
	@$(MAKE) --no-print-directory require-real-llm
	@env -u MOCK_LLM_URL \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    cargo test $(CARGO_TEST_FLAGS) -- --include-ignored
	@env -u MOCK_LLM_URL \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    cargo test --lib --features test-utils $(INTEGRATION_LIB_TESTS) -- --include-ignored

# ── Everything ───────────────────────────────────────────────────────────

test-all: test test-integration test-integration-llm

# ── Granular slices ──────────────────────────────────────────────────────
# Run one integration test file at a time. Same env rules as test-integration.

test-tunnel: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test tunnel_integration -- --ignored

test-tunnel-e2e: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --lib --features test-utils daemon::e2e_tests::tunnel_e2e -- --ignored

test-packaging: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test packaging_integration -- --ignored

# ── Phase B CLI tests (mock-LLM tier) ──────────────────────────────────────

test-cli-send: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_send -- --ignored

test-cli-basics: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_basics -- --include-ignored

# `peko subagent` / `agent_spawn` slice. Uses plain `DaemonGuard::spawn`
# (no `--interval`) — subagent tests don't poll. All multi-turn tests
# in this file are `#[serial]` because they share the mock LLM's
# per-substring counter (see .github/docker/mock-llm/mock_llm_server.py).
test-cli-subagent: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_subagent -- --include-ignored

# ADR-052 tiered system prompt slice (D2/D3/D4/D5/D6). All `#[serial]`:
# the tests share the mock's per-substring counter, and the two
# wire-recording tests (D3 spawn + peer_agent) briefly override the
# process-wide MOCK_LLM_URL to route the daemon through an in-process
# recording proxy.
test-tiered-prompt: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test tiered_prompt -- --include-ignored --test-threads=1

# Built-in tools daemon path: one scripted turn drives Read/Glob/Grep/
# Write/Edit/Bash through `peko send` and checks files + the persisted
# transcript. In-process coverage of all 19 tools lives in `make test`
# (tools::builtin::test_harness).
test-cli-tools: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_tools -- --include-ignored

# Mock LLM sequence feature (Phase C, see .github/docker/mock-llm/mock_llm_server.py).
# Exercises the per-substring counter in the list-value branch of
# MOCK_LLM_SCRIPT. Each test starts by POSTing to `/_test/configure` to
# install its script and reset counters, so the shared mock state is
# deterministic regardless of test order.
test-mock-llm-sequence: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test mock_llm_sequence -- --include-ignored

# `peko send` against the env-described real LLM (common::real_llm).
# Local, opt-in: CI does not run the real-LLM tier.
test-cli-providers: docker-up
	@$(MAKE) --no-print-directory require-real-llm
	@env -u MOCK_LLM_URL PEKOHUB_URL=$(PEKOHUB_URL) \
	    cargo test --test cli_providers -- --include-ignored

require-real-llm:
	@for var in LLM_API_KEY LLM_BASE_URL LLM_MODEL; do \
	    if [ -z "$$(printenv $$var)" ]; then \
	        echo "ERROR: $$var must be set for real-LLM tests (see peko-rs/core/tests/common/real_llm.rs)"; exit 1; \
	    fi; \
	done

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
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test s4_publish_running_agent_with_permission -- --include-ignored

# D6: Inline `Principal` grant/revoke round-trips via IPC (ADR-039,
# post issue #30). Replaces the removed s6_revoke_principal_collapse_e2e.
test-scenarios-s6: docker-up
	@env -u LLM_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test s6_principal_grant_revoke_roundtrip -- --include-ignored

# ── CI entry ─────────────────────────────────────────────────────────────

ci: test test-integration
	@echo "All required tests passed."
