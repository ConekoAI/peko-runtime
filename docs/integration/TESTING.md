# Pekobot Testing — Single Source of Truth

> **Goal:** One model, three one-liners. Unit tests for fast feedback, mock-LLM integration tests as the PR gate, real-LLM integration tests nightly.

---

> **Paths.** All `tests/…`, `src/…`, `Makefile`, `.github/…`, and `e2e_tests/…` paths in this doc are relative to **`peko-runtime/`** (this crate's root), not the monorepo root. Cross-crate paths (`pekohub/backend/…`) are spelled out from the monorepo root.

## 1. TL;DR

| Command | What it runs | Duration | LLM | Docker | When |
|---|---|---|---|---|---|
| `make test` | Unit tests (`cargo test --lib`) | ~30s | No | No | Every save / TDD loop |
| `make test-integration` | All `tests/*.rs` against PekoHub + **mock LLM** | ~3 min | Mock | Yes | Every PR (CI gate) |
| `make test-integration-llm` | Tier above + tests that need a real model | ~5–8 min | Real | Yes | Nightly, or commit tagged `[llm]` |
| `make test-all` | Everything | ~10 min | Real | Yes | Pre-release |

> **Local-dev shortcut:** if you have Node 22 + pnpm + tsx in `../pekohub/backend`, you can run any integration test without Docker — the harness auto-spawns the backend. See §5.

---

## 2. The Two Axes

This codebase used to confuse two different things ("integration" vs "E2E"). We don't anymore:

- **Integration** and **E2E** are the same thing here. The single word **integration** is used throughout.
- The real distinction is **mock LLM vs real LLM**, decided per test using the rule in §3.
- Test scope (protocol check vs full user journey) is orthogonal to the LLM axis. Both kinds of tests live in `tests/*.rs`.

So when you write a new test, ask two questions: *Does it need network/the hub?* (unit vs integration) and *Does it need a real model decision?* (mock-LLM tier vs real-LLM tier).

---

## 3. Mock LLM vs Real LLM Rule

**Default: mock LLM.** Only use a real LLM when you're testing what the model *decides* to do, not what the runtime does with the result.

| Use **mock LLM** when… | Use **real LLM** when… |
|---|---|
| Checking a protocol or transport — push/pull, tunnel WebSocket, SSE proxying | Verifying the model actually decides to call a specific tool |
| Verifying a deterministic CLI flow that ends with a known keyword (`SUCCESS`/`FAIL`) | Multi-turn reasoning, memory continuity, session compaction |
| Asserting plumbing — token routing, streaming chunk shape, error mapping | Provider-specific behavior — smoke-testing `minimax`, `kimi`, etc. |
| Anything where the prompt fully determines the expected response | Anything where "would the model actually do X here?" is the question |

The mock LLM is a deterministic SSE server at [.github/docker/mock-llm/mock_llm_server.py](../../.github/docker/mock-llm/mock_llm_server.py). Each streamed chunk carries both the MiniMax `messages[].content` and the OpenAI `delta.content` shapes, so the same response parses correctly under either the `minimax` or `openai_compatible` provider adapter — tests just set `MOCK_LLM_URL` (mock) or `MINIMAX_API_KEY` (real). The reference implementation of dual-mode selection is [tunnel_e2e.rs:254-261](../../tests/tunnel_e2e.rs#L254-L261).

**Response selection** (first match wins):

1. **`MOCK_LLM_SCRIPT` env** — JSON object `{prompt_substring: response}`. Each value is either a string (echoed as text) or `{"tool_call": {"name": "...", "arguments": "<json-string>"}}`. Lets a test seed scripted dialogs without modifying the mock.
2. **Keyword echo** — `Respond with: <KEYWORD>` (uppercase + underscores + digits) in the prompt returns `<KEYWORD>`. This is the convention every migrated PowerShell test already uses (`SUCCESS`, `ASYNC_SUCCESS`, `TASK_LIST_OK`, etc.).
3. **Tool call** — `Call tool: <name>` (lowercase identifier) in the prompt emits a streamed tool call for `<name>` with empty JSON args.
4. **Default** — `DEFAULT_RESPONSE` env (falls back to `"Peko tunnel works!"`, which is what the long-standing `tunnel_e2e` assertion expects).

Routes served: `POST /v1/text/chatcompletion_v2` (MiniMax path used by `tunnel_e2e`), `POST /v1/chat/completions` and `POST /chat/completions` (OpenAI paths), `GET /health`.

---

## 4. Current Test Inventory

### Unit (`cargo test --lib`)

All `#[test]` and `#[tokio::test]` functions in `src/**`, including:

- **13 subagent integration tests** in [src/agent/tests/subagent_integration_tests.rs](../../src/agent/tests/subagent_integration_tests.rs). They use a `PekoHomeFixture` tempdir and exercise the local `SubagentExecutor` + `SessionManager` — no network.
- **1 JWKS test** at [src/auth/jwt.rs:843](../../src/auth/jwt.rs#L843) (`test_jwks_fetch_from_endpoint`). Spins up a local mock HTTP server on a random port and validates RS256.

Total: full `cargo test --lib` (no test selection needed). No external dependencies.

### Integration (`tests/*.rs`)

| File | Tests | `#[ignore]` | Needs | Container-ready |
|---|---|---|---|---|
| [packaging_integration.rs](../../tests/packaging_integration.rs) | 1 | 1 | PekoHub | Y |
| [pekohub_integration.rs](../../tests/pekohub_integration.rs) | 9 | 9 | PekoHub | Y |
| [registry_integration.rs](../../tests/registry_integration.rs) | 10 | 10 | PekoHub | Y |
| [tunnel_integration.rs](../../tests/tunnel_integration.rs) | 5 | 5 | PekoHub | Y |
| [tunnel_e2e.rs](../../tests/tunnel_e2e.rs) | 1 | 1 | PekoHub + LLM (mock or real) | Y |
| [team_integration.rs](../../tests/team_integration.rs) | 4 | 0 | None (pure Rust) | N/A |
| [extension_packaging.rs](../../tests/extension_packaging.rs) | 6 | 0 | None (pure Rust) | N/A |

**Totals:** 26 hub-gated tests (all `#[ignore]`, un-ignored by `--ignored`) + 10 always-on pure-Rust tests = 36 in `tests/`.

All 5 hub-dependent files share the **same dual-mode `PekohubBackend::start()` harness**: read `PEKOHUB_URL` and reuse a running container, or spawn `node` + `tsx` against `pekohub/backend/tests/fixtures/server.ts`. The `tunnel_*` tests additionally derive `ws_url` from `PEKOHUB_URL` (`http(s)://` → `ws(s)://`, append `/v1/tunnel`).

> **Known issue:** [pekohub_integration::test_pekohub_search_api](../../tests/pekohub_integration.rs#L610) is double-blocked: needs PekoHub *and* has a null-hooks schema validation bug in the search response. Tracked, not blocked on this doc.

### Counts at a glance

- Unit (`cargo test --lib`): everything in `src/**`, no network — includes the 13 subagent and 1 JWKS tests above.
- Integration: 36 tests across 7 files in `tests/`.
- E2E PowerShell scripts in `e2e_tests/`: 91 total (78 live + 13 already under `_archive/`); outside CI, to be dismantled — see §7.

---

## 5. Architecture

```
                        ┌─────────────────────────────────────────┐
                        │         tests/docker/                    │
                        │   docker-compose.integration.yml         │
                        └────────────────┬────────────────────────┘
                                         │
        ┌────────────────────────────────┼──────────────────────────────┐
        │                                │                              │
┌───────▼────────┐              ┌────────▼────────┐           ┌────────▼────────┐
│ pekohub-test    │              │   mock-llm      │           │   test-runner   │
│ ───────────     │              │   ──────────    │           │   ──────────    │
│ Real Fastify app│              │ Python FastAPI  │           │ Rust toolchain  │
│ + PGlite (in-mem│              │ SSE on :8080    │           │ runs cargo test │
│   PostgreSQL)   │              │ MiniMax-compatible│           │ against the     │
│ + Map mock S3   │              │ wire format     │           │ stack via       │
│ + Map mock      │◄─── HTTP ───►│                 │           │ PEKOHUB_URL +   │
│   Meilisearch   │  ws://      │                 │           │ MOCK_LLM_URL    │
│ + ALLOW_DEV_    │  /v1/tunnel │                 │           │                 │
│   AUTH_BYPASS   │              │                 │           │                 │
│ + /test/* eps   │              │                 │           │                 │
└─────────────────┘              └─────────────────┘           └─────────────────┘
```

**Why "test fixture" is not a mock.** `pekohub-test` runs the *real* Fastify app from `pekohub/backend/tests/fixtures/server.ts` — real auth plugin, real tunnel manager, real OCI routes. Only the database (PGlite), storage (in-process `Map`), and search index (in-process `Map`) are swapped out. That gives us maximum confidence in runtime↔hub compatibility without dragging in PostgreSQL, MinIO, or Meilisearch containers.

**Test-fixture endpoints** (used by Rust tests to seed state):

| Endpoint | Purpose |
|---|---|
| `POST /test/create-user` | Creates a real user row in PGlite. Returns `{ id, namespace }`. |
| `POST /test/create-runtime` | Creates a runtime record for tunnel tests. |
| `POST /test/reset` | Truncates DB, clears mock storage + search Maps. Called between tests for isolation. |

### Two ways to run

**Container mode (CI and the recommended default):**
```bash
make docker-up       # starts pekohub-test + mock-llm on tests/docker/
make test-integration
make docker-down
```
Sets `PEKOHUB_URL=http://pekohub-test:3000` and `MOCK_LLM_URL=http://mock-llm:8080` inside the `test-runner` container.

**Local mode (dev loop without Docker):**
```bash
# requires Node 22 + pnpm + tsx in ../pekohub/backend
cargo test --test pekohub_integration -- --ignored
```
With no `PEKOHUB_URL` set, `PekohubBackend::start()` spawns `node` + `tsx` against `pekohub/backend/tests/fixtures/server.ts`, parses `PORT=…` from stdout, waits for `/health`, and kills the child on `Drop`. Override the path via `PEKOHUB_BACKEND_PATH`.

---

## 6. Auth in Tests — No OAuth Needed

The fixture server runs with `ALLOW_DEV_AUTH_BYPASS=true`, but tests still exercise the real auth plugin — they just seed users directly and issue JWTs with the test secret.

```
┌──────────────┐  POST /test/create-user   ┌──────────────┐
│ Test (Rust)  │ ────────────────────────► │ pekohub-test │
│              │ ◄──────────────────────── │              │
│              │   { id, namespace }       │              │
│              │                           │              │
│              │   sign JWT with           │              │
│              │   PEKOHUB_JWT_SECRET      │              │
│              │                           │              │
│              │   POST /v1/auth/api-keys  │              │
│              │   Authorization: Bearer.. │              │
│              │ ────────────────────────► │              │
│              │ ◄──────────────────────── │              │
│              │   { key: "ph_abc…" }      │              │
│              │                           │              │
│              │   OCI Push/Pull           │              │
│              │   Authorization: Bearer.. │              │
│              │ ────────────────────────► │              │
└──────────────┘                           └──────────────┘
```

Reference JWT helper (lives in [tunnel_e2e.rs](../../tests/tunnel_e2e.rs)):

```rust
const PEKOHUB_JWT_SECRET: &str = "test-secret-key-that-is-32-chars-long!!";

fn generate_jwt(user_id: i64, namespace: &str) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};
    #[derive(serde::Serialize)]
    struct Claims { sub: String, namespace: String, iat: u64 }

    let claims = Claims {
        sub: user_id.to_string(),
        namespace: namespace.to_string(),
        iat: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
    };
    encode(&Header::default(), &claims,
           &EncodingKey::from_secret(PEKOHUB_JWT_SECRET.as_bytes())).unwrap()
}
```

Why no full OAuth flow is needed:

| Concern | Reality |
|---|---|
| "We need to test OAuth" | OAuth is provider-side (GitHub/Google). The runtime only redirects to the hub — test the redirect URL, not the IdP. |
| "We need real user accounts" | `/test/create-user` creates real PGlite rows with real IDs, namespaces, and API keys. |
| "We need API-key auth" | Create a key via `/v1/auth/api-keys`, use it for OCI ops. Full path covered. |
| "Tunnel auth needs OAuth" | Tunnel uses DID Ed25519 signatures, not OAuth. Already covered in `tunnel_integration.rs`. |

---

## 7. Migration Roadmap: `e2e_tests/` → `tests/`

`peko-runtime/e2e_tests/` holds 66 PowerShell scripts that live outside CI, overlap heavily with `tests/*.rs`, and still reference a deleted Python mock and a non-existent top-level runner. The end-goal is to dismantle the folder. This roadmap is the spec for the follow-up PRs — none of it is done in this doc-consolidation PR.

### Phase A — Delete redundant (one PR)

Scripts whose coverage already exists in Rust integration tests, plus the orphaned helpers and fixtures that fall out with them:

| PowerShell script(s) | Disposition | Reason |
|---|---|---|
| `packaging/registry_push_pull.ps1`, `registry_layer_dedup.ps1`, `team_registry_dedup.ps1`, `team_registry_snapshot.ps1`, `agent_registry_lifecycle.ps1`, `agent_snapshot_memory.ps1`, `cross_platform_agent_share.ps1` | Delete | Covered by `tests/registry_integration.rs` (10) + `tests/pekohub_integration.rs` (9) |
| `packaging/team_export_import.ps1`, `team_full_lifecycle.ps1`, `team_snapshot_with_sessions.ps1`, `team_subteam_hierarchy.ps1`, `team_with_extensions.ps1` | Delete | Covered by `tests/team_integration.rs` (4) — the 4 unnamed `team_*.ps1` cover the same export/import/sessions/hierarchy/extensions surface |
| `packaging/agent_build_export_import.ps1` | Delete | Covered by `tests/packaging_integration.rs` (1) |
| `packaging/extension_bundle_registry.ps1` | Delete | Covered by `tests/extension_packaging.rs` (6) |
| `packaging/pekohub_contract_test.ps1` | Delete | Covered by `tests/pekohub_integration.rs` (9) |
| `packaging/debug_team.ps1` | Delete | One-off debug dump, no header, no asserts |
| `packaging/test_all.ps1` | Delete | Top-level packaging runner — end-state has no runner script |
| `packaging/RegistryTestHelpers.ps1` | Delete | Helper module; all 4 dependents (`pekohub_contract_test`, `registry_layer_dedup`, `registry_push_pull`, and itself) are being deleted in this PR |
| `packaging/*.agent` (7 files: `agent-a`, `agent-b`, `cross-agent`, `lifecycle-agent`, `memory-agent`, `my-agent`, `registry-test-agent`) | Delete | Fixture files used only by the scripts being deleted |

Also delete: `e2e_tests/cron.db` (committed SQLite artifact), and the stale `e2e_tests/README.md` references to `run_all_tests.ps1` and `mock_registry/`.

> Dead Makefile + workflow references that should land in the same PR:
> - [Makefile:67-75](../../Makefile#L67-L75) `test-full-e2e` target points at a nonexistent `integration-tests/` dir.
> - [.github/workflows/integration.yml:216-241](../../.github/workflows/integration.yml#L216-L241) `full-e2e` job runs `./run_e2e_tests.sh` in the same nonexistent dir on the nightly schedule — silently failing today.

### Phase B — Migrate CLI flows to Rust (3–5 PRs)

Scripts that exercise CLI surfaces with no Rust equivalent. Each becomes one `tests/cli_<area>.rs` file using the `PekohubBackend` harness for hub-touching flows and the mock LLM for deterministic chat:

| PowerShell dir | New Rust test file | Tier |
|---|---|---|
| `e2e_tests/send/` | `tests/cli_send.rs` | mock-LLM |
| `e2e_tests/session/` | `tests/cli_session.rs` | mock-LLM |
| `e2e_tests/cron/` | `tests/cli_cron.rs` | mock-LLM |
| `e2e_tests/agent/`, `e2e_tests/team/`, `e2e_tests/config/` | `tests/cli_basics.rs` | mock-LLM |
| `e2e_tests/extensions/` | `tests/cli_extensions.rs` | real-LLM (tool calls) |
| `e2e_tests/compaction/` | `tests/cli_compaction.rs` | real-LLM (reasoning) |
| `e2e_tests/a2a/`, `e2e_tests/subagent/`, `e2e_tests/tools/` | `tests/cli_a2a.rs`, `tests/cli_subagent.rs`, `tests/cli_tools.rs` | real-LLM (tool-call decisions) |
| `e2e_tests/providers/` | `tests/cli_providers.rs` | real-LLM (gated by `MINIMAX_API_KEY` / `KIMI_API_KEY`) |

### Phase C — Mock-LLM enhancement (✅ landed; unblocks Phase B mock-tier work)

[.github/docker/mock-llm/mock_llm_server.py](../../.github/docker/mock-llm/mock_llm_server.py) supports:

- **`DEFAULT_RESPONSE` env** — overrides the fallback text (was previously hardcoded to `"Peko tunnel works!"`).
- **Keyword echo** — `Respond with: <KEYWORD>` in the prompt returns `<KEYWORD>`. Matches the convention the PowerShell scripts already use (`SUCCESS`, `FAIL`, `MEMORY_SUCCESS`).
- **Tool-call responses** — `Call tool: <name>` in the prompt returns a streamed `tool_calls` array for `<name>` with empty JSON args.
- **`MOCK_LLM_SCRIPT` env** — JSON map of prompt-substring → response (string or `{tool_call: {name, arguments}}`), so tests can seed complex scripted dialogs without modifying the mock.

Full spec lives in §3 above. This unblocks moving ~30 LLM-required PowerShell tests into the mock-LLM tier rather than the real-LLM tier.

### Phase D — Phase-7 user-journey scenarios (S1–S5)

The five end-to-end scenarios become `tests/scenarios/s{1..5}_*.rs`. They run in the real-LLM tier because they exercise full user journeys end to end.

| Scenario | Rust file | Description |
|---|---|---|
| S1: Publish & Discover | `s1_publish_discover.rs` | Push agent → search → pull |
| S2: Team Collaboration | `s2_team_collaboration.rs` | Export team → import team → verify |
| S3: Versioned Extension | `s3_versioned_extension.rs` | Publish ext → install → verify hooks |
| S4: Auth Flow | `s4_auth_flow.rs` | Create user → API key → push → verify ownership |
| S5: Cross-Platform Share | `s5_cross_platform.rs` | Export on platform A → import on B |

> **No `integration-tests/` directory.** The old docs referenced one that never existed on disk. Scenarios live under `tests/scenarios/` and are picked up by the same `cargo test --test '*' -- --ignored` glob — there is no separate runner script and no separate compose stack.

### Phase E — Archive the folder

Once Phases A–D land:
- Move any remaining one-off scripts into `e2e_tests/_archive/` (which already exists for legacy CAP tests), or delete outright.
- Remove `e2e_tests/` from the path filters in `.github/workflows/integration.yml`.
- Delete `e2e_tests/README.md` and the top-level `reset.ps1`.

---

## 8. End-State Makefile

This is what `make help` should print once Phase A of the migration lands. Today's Makefile has more granular per-test-file targets; those collapse into these four:

```makefile
# Fast feedback — no Docker, no LLM
test:                  cargo test --lib

# Tier 1 — PR gate: Docker + PekoHub + mock LLM
test-integration:      docker-up && \
                       cargo test --test '*' -- --ignored
                       # env: PEKOHUB_URL, MOCK_LLM_URL set
                       # env: MINIMAX_API_KEY unset

# Tier 2 — nightly + [llm] commit tag: adds real-LLM tests
test-integration-llm:  docker-up && \
                       cargo test --test '*' -- --ignored
                       # env: PEKOHUB_URL, MINIMAX_API_KEY set
                       # tests opt in via runtime check

# Everything
test-all:              test && test-integration && test-integration-llm
```

**How tests opt into the real-LLM tier.** Use a runtime skip at the top of the test:

```rust
#[tokio::test]
#[ignore = "real LLM required (set MINIMAX_API_KEY)"]
async fn test_thing_that_needs_real_model() {
    if std::env::var("MINIMAX_API_KEY").is_err() { return; }
    // … test body …
}
```

Tests that work with either LLM use whichever is set — mock-first. The reference is the dual-mode logic in [tunnel_e2e.rs:254-261](../../tests/tunnel_e2e.rs#L254-L261).

---

## 9. End-State CI

The current `.github/workflows/integration.yml` runs 6 parallel jobs (lib-tests + 5 per-test-file). The end state collapses to 3:

| Job | Command | When | Needs |
|---|---|---|---|
| `unit` | `make test` | Every PR | — |
| `integration` | `make test-integration` | Every PR | `unit` |
| `integration-llm` | `make test-integration-llm` | Nightly + `[llm]` in commit msg | `integration` |

`MINIMAX_API_KEY` lives as a repo secret. Only `integration-llm` reads it. The `integration` job runs with the secret unset — that's how we guarantee the mock-LLM-tier tests stay mock-only and don't silently leak to the real provider.

---

## 10. Local Dev Workflow

```bash
# 1. TDD loop — fast feedback, no Docker
cargo test --lib path::to::specific::test -- --nocapture

# 2. Push a contract change — verify the hub still likes it
make docker-up
cargo test --test pekohub_integration -- --ignored
make docker-down

# 3. About to open a PR — run the gate locally
make test-integration

# 4. Touched something that affects model behavior — run the real-LLM tier
MINIMAX_API_KEY=sk-… make test-integration-llm
```

**Without Docker** (local mode): set `PEKOHUB_BACKEND_PATH=/path/to/pekohub/backend` and the harness will spawn Node + tsx for you. Useful if you're hacking on both sides at once.

---

## 11. Doc History (for the archaeologists)

This file consolidates three prior docs that were rotated out on 2026-06-12:

- `INTEGRATION_TEST_PLAN.md` — the original plan describing a Python mock registry that has since been deleted.
- `CONTAINERIZED_E2E_PLAN.md` — the migration plan that deleted that Python mock and unified everything onto `PekohubBackend`.
- `TESTING_STRATEGY.md` — a later cheat sheet that already contradicted the older two.

All three were deleted in the same commit that created this file. If you need the historical phase-tracker / status checklists, recover them from git history.
