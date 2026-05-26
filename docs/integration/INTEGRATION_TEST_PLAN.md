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

## 2. Test Strategy: Four Layers

We use a **layered approach** that balances coverage, speed, and realism. The existing `e2e_tests/` directory already contains PowerShell-driven CLI tests — these are the "user journey" layer. Our new Rust integration tests add faster, more granular contract verification.

```
        ┌─────────────────────────────────────────┐
        │  Layer 4: Full E2E (Docker Compose)     │  Real pekohub + real peko-runtime
        │  ~10 min  │  CI nightly                 │  S1-S5 scenarios from plan
        ├─────────────────────────────────────────┤
        │  Layer 3: PowerShell E2E Tests          │  CLI-driven user journeys
        │  ~5 min   │  e2e_tests/packaging/*.ps1  │  Export → push → pull → import
        ├─────────────────────────────────────────┤
        │  Layer 2: Live Contract Tests           │  Rust tests against real pekohub
        │  ~2 min   │  tests/pekohub_integration.rs│  Push/pull/catalog/search
        ├─────────────────────────────────────────┤
        │  Layer 1: Mocked Contract Tests         │  Rust tests with mock registry
        │  ~30 sec  │  tests/registry_integration.rs│  Protocol + auth + media types
        └─────────────────────────────────────────┘
```

### How the Existing E2E Tests Fit In

The `e2e_tests/` directory (PowerShell scripts) sits at **Layer 3**. These tests exercise the complete CLI workflow:

| Directory | Tests | Layer | Needs LLM? |
|-----------|-------|-------|------------|
| `e2e_tests/packaging/` | Registry push/pull, dedup, export/import, team lifecycle | Layer 3 | Some (optional) |
| `e2e_tests/agent/` | Agent create/show/remove basics | Layer 3 | No |
| `e2e_tests/team/` | Team create/list/show | Layer 3 | No |
| `e2e_tests/send/` | Message sending, streaming, profiles | Layer 3 | Yes |
| `e2e_tests/extensions/` | Skill/MCP/universal tool lifecycle | Layer 3 | No |
| `e2e_tests/session/` | Session list/branch/switch/remove | Layer 3 | No |
| `e2e_tests/cron/` | Cron add/list/remove/run | Layer 3 | No |
| `e2e_tests/config/` | Config get/set/validate | Layer 3 | No |

**Key insight:** The PowerShell E2E tests already cover the "happy path" user journeys (Layer 3). What was missing was:
1. **Layer 1** — Fast, granular protocol contract tests (now complete)
2. **Layer 2** — Tests against the real pekohub backend (next phase)
3. **Layer 4** — Full Docker Compose stack with real infrastructure (future)

The PowerShell packaging tests (`registry_push_pull.ps1`, `registry_layer_dedup.ps1`, etc.) will benefit from the enhanced mock registry (auth, media types, catalog) and the fixed `host:port` parsing.

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

### Tier 2: Live pekohub Contract Tests ✅ COMPLETE

**Status:** Complete — 6 tests, all passing

**Goal:** Run pekohub backend (with test DB) and verify OCI protocol + custom API compatibility via raw HTTP.

**Architecture:**

```
┌─────────────────────────────────────────────────────────────┐
│                    Test Harness (Rust test)                  │
│  ┌──────────────┐         ┌──────────────────────────────┐  │
│  │ Test fixture │  HTTP   │  pekohub backend (test mode) │  │
│  │   (Rust)     │  ───►   │  - PGlite (in-memory PG)     │  │
│  │              │         │  - Mock S3 (memory store)    │  │
│  │              │  ◄───   │  - Mock search (Map-based)   │  │
│  │              │         │  - Dev auth bypass ON        │  │
│  └──────────────┘         └──────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

**pekohub test mode setup:**
- `PGlite` for zero-config PostgreSQL (via `tests/fixtures/db.ts`)
- In-memory `Map`-based mock storage (no S3/MinIO needed)
- In-memory `Map`-based mock search (no Meilisearch needed)
- `ALLOW_DEV_AUTH_BYPASS=true` + `NODE_ENV=development` to skip OAuth
- Fastify starts on random ephemeral port; port parsed from `PORT=...` stdout

**Test file:** `peko-runtime/tests/pekohub_integration.rs`

| Test Case | Description |
|-----------|-------------|
| `test_pekohub_health_check` | Verify backend starts and `/health` returns 200 |
| `test_pekohub_manifest_roundtrip` | PUT OCI manifest → GET by tag → verify body |
| `test_pekohub_blob_upload_and_download` | POST upload → PUT blob → HEAD → GET → verify content |
| `test_pekohub_catalog_and_tags` | Push 2 agents → verify `_catalog` and `tags/list` |
| `test_pekohub_search_api` | Push agent with `dev.pekohub.metadata` → search finds it |
| `test_pekohub_bundle_detail_api` | Push agent → verify `/api/v1/bundles/ns/name` and versions |

**Not tested at Layer 2 (by design):**
- `RegistryClient` push/pull — uses Peko-specific manifest format (`schema_version`, `size_bytes`, `layer_type`) that is NOT OCI-compliant. Tested in Layer 1 against mock registry. CLI E2E (Layer 3) handles OCI conversion.

**Run:**
```bash
cd peko-runtime
# Test harness starts pekohub backend automatically
cargo test --test pekohub_integration -- --ignored
```

**Key implementation details:**
- `PekohubBackend` struct spawns Node.js + tsx, parses port from stdout, kills on Drop
- Tests use raw `reqwest` HTTP (not `RegistryClient`) to ensure OCI compliance
- Config blobs must be uploaded before manifest PUT (pekohub validates blob existence)
- Manifest annotations must include `org.opencontainers.image.authors` for `BundleDetail` parsing
- Version tags must be valid semver (e.g., `v2.0.0`, not `v2.0`)

---

### Layer 3: PowerShell E2E Tests (Existing — Enhanced)

**Status:** Exists in `e2e_tests/` — tests the full CLI workflow end-to-end.

**What they cover:**
- Real agent creation, export, push, pull, import via `peko` CLI
- Layer deduplication across multiple agents
- Team lifecycle with registry operations
- Extension bundling and registry operations
- Cross-platform agent sharing

**What we should enhance:**
1. **Update packaging tests to use new mock registry features:**
   - Test auth-protected pushes (`--auth-token`)
   - Verify OCI media type is sent on push
   - Test catalog/tag listing after push
   - Test namespace validation (push to invalid repo name should fail)

2. **Add a new PowerShell test:** `pekohub_contract_test.ps1`
   - Runs against a local pekohub backend (not mock registry)
   - Verifies runtime ↔ pekohub compatibility
   - Can be run in CI after pekohub backend starts

**Run:**
```powershell
cd e2e_tests/packaging
./registry_push_pull.ps1          # Uses mock registry
./registry_layer_dedup.ps1        # Uses mock registry
./packaging_all.ps1               # Runs all packaging tests
```

---

### Layer 4: Full End-to-End Tests (New)

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
| **S1: Publish & Discover** | 1. Create agent → 2. Export `.agent` → 3. `peko agent push` to pekohub → 4. `peko agent search` finds it → 5. Another user `peko agent pull` → 6. Import and verify |
| **S2: Team Collaboration** | 1. Create team with 2 agents → 2. Export `.team` → 3. Push to pekohub → 4. Pull on another machine → 5. Import → 6. Verify both agents work |
| **S3: Versioned Extension** | 1. Build extension → 2. Push v1.0 → 3. Push v1.1 → 4. List versions → 5. Pull specific version → 6. Install and verify tools work |
| **S4: Auth Flow** | 1. Generate API key via pekohub web → 2. `peko login --api-key` → 3. Push (should succeed) → 4. Push to wrong namespace (should fail 403) → 5. Pull without auth (should succeed) |
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

### Phase 1: Layer 1 — Mocked Contract Tests (Week 1) ✅ COMPLETE
- [x] Enhance Python mock registry to validate Peko manifest media types
- [x] Add auth header validation to mock registry
- [x] Add namespace resolution tests to `registry_integration.rs`
- [x] Un-ignore and stabilize existing mock tests in CI
- [x] Fix `RegistryRef::parse` for `host:port/path:tag`
- [x] Align runtime manifest media type to OCI (`MANIFEST_DEFAULT`)

### Phase 2: Layer 2 — Live pekohub Contract Tests (Week 2-3) ✅ COMPLETE
- [x] Create `pekohub/backend/tests/fixtures/server.ts` — standalone test server with PGlite + mock storage/search
- [x] Add `ALLOW_DEV_AUTH_BYPASS` support to pekohub backend config
- [x] Create `peko-runtime/tests/pekohub_integration.rs` with test harness that:
  - Spawns pekohub backend as a child process (Node.js + tsx)
  - Parses ephemeral port from `PORT=...` stdout
  - Waits for `/health` before running tests
  - Kills backend on Drop
- [x] 6 tests passing: health, manifest roundtrip, blob upload, catalog/tags, search API, bundle detail
- [ ] Add CI job for Layer 2

### Phase 3: Layer 3 — PowerShell E2E Test Enhancements (Week 3-4) ✅ PARTIAL
- [x] Create `RegistryTestHelpers.ps1` — unified mock/pekohub backend abstraction
- [x] Create `pekohub_contract_test.ps1` — CLI contract test against real PekoHub
  - Auto-detects PekoHub availability, falls back to mock
  - Tests: export → push → pull → import → dedup → error cases
  - Documents known OCI format mismatch (RegistryClient vs PekoHub)
- [ ] Update `registry_push_pull.ps1` to test auth-protected pushes
- [ ] Update `registry_layer_dedup.ps1` to verify OCI media type on push
- [ ] Add catalog/tag listing verification to packaging tests
- [ ] Ensure all packaging tests pass with enhanced mock registry

### Phase 4: Layer 4 — Full Docker Compose E2E (Week 5-6)
- [ ] Create `integration-tests/docker-compose.yml` with full stack
- [ ] Create `integration-tests/run_e2e_tests.ps1` / `.sh`
- [ ] Implement S1-S5 scenarios
- [ ] Add nightly CI job

### Phase 5: Polish (Week 7)
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
| Manifest PUT (CLI) | `application/vnd.oci.image.manifest.v1+json` | `application/vnd.oci.image.manifest.v1+json` | ⚠️ **NOT aligned** — CLI's `RegistryClient` pushes Peko format, not OCI. Needs fix. |
| Manifest PUT (RegistryClient direct) | Peko format (`schema_version`, `size_bytes`, `layer_type`) | OCI format (`schemaVersion`, `size`, `mediaType`) | ⚠️ Mismatch — `RegistryClient::push_manifest()` serializes `RegistryManifest` directly without OCI conversion |
| Layer media type | `application/vnd.peko.layer.v1.tar+gzip` | `application/octet-stream` or any | ✅ Compatible |
| Auth header | `Bearer <token>` | `Bearer ph_...` or `<jwt>` | ✅ Compatible |
| Reference format | `host/ns/name:tag` | `host/ns/name:tag` | ✅ Compatible |
| Bare ref resolution | `my-agent:tag` → `default/peko/agents/my-agent:tag` | N/A (client-side) | ✅ Compatible |
| Catalog pagination | `?n=20&last=...` | `?n=20&last=...` | ✅ Compatible |
| Search API | `GET /api/v1/search?q=...` | `GET /api/v1/search?q=...` | ✅ Compatible |

### ⚠️ Manifest Format Mismatch — KNOWN ISSUE

`RegistryClient::push_manifest()` serializes `RegistryManifest` directly as JSON and sends it with `Content-Type: application/vnd.oci.image.manifest.v1+json`. However, `RegistryManifest` uses Peko-specific field names (`schema_version`, `size_bytes`, `layer_type`) while OCI expects (`schemaVersion`, `size`, `mediaType`). This causes HTTP 400 `MANIFEST_INVALID` when pushing to PekoHub.

**Impact:**
- Mock registry: ✅ Works (accepts Peko format)
- PekoHub: ❌ Fails (strict OCI validation via Zod schema)

**Required fix:**
Add OCI conversion in `RegistryClient`:
1. `push_manifest()`: Convert `RegistryManifest` → OCI manifest before serializing
2. `fetch_manifest()`: Parse OCI manifest → `RegistryManifest` on pull

**Workaround:**
Use mock registry for CLI E2E tests. PekoHub contract test (`pekohub_contract_test.ps1`) documents this as expected failure.

**Related:**
- `src/registry/mod.rs`: `MANIFEST_PEKO`, `MANIFEST_OCI`, `MANIFEST_DEFAULT`, `MANIFEST_ALL`
- `src/registry/client.rs`: `push_manifest()` line 567, `fetch_manifest()` line 390
- `src/registry/manifest.rs`: `RegistryManifest` struct (Peko format)
