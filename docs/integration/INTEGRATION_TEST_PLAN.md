# Integration Test Plan: peko-runtime ↔ pekohub

> **Goal:** Verify that peko-runtime (Rust CLI/daemon) and pekohub (Node.js registry) work together correctly across all integration points.

---

## 1. Overview

### What We're Testing

| Component | Role | Tech |
|-----------|------|------|
| `peko-runtime` | Rust-based multi-agent runtime — pushes/pulls agent packages, authenticates, searches | Rust, CLI + library |
| `pekohub` | Public registry — OCI Distribution Spec v1.1 + custom APIs for agents/teams/extensions | Node.js 22, Fastify, PostgreSQL, S3, Meilisearch |

### Integration Surface

The two systems communicate via **HTTP** over the OCI Distribution Spec v1.1 and custom REST APIs:

```
┌─────────────────┐         HTTP/1.1          ┌─────────────────┐
│  peko-runtime   │  ◄────────────────────►  │    pekohub      │
│   (Rust CLI)    │   OCI v1.1 + Custom API  │  (Fastify API)  │
└─────────────────┘                          └─────────────────┘
       │                                            │
       │  ┌─ Push agent/team/extension packages     │  ┌─ PostgreSQL
       │  ├─ Pull packages by tag/digest            ├──┼─ S3 (blobs)
       │  ├─ Search registry                        │  └─ Meilisearch
       │  ├─ List tags / catalog                    │
       │  └─ Authenticate (API key / OAuth)         │
       │                                            │
```

---

## 2. Test Strategy: Three Tiers

We use a **three-tier pyramid** to balance coverage, speed, and realism:

```
        ┌─────────────┐
        │   Tier 3    │  Full E2E: real pekohub + real peko-runtime
        │  (Slowest)  │  Docker Compose stack, CI nightly
        │   ~10 min   │
        ├─────────────┤
        │   Tier 2    │  Contract Tests: pekohub running, runtime as client
        │  (Medium)   │  Rust integration tests against real backend
        │   ~2 min    │
        ├─────────────┤
        ├─────────────┤
        │   Tier 1    │  Mocked Tests: mock registry ↔ real runtime logic
        │  (Fastest)  │  Rust tests with Python mock server (existing)
        │   ~30 sec   │
        └─────────────┘
```

### Tier 1: Mocked Contract Tests ✅ COMPLETE

**Status:** Complete — 14 tests, all passing

**What is tested:**
- Registry client push/pull roundtrip
- Layer deduplication (skip existing)
- Digest verification on download
- Progress event streaming
- Error handling (404, 409, invalid digest)
- **Manifest media type validation** — Mock accepts both `application/vnd.peko.manifest.v1+json` and `application/vnd.oci.image.manifest.v1+json`; rejects invalid types
- **Auth header verification** — `--auth-token` protects mutating operations; read operations remain public
- **Namespace resolution** — Bare refs like `my-agent:v1` resolve to `default/peko/agents/my-agent:v1`
- **Catalog & tags endpoints** — `GET /v2/_catalog` and `GET /v2/{name}/tags/list` with pagination

**Run:**
```bash
cd peko-runtime
cargo test --test registry_integration -- --ignored
```

**Key implementation details:**
- Mock registry auto-starts on ephemeral port per test (no manual `python main.py --port 18765` needed)
- Tests use `MockRegistry::start(None).await` which spawns Python, parses `PORT=...` from stdout, and waits for `/v2/` health check
- `RegistryRef::parse` now correctly handles `host:port/path:tag` via `looks_like_host_port()`
- `media_types::MANIFEST_DEFAULT` is `MANIFEST_OCI` — runtime now sends OCI media type on push
- `RegistryClient::accept_manifest_media_types()` returns both accepted types for Content-Type negotiation

---

### Tier 2: Live pekohub Contract Tests (New)

**Goal:** Run pekohub backend (with test DB) and have peko-runtime's Rust integration tests hit it directly.

**Architecture:**

```
┌─────────────────────────────────────────────────────────────┐
│                    Test Harness (Rust test)                  │
│  ┌──────────────┐         ┌──────────────────────────────┐  │
│  │ Test fixture │  HTTP   │  pekohub backend (test mode) │  │
│  │   (Rust)     │  ───►   │  - PGlite (in-memory PG)     │  │
│  │              │         │  - Mock S3 (memory store)    │  │
│  │              │  ◄───   │  - Mock search (no-op)       │  │
│  │              │         │  - Dev auth bypass ON        │  │
│  └──────────────┘         └──────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

**pekohub test mode setup:**
- Use `PGlite` (already used in `tests/integration/`) for zero-config PostgreSQL
- Use in-memory/mock S3 instead of MinIO
- Disable Meilisearch (mock the search plugin)
- Enable `ALLOW_DEV_AUTH_BYPASS=true` + `NODE_ENV=development` to skip OAuth
- Start Fastify on a random ephemeral port

**New Rust test file:** `peko-runtime/tests/pekohub_integration.rs`

| Test Case | Description |
|-----------|-------------|
| `test_pekohub_push_agent` | Build agent locally → push to pekohub → verify catalog lists it |
| `test_pekohub_pull_agent` | Push agent → pull by tag → verify layers match |
| `test_pekohub_pull_by_digest` | Push agent → pull by manifest digest → verify |
| `test_pekohub_tag_listing` | Push multiple versions → list tags → verify order |
| `test_pekohub_layer_dedup` | Push agent A (layers L1, L2) → push agent B (shares L1) → verify L1 not re-uploaded |
| `test_pekohub_409_on_repush` | Push tag `v1.0` → push same tag again → expect 409 |
| `test_pekohub_search_indexing` | Push agent with metadata → search API returns it |
| `test_pekohub_manifest_annotations` | Push with `dev.pekohub.metadata` annotations → verify bundle record in DB |
| `test_pekohub_catalog_pagination` | Push 5 agents → paginate catalog with `n` and `last` |

**Run:**
```bash
cd peko-runtime
# Test harness starts pekohub backend automatically
cargo test --test pekohub_integration
```

---

### Tier 3: Full End-to-End Tests (New)

**Goal:** Verify the complete user journey with real infrastructure.

**Architecture:**

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         Docker Compose Stack                             │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌──────────┐ │
│  │   pekohub   │    │ PostgreSQL  │    │    MinIO    │    │Meilisearch│ │
│  │   backend   │◄──►│   (5432)    │    │   (9000)    │    │  (7700)   │ │
│  │   (:3000)   │    └─────────────┘    └─────────────┘    └──────────┘ │
│  └──────┬──────┘                                                        │
│         │ HTTP                                                          │
│  ┌──────┴──────┐                                                        │
│  │ peko-runtime│  Built from source, runs CLI commands                   │
│  │   (CLI)     │  against pekohub backend                                │
│  └─────────────┘                                                        │
└─────────────────────────────────────────────────────────────────────────┘
```

**Test scenarios (PowerShell or Rust-driven):**

| Scenario | Steps |
|----------|-------|
| **S1: Publish & Discover** | 1. Create agent → 2. Export `.agent` → 3. `pekobot agent push` to pekohub → 4. `pekobot agent search` finds it → 5. Another user `pekobot agent pull` → 6. Import and verify |
| **S2: Team Collaboration** | 1. Create team with 2 agents → 2. Export `.team` → 3. Push to pekohub → 4. Pull on another machine → 5. Import → 6. Verify both agents work |
| **S3: Versioned Extension** | 1. Build extension → 2. Push v1.0 → 3. Push v1.1 → 4. List versions → 5. Pull specific version → 6. Install and verify tools work |
| **S4: Auth Flow** | 1. Generate API key via pekohub web → 2. `pekobot login --api-key` → 3. Push (should succeed) → 4. Push to wrong namespace (should fail 403) → 5. Pull without auth (should succeed) |
| **S5: Cross-Platform Share** | 1. Build agent on Windows → 2. Push → 3. Pull on Linux (Docker) → 4. Verify config/memory preserved |

**Run:**
```bash
cd integration-tests/
docker-compose up -d pekohub postgres minio meilisearch
# Wait for health checks
./run_e2e_tests.ps1   # or ./run_e2e_tests.sh
```

---

## 3. Test Data & Fixtures

### Agent Package Fixture

A minimal `.agent` package for deterministic tests:

```
test-fixture.agent
├── manifest.json          # AgentManifest (v1.0.0, name: "integration-test")
├── layers/
│   ├── config/            # Agent config (JSON)
│   ├── identity/          # DID document + keys
│   └── skills/            # One SKILL.md
```

Generated via a Rust helper in the test suite:
```rust
fn create_test_agent_package(name: &str) -> (PathBuf, AgentManifest) {
    // Creates temp dir, writes layers, returns path + manifest
}
```

### pekohub Test Fixture (Node.js)

A shared test harness for pekohub:

```typescript
// pekohub/backend/tests/fixtures/integration.ts
export async function buildTestApp(opts?: { authBypass?: boolean }): Promise<FastifyInstance>;
export async function createTestUser(app: FastifyInstance, namespace: string): Promise<{ apiKey: string }>;
export async function pushTestBundle(app: FastifyInstance, namespace: string, name: string, manifest: object): Promise<void>;
```

---

## 4. CI/CD Integration

### GitHub Actions Workflow

```yaml
# .github/workflows/integration-tests.yml
name: Integration Tests

on:
  push:
    branches: [main]
  pull_request:
    paths:
      - 'peko-runtime/src/registry/**'
      - 'pekohub/backend/src/routes/oci/**'
      - 'pekohub/backend/src/routes/api/**'
  schedule:
    - cron: '0 2 * * *'  # Nightly

jobs:
  tier1-mocked:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-action@stable
      - uses: actions/setup-python@v5
        with: { python-version: '3.12' }
      - run: pip install fastapi uvicorn
      - run: cd peko-runtime && cargo test --test registry_integration -- --ignored

  tier2-contract:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-action@stable
      - uses: actions/setup-node@v4
        with: { node-version: '22' }
      - run: |
          cd pekohub
          pnpm install
          cd backend && pnpm db:push  # PGlite may not need this
      - run: cd peko-runtime && cargo test --test pekohub_integration
        env:
          PEKOHUB_TEST_BACKEND_PATH: ../pekohub/backend

  tier3-e2e:
    runs-on: ubuntu-latest
    if: github.event_name == 'schedule' || contains(github.event.head_commit.message, '[e2e]')
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - run: cd integration-tests && docker-compose up -d
      - run: sleep 15  # Wait for services
      - run: cd integration-tests && ./run_e2e_tests.sh
```

---

## 5. Implementation Roadmap

### Phase 1: Tier 1 Improvements (Week 1) ✅ COMPLETE
- [x] Enhance Python mock registry to validate Peko manifest media types
- [x] Add auth header validation to mock registry
- [x] Add namespace resolution tests to `registry_integration.rs`
- [x] Un-ignore and stabilize existing mock tests in CI

### Phase 2: Tier 2 Contract Tests (Week 2-3)
- [ ] Create `pekohub/backend/tests/fixtures/integration.ts` shared harness
- [ ] Add `ALLOW_DEV_AUTH_BYPASS` support to pekohub backend config
- [ ] Create `peko-runtime/tests/pekohub_integration.rs` with test harness that:
  - Spawns pekohub backend as a child process (Node.js)
  - Waits for health check
  - Runs push/pull/search/catalog tests
  - Kills backend on cleanup
- [ ] Add CI job for Tier 2

### Phase 3: Tier 3 E2E Tests (Week 4-5)
- [ ] Create `integration-tests/docker-compose.yml` with full stack
- [ ] Create `integration-tests/run_e2e_tests.ps1` / `.sh`
- [ ] Implement S1-S5 scenarios
- [ ] Add nightly CI job

### Phase 4: Polish (Week 6)
- [ ] Add test reporting (JUnit XML)
- [ ] Add coverage reporting for integration paths
- [ ] Document debugging guide for flaky tests

---

## 6. Risk & Mitigation

| Risk | Impact | Mitigation |
|------|--------|------------|
| pekohub backend startup is slow (~5s) | Tier 2 tests slow | Use PGlite + mock plugins; spawn once per test file |
| Port collisions in parallel tests | Flaky tests | Use ephemeral port 0 + parse from stdout |
| Windows path issues in E2E | Tests fail on Windows | Use forward slashes in tests; test on both OS |
| LLM-dependent tests are flaky | False negatives | Use deterministic keyword verification (already in packaging tests) |
| S3/MinIO state leakage between tests | Isolation failures | Reset MinIO bucket before each scenario |

---

## 7. Appendix: API Compatibility Matrix

| Feature | peko-runtime sends | pekohub expects | Status |
|---------|-------------------|-----------------|--------|
| Manifest PUT | `application/vnd.oci.image.manifest.v1+json` | `application/vnd.oci.image.manifest.v1+json` | ✅ Aligned — runtime sends `MANIFEST_DEFAULT` (OCI type) |
| Layer media type | `application/vnd.peko.layer.v1.tar+gzip` | `application/octet-stream` or any | ✅ Compatible |
| Auth header | `Bearer <token>` | `Bearer ph_...` or `<jwt>` | ✅ Compatible |
| Reference format | `host/ns/name:tag` | `host/ns/name:tag` | ✅ Compatible |
| Bare ref resolution | `my-agent:tag` → `default/peko/agents/my-agent:tag` | N/A (client-side) | ✅ Compatible |
| Catalog pagination | `?n=20&last=...` | `?n=20&last=...` | ✅ Compatible |
| Search API | `GET /api/v1/search?q=...` | `GET /api/v1/search?q=...` | ✅ Compatible |

### ✅ Manifest Media Type Alignment — RESOLVED

The runtime now uses `application/vnd.oci.image.manifest.v1+json` as the default manifest media type for push operations (`media_types::MANIFEST_DEFAULT`). The legacy Peko type (`application/vnd.peko.manifest.v1+json`) is still accepted on pull for backward compatibility.

**Changes:**
- `src/registry/mod.rs`: Added `MANIFEST_PEKO`, `MANIFEST_OCI`, `MANIFEST_DEFAULT`, `MANIFEST_ALL`
- `src/registry/client.rs`: `push_manifest()` uses `media_types::MANIFEST_DEFAULT`; added `accept_manifest_media_types()`
- Mock registry accepts both types on PUT and returns the stored type on GET
