# Pekobot Test Targets — see docs/integration/TESTING.md
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
        test-cli-subagent test-cli-tools test-cli-compaction \
        test-cli-extensions test-cli-providers test-cli-a2a \
        test-mock-llm-sequence \
        ci

# All integration test crates (live in tests/*.rs).
INTEGRATION_TESTS := pekohub_integration tunnel_integration tunnel_e2e \
                     packaging_integration registry_integration \
                     team_integration extension_packaging \
                     cli_send cli_session cli_basics cli_cron cli_subagent \
                     cli_tools cli_compaction cli_extensions cli_providers \
                     cli_a2a \
                     mock_llm_sequence
CARGO_TEST_FLAGS  := $(addprefix --test ,$(INTEGRATION_TESTS))

# Default ports exposed by docker-compose.integration.yml; CI overrides
# these for in-container runs (e.g. PEKOHUB_URL=http://pekohub-test:3000).
#
# Uses `docker compose` (v2 plugin) rather than the standalone `docker-compose`
# (v1) binary, which is not present on GitHub-hosted Linux runners and is
# being deprecated by Docker Desktop.
PEKOHUB_URL  ?= http://localhost:3000
MOCK_LLM_URL ?= http://localhost:8080

help:
	@echo "Pekobot Test Targets (see docs/integration/TESTING.md)"
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
	@echo "    test-cli-subagent / test-cli-tools / test-cli-compaction"
	@echo "    test-cli-extensions"
	@echo "    test-cli-providers (real-LLM tier — needs MINIMAX_API_KEY / KIMI_API_KEY)"
	@echo "    test-cli-a2a (real-LLM tier — needs MINIMAX_API_KEY; 2-LLM-call flows)"
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
	docker compose -f tests/docker/docker-compose.integration.yml up -d

docker-down:
	docker compose -f tests/docker/docker-compose.integration.yml down -v

# ── Tier 1: PR gate — Docker + PekoHub + mock LLM ────────────────────────
# MINIMAX_API_KEY is unset so a leaking env doesn't silently switch the
# dual-mode tests to the real provider.
#
# --include-ignored runs BOTH the hub-gated #[ignore] tests AND the
# always-on pure-Rust tests in team_integration / extension_packaging
# (10) plus the 6 offline CLI tests in cli_basics.
# Plain --ignored would skip those 16 always-on tests entirely.

test-integration: docker-up
	@env -u MINIMAX_API_KEY \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test $(CARGO_TEST_FLAGS) -- --include-ignored

# ── Tier 2: nightly + [llm] commit tag — adds real-LLM tests ─────────────
# MOCK_LLM_URL is unset so the dual-mode rule at tunnel_e2e.rs:63-76
# falls through to the real provider.

test-integration-llm: docker-up
	@if [ -z "$$MINIMAX_API_KEY" ]; then \
	    echo "ERROR: MINIMAX_API_KEY must be set for test-integration-llm"; exit 1; \
	fi
	@env -u MOCK_LLM_URL \
	    PEKOHUB_URL=$(PEKOHUB_URL) \
	    cargo test $(CARGO_TEST_FLAGS) -- --include-ignored

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
	    cargo test --test tunnel_e2e -- --ignored

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

test-cli-session: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_session -- --ignored

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
# because they share the mock LLM's per-substring counter. Replaces
# the mockable subset of `e2e_tests/tools/built-in/*.ps1`. See
# docs/integration/TESTING.md §7 for the deferred list
# (tool_async / tool_timeout / tool_update_mid_session).
test-cli-tools: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_tools -- --include-ignored

# `peko session compact` slice. `peko session compact` is
# truncation-based (see src/compaction/cli.rs:75), so the compaction
# itself doesn't need a real LLM — only the multi-turn setup phase
# does, and that's scripted via mock-LLM tool_call sequences. All
# `#[serial]`. See docs/integration/TESTING.md §7 for the deferred
# `compaction_auto.ps1` (auto-compaction uses the LLM to generate the
# summary text — real-LLM tier).
test-cli-compaction: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_compaction -- --include-ignored

# `peko ext install/list/info/enable/disable/uninstall` slice. All
# `#[ignore]` (daemon required) but NOT `#[serial]` — none of these
# tests drive the mock LLM. Replaces the L1 (lifecycle-only) subset
# of `e2e_tests/extensions/*.ps1`; the L2 (start/stop/status) and
# L3 (LLM-driven tool execution) tests stay deferred to a follow-up
# because they require Python and/or Node runtimes in the test
# environment. See docs/integration/TESTING.md §7 for the
# extensions migration context.
test-cli-extensions: docker-up
	@env -u MINIMAX_API_KEY PEKOHUB_URL=$(PEKOHUB_URL) MOCK_LLM_URL=$(MOCK_LLM_URL) \
	    cargo test --test cli_extensions -- --include-ignored

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
test-cli-providers: docker-up
	@env -u MOCK_LLM_URL PEKOHUB_URL=$(PEKOHUB_URL) \
	    MINIMAX_API_KEY=$(MINIMAX_API_KEY) KIMI_API_KEY=$(KIMI_API_KEY) \
	    cargo test --test cli_providers -- --include-ignored

# `peko send` with the `a2a_send` built-in tool (agent-to-agent
# messaging). Real-LLM tier — each test is a 2-LLM-call flow
# (delegator → a2a_send → worker). Tests early-return if
# `MINIMAX_API_KEY` is unset, so a bare `cargo test` still passes.
# Total wall clock is ~3-5 min for all 13 tests. See
# docs/integration/TESTING.md §7 for the a2a migration context.
test-cli-a2a: docker-up
	@env -u MOCK_LLM_URL PEKOHUB_URL=$(PEKOHUB_URL) \
	    MINIMAX_API_KEY=$(MINIMAX_API_KEY) \
	    cargo test --test cli_a2a -- --include-ignored

# ── CI entry ─────────────────────────────────────────────────────────────

ci: test test-integration
	@echo "All required tests passed."
