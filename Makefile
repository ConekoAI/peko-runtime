# Pekobot Containerized Test Targets
.PHONY: help test test-lib test-integration test-pekohub test-tunnel \
        test-tunnel-e2e test-packaging test-subagent test-all-integration \
        docker-build docker-up docker-down test-in-docker ci

help:
	@echo "Pekobot Containerized Test Targets"
	@echo ""
	@echo "  test                Run all non-ignored tests (fast)"
	@echo "  test-lib            Run library tests only"
	@echo "  test-integration    Run all integration tests in Docker"
	@echo "  test-pekohub        Run pekohub_integration tests (registry + hub)"
	@echo "  test-tunnel         Run tunnel_integration tests"
	@echo "  test-tunnel-e2e     Run tunnel_e2e tests (with mock LLM)"
	@echo "  test-packaging      Run packaging_integration tests"
	@echo "  test-subagent       Run subagent integration tests (un-ignored)"
	@echo "  test-all-integration Run ALL integration tests"
	@echo "  test-full-e2e       Run Layer 4 full Docker Compose E2E"
	@echo ""
	@echo "  docker-build        Build all Docker images"
	@echo "  docker-up           Start Docker Compose stack"
	@echo "  docker-down         Stop Docker Compose stack"
	@echo "  ci                  Run full CI test suite"

test:
	cargo test --lib

test-lib:
	cargo test --lib

# ── Docker-based Integration Tests ──────────────────────────────────────

docker-build:
	docker build -t peko/pekohub-test:latest \
		-f .github/docker/pekohub-test/Dockerfile ../pekohub
	docker build -t peko/mock-llm:latest \
		-f .github/docker/mock-llm/Dockerfile .github/docker/mock-llm

docker-up:
	docker-compose -f tests/docker/docker-compose.integration.yml up -d

docker-down:
	docker-compose -f tests/docker/docker-compose.integration.yml down -v

test-pekohub: docker-up
	docker-compose -f tests/docker/docker-compose.integration.yml \
		run --rm test-runner cargo test --test pekohub_integration -- --ignored

test-tunnel: docker-up
	docker-compose -f tests/docker/docker-compose.integration.yml \
		run --rm test-runner cargo test --test tunnel_integration -- --ignored

test-tunnel-e2e: docker-up
	docker-compose -f tests/docker/docker-compose.integration.yml \
		run --rm -e MOCK_LLM_URL=http://mock-llm:8080 \
		test-runner cargo test --test tunnel_e2e -- --ignored

test-packaging: docker-up
	docker-compose -f tests/docker/docker-compose.integration.yml \
		run --rm test-runner cargo test --test packaging_integration -- --ignored

test-subagent:
	cargo test --lib subagent_integration

test-all-integration: test-pekohub test-tunnel test-packaging test-subagent

# ── Layer 4: Full E2E ───────────────────────────────────────────────────

test-full-e2e:
	@echo "Starting full Docker Compose stack..."
	cd integration-tests && docker-compose up -d
	@echo "Waiting for services..."
	@sleep 15
	cd integration-tests && ./run_e2e_tests.sh
	cd integration-tests && docker-compose down -v

# ── CI ──────────────────────────────────────────────────────────────────

ci: test-lib test-all-integration
	@echo "All tests passed!"
