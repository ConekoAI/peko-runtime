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

The mock LLM is a deterministic SSE server at [.github/docker/mock-llm/mock_llm_server.py](../../.github/docker/mock-llm/mock_llm_server.py). Each streamed chunk carries both the MiniMax `messages[].content` and the OpenAI `delta.content` shapes, so the same response parses correctly under either the `minimax` or `openai_compatible` provider adapter — tests just set `MOCK_LLM_URL` (mock) or `MINIMAX_API_KEY` (real). The reference implementation of dual-mode selection is [tunnel_e2e.rs:63-76](../../tests/tunnel_e2e.rs#L63-L76).

**Response selection** (first match wins):

1. **`MOCK_LLM_SCRIPT` env** — JSON object `{prompt_substring: response}`. Each value is either a string (echoed as text) or `{"tool_call": {"name": "...", "arguments": "<json-string>"}}`. Lets a test seed scripted dialogs without modifying the mock.
   - **Sequence** — if a value is a *list* of response specs (strings, `tool_call` dicts, or a mix), the i-th time the substring matches returns the i-th element. After the list is exhausted, the last element is returned for every subsequent match, so a test scripting N turns doesn't crash on a stray N+1 call. Counters are **per-substring** (keyed by the `prompt_substring` key), so two dialogs keyed on different substrings don't interfere. Reset by POSTing to `/_test/configure` with a new `MOCK_LLM_SCRIPT` body (see below). This is the feature that moves multi-turn LLM decisions (tool-call → result → reasoning → keyword) into the mock tier. Reference: [tests/mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs).
2. **Keyword echo** — `Respond with: <KEYWORD>` (uppercase + underscores + digits) in the prompt returns `<KEYWORD>`. This is the convention every migrated PowerShell test already uses (`SUCCESS`, `ASYNC_SUCCESS`, `TASK_LIST_OK`, etc.).
3. **Tool call** — `Call tool: <name>` (lowercase identifier) in the prompt emits a streamed tool call for `<name>` with empty JSON args.
4. **Default** — `DEFAULT_RESPONSE` env (falls back to `"Peko tunnel works!"`, which is what the long-standing `tunnel_e2e` assertion expects).

**Test-only `/_test/configure` endpoint.** POST a JSON body whose keys mirror the env vars (e.g. `{"MOCK_LLM_SCRIPT": "{\"turn\":[\"r1\",\"r2\"]}"}`) and the server swaps the env var in place and clears the per-substring counter map. This is how `tests/mock_llm_sequence.rs` and other future sequence-driven tests get a deterministic baseline without restarting the container.

Routes served: `POST /v1/text/chatcompletion_v2` (MiniMax path used by `tunnel_e2e`), `POST /v1/chat/completions` and `POST /chat/completions` (OpenAI paths), `POST /_test/configure` (test-only), `GET /health`.

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
| [cli_send.rs](../../tests/cli_send.rs) | 7 | 7 | PekoHub + mock LLM | Y |
| [cli_session.rs](../../tests/cli_session.rs) | 9 | 9 | PekoHub + mock LLM | Y |
| [cli_basics.rs](../../tests/cli_basics.rs) | 14 | 8 | Mixed (6 offline, 8 need PekoHub + mock LLM) | Partial |
| [cli_cron.rs](../../tests/cli_cron.rs) | 18 | 18 | PekoHub + mock LLM (2 are agent-tool multi-turn, §3 Sequence) | Y |
| [cli_subagent.rs](../../tests/cli_subagent.rs) | 7 | 7 | PekoHub + mock LLM (all multi-turn, §3 Sequence, `#[serial]`) | Y |
| [cli_tools.rs](../../tests/cli_tools.rs) | 6 | 6 | PekoHub + mock LLM (single-turn, §3 Sequence, `#[serial]`) | Y |
| [cli_compaction.rs](../../tests/cli_compaction.rs) | 8 | 8 | PekoHub + mock LLM (multi-turn setup + `--dry-run --json` shape; T1 smoke + T1 multi + T2 actual + T3 cache + T4 usable + T5 custom-instruction + T6 incremental + extension hook, all `#[serial]`) | Y |
| [cli_extensions.rs](../../tests/cli_extensions.rs) | 10 | 10 | PekoHub + mock LLM (L1 install/list/info/enable/disable/uninstall lifecycle, NOT `#[serial]` — no LLM use) | Y |
| [mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs) | 6 | 6 | mock LLM (sequence feature, §3) | Y |
| [cli_providers.rs](../../tests/cli_providers.rs) | 2 | 2 | real LLM (minimax + kimi smoke; needs `MINIMAX_API_KEY` / `KIMI_API_KEY`; tests early-return when unset so mock tier still passes) | Y |
| [cli_a2a.rs](../../tests/cli_a2a.rs) | 9 | 9 | real LLM (`a2a_send` blocking + isolation flows; needs `MINIMAX_API_KEY`; 2-LLM-call flows; tests early-return when unset so mock tier still passes) | Y |
| [scenarios/s1_local_agent_with_extensions.rs](../../tests/scenarios/s1_local_agent_with_extensions.rs) | 6 | 6 | mock LLM (Phase D slice 1: create agent + install skill + enable on agent + chat locally; 6 lifecycle scenarios) | Y |

**Totals:** 116 hub- or mock-LLM-gated tests (all `#[ignore]`, un-ignored by `--include-ignored`) + 16 always-on tests (10 pure Rust + 6 offline CLI) = 132 in `tests/`.

The 5 files that exercise the hub directly — [packaging_integration.rs](../../tests/packaging_integration.rs), [pekohub_integration.rs](../../tests/pekohub_integration.rs), [registry_integration.rs](../../tests/registry_integration.rs), [tunnel_integration.rs](../../tests/tunnel_integration.rs), [tunnel_e2e.rs](../../tests/tunnel_e2e.rs) — share the **same dual-mode `PekohubBackend::start()` harness** in [tests/common/harness.rs](../../tests/common/harness.rs): read `PEKOHUB_URL` and reuse a running container, or spawn `node` + `tsx` against `pekohub/backend/tests/fixtures/server.ts`. The `tunnel_*` tests additionally derive `ws_url` from `PEKOHUB_URL` (`http(s)://` → `ws(s)://`, append `/v1/tunnel`). The 10 `cli_*` files ([cli_send.rs](../../tests/cli_send.rs), [cli_session.rs](../../tests/cli_session.rs), [cli_basics.rs](../../tests/cli_basics.rs), [cli_cron.rs](../../tests/cli_cron.rs), [cli_subagent.rs](../../tests/cli_subagent.rs), [cli_tools.rs](../../tests/cli_tools.rs), [cli_compaction.rs](../../tests/cli_compaction.rs), [cli_extensions.rs](../../tests/cli_extensions.rs), [cli_providers.rs](../../tests/cli_providers.rs), [cli_a2a.rs](../../tests/cli_a2a.rs) — note: `cli_compaction.rs` covers the full 6 PS scenarios + the extension test, `cli_extensions.rs` covers the L1 install/list/info/enable/disable/uninstall surface, `cli_providers.rs` covers the real-LLM minimax + kimi smoke flows, and `cli_a2a.rs` covers the real-LLM `a2a_send` blocking + async + isolation flows, see §7) also need the hub but use a different pattern: they spawn the `peko` daemon as a subprocess against the same stack and let the daemon do the hub calls. [mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs) does not need PekoHub — it talks to the mock directly (plus the peko daemon for the three-call flow) — but ships in the same docker-up workflow for the dev-loop convenience.

> **Known issue:** [pekohub_integration::test_pekohub_search_api](../../tests/pekohub_integration.rs#L466) is double-blocked: needs PekoHub *and* has a null-hooks schema validation bug in the search response. Tracked, not blocked on this doc.

### Counts at a glance

- Unit (`cargo test --lib`): everything in `src/**`, no network — includes the 13 subagent and 1 JWKS tests above.
- Integration: 132 tests across 18 files in `tests/`.
- E2E PowerShell scripts in `e2e_tests/`: 58 total (45 live + 13 already under `_archive/`); outside CI, to be dismantled — see §7. The Phase B legs that have landed (`send/`, `session/`, `agent/`, `team/`, `config/`, `cron/`, `subagent/`, `tools/built-in/`, `compaction/{cli,extension}`, `extensions/`, `providers/`, `a2a/`) move the PS scripts into a "redundant" state but keep them on disk until Phase E finalizes the cleanup. Only `compaction_auto.ps1` (real-LLM tier), `compaction_all.ps1` (the meta-runner), and `a2a_async.ps1` (deferred — async not wired in production) are still pending.

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
┌───────▼─────────┐           ┌───────────▼──────────┐        ┌──────────▼─────────┐
│  pekohub-test   │           │      mock-llm        │        │  cargo on host     │
│ ───────────     │           │   ──────────         │        │  ─────────────     │
│ Real Fastify    │           │ Python FastAPI       │        │ Rust toolchain     │
│   + PGlite      │           │ SSE on :8080         │        │ cargo test runs    │
│   + Map mock S3 │◄── HTTP ─►│ MiniMax-compatible   │◄──HTTP─│ against the stack  │
│   + Map mock    │   ws://   │   wire format        │        │ via PEKOHUB_URL +  │
│     Meilisearch │  /v1/    │                      │        │   MOCK_LLM_URL     │
│   + /test/* eps │  tunnel  │                      │        │ (the GitHub runner │
│   + ALLOW_DEV_  │          │                      │        │  or the dev box)   │
│     AUTH_BYPASS │          │                      │        │                    │
└─────────────────┘          └──────────────────────┘        └────────────────────┘
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
Sets `PEKOHUB_URL=http://pekohub-test:3000` and `MOCK_LLM_URL=http://mock-llm:8080` in the `make test-integration` recipe's env, which cargo inherits as the host-side process.

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

Reference JWT helper (lives in [tests/common/auth.rs](../../tests/common/auth.rs), re-exported as `common::generate_jwt`):

```rust
pub const PEKOHUB_JWT_SECRET: &str = "test-secret-key-that-is-32-chars-long!!";

pub fn generate_jwt(user_id: i64, namespace: &str) -> String {
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

`peko-runtime/e2e_tests/` still holds 45 live PowerShell scripts that live outside CI, overlap heavily with `tests/*.rs`, and still reference a deleted Python mock. The end-goal is to dismantle the folder. Phases A and the cli_send / cli_session / cli_basics / cli_cron / cli_subagent / cli_extensions / cli_providers legs of Phase B have already landed; cli_a2a partially landed (9 tests, async deferred).

### Phase A — Delete redundant (✅ landed)

`e2e_tests/packaging/` (and its helpers, runner, and 7 fixture `.agent` files) was deleted. Its coverage now lives in `tests/registry_integration.rs`, `tests/pekohub_integration.rs`, `tests/packaging_integration.rs`, `tests/team_integration.rs`, and `tests/extension_packaging.rs`. The redundant agent fixture files were rehomed to the peko-runtime root for use by other tests; the redundant `e2e_tests/cron.db` and stale README references to `run_all_tests.ps1` and `mock_registry/` were also removed. The dead `test-full-e2e` Makefile target and the `full-e2e` GitHub workflow job (both pointing at a nonexistent `integration-tests/` dir) were removed at the same time.

### Phase B — Migrate CLI flows to Rust (3–5 PRs)

Scripts that exercise CLI surfaces with no Rust equivalent. Each becomes one `tests/cli_<area>.rs` file using the `PekohubBackend` harness for hub-touching flows and the mock LLM for deterministic chat. Tests that drive the `peko` daemon as a subprocess live under [tests/common/subprocess.rs](../../tests/common/subprocess.rs) and [tests/common/cli.rs](../../tests/common/cli.rs).

| PowerShell dir | New Rust test file | Tier | Status |
|---|---|---|---|
| `e2e_tests/send/` | `tests/cli_send.rs` | mock-LLM | ✅ Migrated (7 tests) |
| `e2e_tests/session/` | `tests/cli_session.rs` | mock-LLM | ✅ Migrated (9 tests) |
| `e2e_tests/agent/`, `e2e_tests/team/`, `e2e_tests/config/` | `tests/cli_basics.rs` | mixed | ✅ Migrated (14 tests: 6 offline, 8 mock-LLM) |
| `e2e_tests/cron/` | `tests/cli_cron.rs` | mock-LLM | ✅ Migrated (18 tests; of which 2 are agent-tool multi-turn via §3 Sequence — see coverage gap below) |
| `e2e_tests/extensions/` | `tests/cli_extensions.rs` | mock-LLM (L1 install/list/info/enable/disable/uninstall lifecycle, NOT `#[serial]` — no LLM use) | ✅ Migrated (10 tests; L2+L3 deferred to follow-up — see coverage gap below) |
| `e2e_tests/compaction/{cli,extension}` | `tests/cli_compaction.rs` | mock-LLM (multi-turn setup + `--dry-run --json`; T1 smoke + T1 multi [Issue 030 regression] + T2 actual + T3 cache + T4 usable + T5 custom-instruction + T6 incremental + extension hook, all `#[serial]`) | ✅ Migrated (8 tests; `compaction_auto.ps1` and `compaction_all.ps1` still pending — see coverage gap below) |
| `e2e_tests/a2a/` | `tests/cli_a2a.rs` | real-LLM (2-LLM-call flows via `a2a_send`; needs `MINIMAX_API_KEY`; tests early-return when unset so mock tier still passes) | ✅ Migrated (9 tests: 4 blocking + 5 isolation; `a2a_async.ps1` is deferred — the `a2a_send` tool's schema doesn't expose `_async`, see coverage gap below) |
| `e2e_tests/tools/built-in/` | `tests/cli_tools.rs` | mock-LLM (single tool_call per test, §3 Sequence) | ✅ Migrated (6 tests, one per built-in tool: glob, grep, read_file, write_file, str_replace_file, shell. See coverage gap below for the 4 top-level PS scripts deferred.) |
| `e2e_tests/subagent/` | `tests/cli_subagent.rs` | mock-LLM (tool-call decisions via Sequence) | ✅ Migrated (7 tests covering parent-side blocking path, isolated mode, labeled mode, inline-result, 2-level nesting, depth-limit smoke, shared/isolated context; `subagent_async.ps1` + `subagent_status_list.ps1` deferred — see coverage gap below) |
| `e2e_tests/providers/` | `tests/cli_providers.rs` | real-LLM (gated by `MINIMAX_API_KEY` / `KIMI_API_KEY`; tests early-return when unset so mock tier still passes) | ✅ Migrated (2 tests: `cli_providers_minimax_smoke` + `cli_providers_kimi_smoke`) |

#### Phase B coverage gap — `e2e_tests/cron/cron_agent_tool.ps1`

**Status:** ✅ Migrated to `tests/cli_cron.rs` as 2 new mock-LLM-tier tests (`cron_agent_tool_schedules_and_lists_job`, `cron_agent_tool_schedules_and_cancels_job`). PS TEST 3 (wait 3:30 for execution) is intentionally not migrated — too slow for CI; the scheduling and cancellation sides are what exercise the agent-tool chain, and execution itself is covered by the daemon-CRUD tests in the same file.

**What the migrated tests cover:** the agent uses its built-in `cron` tool (sub-commands `at` / `list` / `cancel`) to self-schedule, self-list, and self-cancel jobs. The schedule test verifies the resulting job is visible to the daemon (`peko cron list` shows it); the cancel test verifies an agent-cancelled job disappears from `peko cron list`.

**How the mock drives the multi-turn dialog:** §3 *Sequence* lets a `MOCK_LLM_SCRIPT` list value carry one response per LLM call. The schedule test scripts 3 elements: `tool_call(cron, at, ...)` → `tool_call(cron, list)` → text `TOOL_SUCCESS`. The cancel test scripts 4: `at` → `list` → `cancel` (using `cancel_label` so the mock doesn't need a pre-known `job_id`) → text `CANCEL_SUCCESS`. Each turn's tool_call's `function.arguments` is a JSON-encoded string with the structured `cron` args the runtime's `CronTool` dispatcher needs (`sub_command`, `time`, `label`, `task`, `agent_id` for `at`; `sub_command: "cancel"`, `cancel_label: "..."` for cancel).

**Reference for the mock-side syntax:** [tests/mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs) (`mock_llm_script_list_supports_mixed_text_and_tool_call`); the helper that POSTs to `/_test/configure` lives in [tests/common/mock_configure.rs](../../tests/common/mock_configure.rs).

#### Phase B coverage gap — `e2e_tests/subagent/*.ps1` (deeper scenarios)

**Status:** Mostly migrated. `tests/cli_subagent.rs` ships 7 tests that exercise the `agent_spawn` blocking path end-to-end through the peko daemon:

| Rust test | Maps to PS sub-test | What it asserts |
|---|---|---|
| `subagent_blocking_t1_write_file` | `subagent_blocking.ps1` T1 | parent spawns child; child writes a file; parent reports `BLOCKING_SUCCESS`; child wrote the file at the expected path with the expected content |
| `subagent_blocking_t2_isolated` | `subagent_blocking.ps1` T2 | same as T1 with `isolated: true` |
| `subagent_blocking_t4_inline_read` | `subagent_blocking.ps1` T4 | parent writes a file, spawns child, child reads it and returns the content inline; parent reports `INLINE_SUCCESS` |
| `subagent_nesting_t1_depth2_writes_file` | `subagent_nesting.ps1` T1 | 3-level chain (parent → child-A → grandchild-B); grandchild writes a file that the test reads back |
| `subagent_nesting_t2_depth_limit` | `subagent_nesting.ps1` T2 (smoke) | 3-level chain dispatches through the runtime; the depth-limit code path itself is unit-tested in `src/agent/tests/subagent_integration_tests.rs::test_depth_limit_enforcement` with `ExecutionConfig { max_depth: 2 }` |
| `subagent_isolation_t1_shared_workspace` | `subagent_isolation.ps1` T1 | parent writes a file, spawns a non-isolated child, child reads it back; parent reports `SHARED_OK` |
| `subagent_isolation_t2_isolated_writes_file` | `subagent_isolation.ps1` T2 | parent spawns `isolated: true` child, child writes a file that the test reads back |

The child-side file write assertions are real end-to-end reads: the test creates a `PekoCli` with an isolated `HOME`, the daemon writes the file under `<peko_dir>/data/workspaces/`, and the test reads it back. Proving the file landed at the expected path is the proof that the blocking `agent_spawn` actually drove the child through the full `AgenticLoop` and that the child's `write_file` tool call dispatched through the daemon's `ToolRuntime` to disk.

**What is deferred (to a follow-up PR):** the 3rd sub-test from `subagent_blocking.ps1` (`T3_shell` — cross-platform `echo foo > bar` via the `shell` tool) is dropped because the platform-portable shell semantics (cmd.exe vs /bin/sh quoting, redirects) are not stable enough for a CI assertion. The `subagent_nesting.ps1` T2 depth-limit smoke covers the dispatch plumbing only; the hard limit itself is enforced at `src/agent/subagent_executor.rs:230-235` and exercised in the in-process unit tests, which give a more deterministic assertion than a 4-level mock chain.

**What is also deferred:** `subagent_async.ps1` (4 sub-tests) and `subagent_status_list.ps1` (4 sub-tests) require driving the `task` tool (status/list/cancel) and `agent_spawn` in `_async: true` mode. Both need to look up `task_id` in the in-process `AsyncTaskRegistry` (in `src/extension/async_exec/executor/`), which is built per-daemon and is not currently addressable from a Rust integration test that goes through the `peko` CLI. PR-3 path: add a test-only `peko subagent list --json` (or `peko task list --json`) CLI subcommand that reads the in-process `AsyncTaskRegistry` and dumps it as JSON, then write `cli_subagent.rs` tests that drive the parent's mock to emit a `task` tool_call and assert on the CLI output. Same shape as the `peko cron list --json` round-trip in `cli_cron.rs`.

**A non-obvious gotcha this migration uncovered:** the per-agent `[extensions] enabled` whitelist is consulted in TWO places with different comparison semantics. The per-agent tool filter at `src/agent/agent.rs:121-135` matches against `tool.name()` (e.g. `"agent_spawn"`). The runtime dispatcher at `src/extension/core/tool_registry.rs:60-63` matches against the tool's owning canonical extension ID (e.g. `"builtin:tool:agent_spawn"`). A test agent's whitelist must therefore contain BOTH forms of every enabled tool, otherwise the per-agent init drops the tool (or the dispatcher marks it disabled at execution time) and the parent's tool call gets `"Error: Tool 'agent_spawn' is currently disabled..."`. The `write_subagent_agent` helper in `tests/cli_subagent.rs` ships both forms.

#### Phase B coverage gap — `e2e_tests/tools/*.ps1` (top-level scripts deferred)

**Status:** Partially migrated. `tests/cli_tools.rs` ships 6 tests that exercise the 6 built-in tools (`shell`, `read_file`, `write_file`, `glob`, `grep`, `str_replace_file`) end-to-end through the peko daemon. Each test scripts one `tool_call(<tool>, …)` via the mock LLM, asserts the tool dispatched, and (for `write_file` / `str_replace_file`) verifies the file-side effect on disk. The per-tool `Tool` impls are unit-tested in `src/tools/builtin/fs/*.rs` and `src/tools/builtin/shell.rs`; the CLI integration concern is that the tool call wires through the agent loop and the dispatcher. The 2–3 sub-tests per PS script (e.g. `glob` T1 `*.py`, T2 `*.rs`, T3 `**/*.rs`) collapse to a single representative case per tool in the Rust file because the shape is identical and the per-tool filtering is unit-tested separately.

**What is deferred (the 4 top-level PS scripts):**

| PS script | Sub-tests | Why deferred |
|---|---|---|
| `tool_all.ps1` | (meta-runner) | Replaced by `cargo test --test cli_tools`. |
| `tool_async.ps1` | 7 sub-tests | Exercises `_async: true` plumbing against the in-process `AsyncTaskRegistry` (`src/extension/async_exec/executor/executor.rs:43-76`). The PS file itself documents many sub-tests as `"EXPECTED FAIL (feature may be stubbed)"` — the framework-level async support is not yet fully wired. |
| `tool_timeout.ps1` | 1 test | Depends on the LLM reasoning about `_timeout: 3` semantics (the LLM has to choose to pass `_timeout: 3` to the `shell` tool). Real-LLM tier. |
| `tool_update_mid_session.ps1` | 4 sub-tests | Tests ADR-019 mid-session `peko ext enable/disable --target` (the system-prompt / provider-tool-schema / dispatcher-allowed-set triad). The daemon-side enable/disable is a config-level test; the LLM-side reasoning is real-LLM tier. Better as a dedicated ADR-019 PR that exercises the mid-session plumbing directly without the LLM reasoning loop. |

These four scripts stay in `e2e_tests/tools/` until those features are wired enough to mock. The 6 `built-in/*.ps1` files that this PR migrates stay in place (now redundant with `tests/cli_tools.rs`) and will be deleted in Phase E, consistent with how the `cron/`, `subagent/`, `agent/`, etc. migrations handled their now-redundant PS scripts.

**A non-obvious gotcha this migration (independently) re-confirmed:** the same per-agent `[extensions] enabled` whitelist gotcha from `cli_subagent.rs` applies to the 6 built-in tools. `write_builtin_agent` in `tests/cli_tools.rs` lists BOTH the bare tool names (`"shell"`, `"read_file"`, `"write_file"`, `"glob"`, `"grep"`, `"str_replace_file"`) AND the canonical `builtin:tool:…` IDs. Without both forms, the per-agent init drops the tool or the dispatcher marks it disabled, and the parent's `tool_call` returns `"Error: Tool 'write_file' is currently disabled..."`.

#### Phase B coverage gap — `e2e_tests/compaction/{cli,extension}.ps1` (now migrated) and `compaction_auto.ps1` (still deferred, real-LLM tier)

**Status:** Migrated (8 tests). `tests/cli_compaction.rs` now ships the full PS-scenario coverage for `compaction_cli.ps1` and `compaction_extension.ps1`:

| Rust test | Maps to PS sub-test | What it asserts |
|---|---|---|
| `cli_compact_dry_run_json_reports_metadata` | `compaction_cli.ps1` T1 (smoke) | `peko session compact --dry-run --json` returns `success: true`, `dry_run: true`, and includes the `DryRunReport` fields (`estimated_tokens`, `context_window`, `percent`, `message_count`, `messages_to_compact`). Locked by the wire-format fix in [`src/commands/session.rs:286-356`](../../pekobot/peko-runtime/src/commands/session.rs#L286-L356). |
| `cli_compact_dry_run_json_reports_message_counts_after_multi_turn` | `compaction_cli.ps1` T1 (full) — also Issue 030 regression | After 6 mock-LLM-driven `peko send` rounds, dry-run reports `message_count >= 6` and `messages_to_compact >= 1`. Regression for [Issue 030](../../issues/closed/030-cli-compaction-message-count-zero.md): the original dry-run response reused the real-compaction `SessionCompacted` wire shape and hard-coded `messages_compacted: 0`, which the CLI re-mapped to both `message_count` and `messages_to_compact`. Fixed in [`src/ipc/packet.rs:1054-1063`](../../pekobot/peko-runtime/src/ipc/packet.rs#L1054-L1063) by introducing a dedicated `ResponsePacket::SessionCompactDryRun` variant. |
| `cli_compact_actual_records_compaction_in_jsonl` | `compaction_cli.ps1` T2 | `peko session compact --json` returns `success: true`, `messages_compacted >= 1`, `tokens_before > tokens_after`. The session JSONL contains exactly 1 `compaction` event with `compaction_number: 1` and a non-empty `summary`. |
| `cli_compact_updates_context_cache` | `compaction_cli.ps1` T3 | After a real compact, `<session_id>.context.cache` exists and contains a system message with a "Conversation Summary" / "Compacted" marker. |
| `cli_compact_session_usable_after_compaction` | `compaction_cli.ps1` T4 | After a real compact, one more `peko send` round completes: the response contains the `POST_COMPACT_SUCCESS` sentinel and `<peko_dir>/data/workspaces/post_compact.txt` contains the file content. |
| `cli_compact_custom_instruction_in_summary` | `compaction_cli.ps1` T5 | A compact with `--instruction "Focus on file operations"` records that text in the latest compaction event's `summary`. |
| `cli_compact_incremental_compaction_numbers` | `compaction_cli.ps1` T6 | Two compactions produce 2 JSONL events with `compaction_number` 1 and 2 in strictly increasing order. |
| `cli_compact_with_compaction_extension_installed` | `compaction_extension.ps1` T1-T4 | With the on-disk `e2e_tests/compaction/extensions/custom_compactor` extension installed (registers `session.compaction` and `session.compaction_post` hooks), the full flow still works: real compact succeeds, JSONL has a compaction event, post-compact `peko send` writes a file, custom-instruction compact preserves the instruction, and the two `compaction_number` values are 1 and 2. |

**Issue 030 — the bug that was unblocking the T1-full test:** the dry-run path of `peko session compact --dry-run --json` was hard-coding `message_count: 0` despite a populated JSONL. The bug was a wire-format overload — the daemon's `SessionCompacted` response was reused for dry-run, with `messages_compacted: 0`, and the CLI re-mapped that single field to both `message_count` and `messages_to_compact`. The fix in [src/ipc/packet.rs:1054-1063](../../pekobot/peko-runtime/src/ipc/packet.rs#L1054-L1063) introduces a dedicated `ResponsePacket::SessionCompactDryRun` variant; the daemon sends it for dry-run and the CLI matches it. Three new unit tests in [src/commands/session.rs:543-587](../../pekobot/peko-runtime/src/commands/session.rs#L543-L587) lock the field-name contract; the `cli_compact_dry_run_json_reports_message_counts_after_multi_turn` test (above) is the end-to-end regression.

**What is still deferred to a follow-up PR (`compaction_auto.ps1`):**

| PS sub-test | Why deferred |
|---|---|
| `compaction_auto.ps1` (all 6 sub-tests) | Auto-compaction uses the LLM to GENERATE the summary text (`src/compaction/mod.rs:462` — `compactor.compact(&messages, &provider)`). The mock can emit canned summary text, but auto-compaction is an internal agent-loop trigger that requires multi-turn real LLM use to actually reach the threshold. Real-LLM tier. |

The auto-compactor's threshold-detection logic is unit-tested at the in-process layer (see `src/compaction/integration_tests.rs`); the missing CI coverage is the end-to-end agent-loop threshold trigger, which is intrinsically real-LLM tier. `compaction_auto.ps1` and the meta-runner `compaction_all.ps1` stay in place until Phase E, consistent with the other migrations.

#### Phase B coverage gap — `e2e_tests/extensions/*.ps1` (L1 migrated, L2+L3 deferred)

**Status:** L1 migrated (10 tests). `tests/cli_extensions.rs` now ships the lifecycle-only coverage of the 9 PS scripts. The remaining L2 and L3 sub-tests stay deferred until Python and/or Node runtimes are guaranteed in the test environment.

| Rust test | Maps to PS sub-test | What it asserts |
|---|---|---|
| `ext_install_skill_tier1_detect` | `skill/python/test.ps1` T1+T2 | `peko ext install <calculator-skill-dir>` (no `--type`) succeeds via Tier 1 `SKILL.md` detection; `peko ext list` shows the skill; `peko ext info` reports `type: skill` |
| `ext_install_mcp_standard_tier1_server_json` | `mcp/python/standard/test.ps1` T1+T2 | `peko ext install <standard-echo-dir>` (NO `--type`, NO `manifest.yaml`) succeeds via Tier 1 `server.json` detection; `peko ext info` reports `type: mcp` |
| `ext_install_mcp_manifest_reserved_params` | `mcp/python/params_injection/test.ps1` T1 | Tier 2 install of an MCP server; the on-disk install preserves the manifest's `reserved_parameters` block (`agent_id`/`session_id` with `source: runtime`) verbatim |
| `ext_install_universal_python_multi_file_copies_subdirs` | `universal/python/multi_file/test.ps1` T3+T4 | Recursive copy works: the install dir contains `manifest.yaml`, the top-level `.py`, AND `utils/__init__.py`, `utils/calculator.py`, `utils/validators.py`, `utils/formatter.py` |
| `ext_install_universal_python_simple_manifest_roundtrip` | `universal/python/simple/test.ps1` T2 | Tier 2 install of a single-file Python tool; on-disk manifest preserves `extension_type: universal-tool` and `parameters:` |
| `ext_install_universal_python_reserved_params_manifest` | `universal/python/reserved_params/test.ps1` T2 | On-disk manifest preserves `reserved_parameters: { session_id, agent_id }` with `source: "runtime"` and `field: "..."` subkeys |
| `ext_install_universal_node_manifest_parsed` | `universal/node/custom.ps1` T1 | Tier 2 install of a Node.js tool; `peko ext info` reports `type: universal-tool`. **Does NOT exec the Node tool — L3 tier.** |
| `ext_install_gateway_manifest_parsed` | `gateway/http_basics/test.ps1` T1 | Tier 2 install of a gateway; `peko ext info` reports `type: gateway`. **Does NOT start the Node gateway — L2/L3 tier.** |
| `ext_install_uninstall_roundtrip` | Cross-cutting (all 9 PS scripts) | Install → list shows it → uninstall → list no longer shows it → info returns non-zero exit; on-disk install dir is also gone |
| `ext_enable_for_agent_modifies_whitelist` | Cross-cutting (6 of 9 PS scripts) | `peko ext enable calculator_simple --target <agent>` writes the canonical ID into the agent's `config.toml` at `[extensions] enabled`; `peko ext disable` removes it |

**The three structural facts this migration documents** (production behavior is correct as-is; the tests assert on the actual production behavior, not on the PS scripts' `--type` flag values):

1. **`--type` flag is ignored at the CLI level.** [`src/commands/ext.rs:363`](../../pekobot/peko-runtime/src/commands/ext.rs#L363) destructures `r#type: _`. Tier 2 detection from `manifest.yaml` is the production path; `--type` is the user's escape hatch when the manifest is missing or wrong. Tests assert on the *detected* type (from `peko ext info`), not the flag value.
2. **Tier 1 `SKILL.md` detection requires the install path to be the skill subdirectory itself.** The detector at [`src/extension/manager/mod.rs:215-256`](../../pekobot/peko-runtime/src/extension/manager/mod.rs#L215-L256) checks `path.join("SKILL.md").exists()` — so `peko ext install <parent>/calculator-skill/` works, but `peko ext install <parent>/` does NOT. The tests mirror the PS scripts' `install` invocation.
3. **The SKILL.md `name:` frontmatter field becomes the extension ID.** [`src/extensions/skill/adapter.rs:108-130`](../../pekobot/peko-runtime/src/extensions/skill/adapter.rs#L108-L130). The calculator-skill fixture has `name: calculator-skill` so install creates an extension with ID `calculator-skill`.

**What is still deferred (L2 + L3, follow-up PRs):**

L2 — background runtime start/stop/status/restart (5 PS sub-tests across gateway + MCP). Blocked on the **runtime dependency**: MCP needs Python, gateway needs Node, the docker-compose stack only guarantees Python. Plan: add `PEKO_TEST_PYTHON` / `PEKO_TEST_NODE` env vars to the `test-integration` Makefile recipes (same shape as `MOCK_LLM_URL`); tests early-return if the runtime is not on `PATH`.

L3 — `peko send` tool execution via mock LLM (12+ PS sub-tests across mcp, universal-python, universal-node, skill, reserved-params-async). Blocked on the **runtime dependency** PLUS the LLM-driven multi-turn dialog (each sub-test scripts an N-turn `tool_call` → `tool_call` → `text <SENTINEL>` dialog in `MOCK_LLM_SCRIPT`). The `AsyncTaskRegistry` access from tests (for `reserved_params` async tests) needs a `peko ext status --json` test-only CLI subcommand that reads the in-process registry — same shape as the `peko subagent list --json` test-only CLI needed for `subagent_async.ps1`. The same subcommand can serve both.

The 9 PS scripts stay in `e2e_tests/extensions/` until Phase E cleanup lands; the on-disk fixtures (`calculator-skill/SKILL.md`, `mcp_server.py`, `multi_file_calc/utils/`, `gateway.js`, `string_tool.js`, `identity_tool.js`, `slow_calculator.py`, `calculator_simple.py`) are reused by the Rust tests.

#### Phase B coverage gap — `e2e_tests/providers/*.ps1` (migrated, real-LLM tier)

**Status:** Migrated (2 tests). `tests/cli_providers.rs` now ships the two real-LLM smoke flows from the PS scripts.

| Rust test | Maps to PS script | What it asserts |
|---|---|---|
| `cli_providers_minimax_smoke` | `minimax.ps1` | `peko send <agent> "Hello, can you tell me a short joke?" --no-stream` returns a non-empty response from the real MiniMax (Anthropic-compatible) provider at `https://api.minimaxi.com/anthropic` with the configured `MiniMax-M2.7` model. Skips if `MINIMAX_API_KEY` is unset. |
| `cli_providers_kimi_smoke` | `kimi.ps1` | `peko send <agent> "Hi" --no-stream` returns a non-empty response from the real Kimi provider at `https://api.kimi.com/coding` with the configured `k2p5` model. Skips if `KIMI_API_KEY` is unset. |

**Two structural facts this migration surfaces (and works around, without code changes):**

1. **`PekoCli::cmd()` removes `MINIMAX_API_KEY` from the daemon's env** — see [tests/common/cli.rs:115](../../tests/common/cli.rs#L115). The safeguard exists so a leaking env can't switch mock-tier tests to the real provider mid-run. For the providers test, this means env-var inheritance to the daemon doesn't work for the minimax test. The tests work around it by writing the `api_key` *directly into the agent's config.toml* (same dual-mode pattern as [tunnel_e2e.rs:78-96](../../tests/tunnel_e2e.rs#L78-L96)) rather than relying on env-var lookup at the agent-config-build step. `KIMI_API_KEY` is NOT removed by `PekoCli::cmd()`, but we use the same direct-config pattern for both tests for symmetry.
2. **Provider-specific base_url / default_model are baked into the test's `write_provider_agent` helper** (per [`src/common/services/agent_service.rs:239-269`](../../pekobot/peko-runtime/src/common/services/agent_service.rs#L239-L269)): minimax uses `https://api.minimaxi.com/anthropic` + `MiniMax-M2.7`; kimi uses `https://api.kimi.com/coding` + `k2p5`. Bypassing `peko agent create --provider <p>` (which is what the PS scripts do) means we hard-code these into the test fixture rather than going through the agent-service helper.

**CI plumbing:** the providers tests run under the `Integration (real LLM)` job in [.github/workflows/integration.yml](../../.github/workflows/integration.yml), which fires on nightly cron (`0 2 * * *`), manual `workflow_dispatch`, or commit messages containing `[llm]`. The workflow passes both `MINIMAX_API_KEY` and `KIMI_API_KEY` as `secrets.*` env, and the Makefile recipe unsets `MOCK_LLM_URL` so the dual-mode rule at [tunnel_e2e.rs:63-76](../../tests/tunnel_e2e.rs#L63-L76) (and our own `kimi_api_key()` / `minimax_api_key()` env checks) falls through to the real provider.

The 2 PS scripts stay in `e2e_tests/providers/` until Phase E cleanup; they are now redundant with `tests/cli_providers.rs`.

#### Phase B coverage gap — `e2e_tests/a2a/*.ps1` (migrated, real-LLM tier, 2-LLM-call flows)

**Status:** Partially migrated (9 tests). `tests/cli_a2a.rs` now ships 9 sub-tests from 2 PS scripts (`a2a_blocking.ps1` T1-T4 + `a2a_isolation.ps1` T1-T5). `a2a_async.ps1` T1-T4 are **deferred** — see [the "async not wired" note below](#async-not-wired). `a2a_all.ps1` is the meta-runner; not migrated.

| Rust test | Maps to PS sub-test | What it asserts |
|---|---|---|
| `a2a_blocking_t1_tool_available` | `a2a_blocking.ps1` T1 | The `a2a_send` tool is registered in the delegator's tool whitelist. Asserts on `A2A_AVAILABLE` sentinel OR `a2a_send` mention in delegator response. |
| `a2a_blocking_t2_blocking_execution` | `a2a_blocking.ps1` T2 | Delegator's `a2a_send` to the worker creates a worker session. Asserts on `A2A_SUCCESS` sentinel OR `worker_session_count > before`. |
| `a2a_blocking_t3_session_resumption` | `a2a_blocking.ps1` T3 | A second `a2a_send` from the same delegator reuses the existing worker session (count unchanged). |
| `a2a_blocking_t4_caller_annotation` | `a2a_blocking.ps1` T4 | The worker session's history contains a user message prefixed `[Message from agent: <delegator>]` (see `a2a_send.rs:99-104`). |
| `a2a_isolation_t1_caller_a_session` | `a2a_isolation.ps1` T1 | Caller A's first `a2a_send` creates exactly 1 target session. |
| `a2a_isolation_t2_caller_b_session` | `a2a_isolation.ps1` T2 | Caller B's first `a2a_send` creates a 2nd target session (isolated from caller A's). |
| `a2a_isolation_t3_peer_id_isolation` | `a2a_isolation.ps1` T3 | The two target sessions have distinct `peer_id`s matching callerA / callerB. |
| `a2a_isolation_t4_caller_a_resumes` | `a2a_isolation.ps1` T4 | Caller A's second call resumes its OWN session (session_id unchanged). |
| `a2a_isolation_t5_message_counts` | `a2a_isolation.ps1` T5 | Both isolated sessions have ≥ 3 messages after 3 send calls. |

<a id="async-not-wire"></a>**`a2a_async.ps1` is deferred (async not wired in production).** The `a2a_send` tool's parameter schema at [`src/tools/builtin/messaging/a2a_send.rs:148-171`](../../pekobot/peko-runtime/src/tools/builtin/messaging/a2a_send.rs#L148-L171) only declares `target_agent` and `message` — it does NOT expose `_async` (or `_timeout`) as a parameter. The framework-level `AsyncExecutionRouter` is supposed to intercept `_async: true` at dispatch time, but with the parameter omitted from the schema the LLM never sends it. A real LLM run correctly observed: "the a2a_send tool schema does not include an `_async` parameter — it operates synchronously by default". The `a2a_async.ps1` PS script is therefore aspirational; its "PASS" verdict was the structural fallback. Migration of `a2a_async.ps1` T1-T4 is a follow-up PR when the schema is fixed.

**Why real-LLM tier (not mockable):** every test requires a real LLM to drive the delegator's `a2a_send` tool_call AND a real LLM to drive the worker's response. The LLM-call count is ~1.5-2 per test (delegator always; worker only if it has tools), so total wall clock for the 9 tests is ~3-4 min. Each test early-returns if `MINIMAX_API_KEY` is unset, so a bare `cargo test` still passes.

**Why lenient assertions:** real LLMs are non-deterministic — even a clear "reply exactly A2A_SUCCESS" instruction may not be followed verbatim. The PS scripts' "PASS" verdict falls through to a structural check (e.g. "the worker session was created") when the LLM doesn't emit the literal sentinel. The Rust tests mirror this: an LLM-output sentinel match is a sufficient pass, but a structural side-effect (worker session count increased, peer_id matches, session_id unchanged) is also a pass. Tests that pass via the LLM-output fallback log a `WARN:` so CI logs can show how often the lenient branch fires.

**Why direct config.toml writes:** same as `cli_providers.rs` — `PekoCli::cmd()` removes `MINIMAX_API_KEY` from the daemon's env to safeguard mock-tier tests, so the tests bake the `api_key` into each agent's `config.toml` directly rather than going through the `peko auth set` + `peko agent create --provider minimax` flow.

The 3 PS scripts stay in `e2e_tests/a2a/` until Phase E cleanup. `a2a_blocking.ps1` and `a2a_isolation.ps1` are now redundant with `tests/cli_a2a.rs`; `a2a_async.ps1` stays until the async path is wired.

**A non-obvious gotcha this migration uncovered (and worked around):** the daemon's `read_file` tool resolves relative paths against `<peko_dir>/data/workspaces/` (the **shared** workspaces root), not the per-agent subdir. The PS scripts write sentinel files to `$env:APPDATA/peko/workspaces/default/$worker/test_a2a.txt` (per-agent subdir) and rely on the lenient structural fallback ("the worker session was created, so a2a_send dispatched") — the actual `read_file` call in the PS scripts was *failing* (file not found at the resolved path) and the worker was responding with "I don't have access to the file". The Rust tests bake the sentinel into the **shared** workspaces root instead, so the worker's `read_file` actually finds the file. See [`tests/cli_tools.rs:108-115`](../../tests/cli_tools.rs#L108-L115) for the workspace-resolution explanation. Each test uses a unique file name (e.g. `test_a2a.txt` for the blocking test) so cross-test collisions don't occur.

### Phase C — Mock-LLM enhancement (✅ landed; unblocks Phase B mock-tier work)

[.github/docker/mock-llm/mock_llm_server.py](../../.github/docker/mock-llm/mock_llm_server.py) supports:

- **`DEFAULT_RESPONSE` env** — overrides the fallback text (was previously hardcoded to `"Peko tunnel works!"`).
- **Keyword echo** — `Respond with: <KEYWORD>` in the prompt returns `<KEYWORD>`. Matches the convention the PowerShell scripts already use (`SUCCESS`, `FAIL`, `MEMORY_SUCCESS`).
- **Tool-call responses** — `Call tool: <name>` in the prompt returns a streamed `tool_calls` array for `<name>` with empty JSON args.
- **`MOCK_LLM_SCRIPT` env** — JSON map of prompt-substring → response (string or `{tool_call: {name, arguments}}`), so tests can seed complex scripted dialogs without modifying the mock.
  - **Sequence (list value)** — a value may be a *list* of response specs; the i-th time the substring matches returns the i-th element, then clamps to the last element. Per-substring counter, reset by `POST /_test/configure`. This is the feature that lets multi-turn tests (tool-call → result → keyword) stay in the mock tier. Spec in §3 above; reference test in [tests/mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs).
- **`POST /_test/configure`** — test-only endpoint to swap `MOCK_LLM_SCRIPT` / `DEFAULT_RESPONSE` and clear the per-substring counters without restarting the container.

Full spec lives in §3 above. The string and tool-call forms unblocked moving the LLM-required PowerShell tests in `e2e_tests/send/`, `e2e_tests/session/`, and the chat-dependent half of `e2e_tests/agent/` into the mock-LLM tier rather than the real-LLM tier — 24 tests have already been migrated (see Phase B). The Sequence form unblocked the remaining mock-tier migrations (`cron_agent_tool.ps1` plus the `cli_extensions` / `cli_a2a` / `cli_subagent` / `cli_tools` / `cli_compaction` slices in Phase B) — see the row flips in the Phase B table above.

### Phase D — Phase-7 user-journey scenarios (D1–D4)

Four end-to-end user-journey scenarios, each its own PR and its own
`tests/scenarios/sN_*.rs` file. **Mock-LLM tier** (not real-LLM
as the prior §7 stub stated — that was wrong, this section
supersedes it). The LLM is incidental: it provides the chat
payload that the orchestration plumbing streams back. What we are
testing is the runtime↔registry↔tunnel↔Pekohub-relay plumbing.

**Scope note.** **Team is out of scope for Phase D.**
Team-shared extensions, team-scoped permissions, and team-push
flows are deferred to a separate follow-up.

| # | Rust file | Flow | Status |
|---|---|---|---|
| D1 | [tests/scenarios/s1_local_agent_with_extensions.rs](../../tests/scenarios/s1_local_agent_with_extensions.rs) | Flow 1+2 — create agent, create ext, enable on agent, chat locally | ✅ PR-1 (6 tests) |
| D2 | `tests/scenarios/s2_extension_registry_roundtrip.rs` | Flow 3+4 — author `peko ext push` → pekohub → collab `peko ext pull` → enable → chat | ⏳ Pending PR-2 (4 tests planned) |
| D3 | `tests/scenarios/s3_agent_registry_roundtrip.rs` | Flow 5 — author `peko agent push` (carrying ext refs) → pekohub → collab `peko agent pull` → ext auto-pulled → run | ⏳ Pending PR-3 (4 tests planned) |
| D4 | `tests/scenarios/s4_publish_running_agent_with_permission.rs` | Flow 6 — author runs agent behind tunnel → permitted user → 200; random → 403; unauth → 401 | ⏳ Pending PR-4 (3 tests planned) |

**Why mock-LLM tier, not real-LLM.** These scenarios assert on
plumbing — the keyword echo / `MOCK_LLM_SCRIPT` payload proves
"the runtime successfully delivered the message through the
orchestration path and got a response back". What the LLM would
*decide* to do with the prompt is irrelevant. This is the same
approach the Phase B `cli_send` / `cli_session` / `cli_subagent`
migrations used (mock LLM to drive non-decision-bound flows).

**Per-scenario helpers.** Each `sN_*.rs` follows the shape of the
existing CLI tests: `mod common; use common::{PekoCli, …};
async fn scenario_X() { … }`. Tests are `#[ignore] = "requires
mock LLM"` (D1) or `#[ignore] = "requires PekoHub + mock LLM"`
(D2-D4). PekoHub-touching flows use `PekohubBackend::start()` +
`reset_pekohub()` from [tests/common/harness.rs](../../tests/common/harness.rs).
Two-`PekoCli` scenarios (D2, D3) follow the `PekoCli::new()`
pattern from [tests/common/cli.rs:25-62](../../tests/common/cli.rs#L25-L62):
each instance owns its own `TempDir`; on Windows each gets a
unique named pipe via `PEKO_DAEMON_PIPE`.

**Subdir discovery.** Cargo's integration-test auto-discovery only
finds `tests/*.rs` directly. Files in `tests/scenarios/` need an
explicit `[[test]]` entry in `Cargo.toml` (see the
`s1_local_agent_with_extensions` block at the bottom of
`Cargo.toml`); D2-D4 add their own entries in their respective PRs.

**D4 specifics — no production code change.** The per-instance
ACL is enforced server-side at
[pekohub/backend/src/services/instances.ts:339-345](../../../../pekohub/backend/src/services/instances.ts#L339-L345)
(`canChat`), with the relay returning 403 before any tunnel
traffic when the caller is not the owner and not in
`allowedUsers` ([routes/api/instances.ts:569-573](../../../../pekohub/backend/src/routes/api/instances.ts#L569-L573)).
The runtime pushes `allowedUsers` to pekohub on
`instance_announce` and `exposure_update`. D4 asserts on the
end-to-end 200/403/401 outcome — there is no production code
gap to close. The runtime-side grant path is `peko agent permit
<agent> <subject> chat`, which updates `config.permissions` and
triggers an `exposure_update`.

> **No `integration-tests/` directory.** The old docs referenced
> one that never existed on disk. Scenarios live under
> `tests/scenarios/` and are picked up by the same
> `cargo test --test '*' -- --include-ignored` glob.

### Phase E — Archive the folder

Once Phases A–D land:
- Move any remaining one-off scripts into `e2e_tests/_archive/` (which already exists for legacy CAP tests), or delete outright.
- Remove `e2e_tests/` from the path filters in `.github/workflows/integration.yml`.
- Delete `e2e_tests/README.md` and the top-level `reset.ps1`.

---

## 8. End-State Makefile

The four canonical targets ([Makefile](../../Makefile)):

```makefile
# Fast feedback — no Docker, no LLM
test:                  cargo test --lib

# Tier 1 — PR gate: Docker + PekoHub + mock LLM
test-integration:      docker-up && \
                       env -u MINIMAX_API_KEY \
                         PEKOHUB_URL=… MOCK_LLM_URL=… \
                         cargo test --test pekohub_integration --test tunnel_integration \
                                    --test tunnel_e2e --test packaging_integration \
                                    --test registry_integration --test team_integration \
                                    --test extension_packaging \
                                    --test cli_send --test cli_session --test cli_basics \
                                    --test cli_cron --test cli_subagent \
                                    --test cli_tools \
                                    --test cli_compaction \
                                    --test cli_extensions \
                                    --test cli_providers \
                                    --test cli_a2a \
                                    --test s1_local_agent_with_extensions \
                                    --test mock_llm_sequence \
                                    -- --include-ignored

# Tier 2 — nightly + [llm] commit tag: adds real-LLM tests
test-integration-llm:  docker-up && \
                       env -u MOCK_LLM_URL \
                         PEKOHUB_URL=… \
                         cargo test --test … -- --include-ignored
                       # MINIMAX_API_KEY must be set; recipe refuses otherwise.

# Everything
test-all:              test && test-integration && test-integration-llm
```

The per-test-file granular targets (`test-pekohub`, `test-tunnel`, `test-tunnel-e2e`, `test-packaging`, `test-registry`, `test-subagent`, `test-cli-send`, `test-cli-session`, `test-cli-basics`, `test-cli-cron`, `test-cli-subagent`, `test-cli-tools`, `test-cli-compaction`, `test-cli-extensions`, `test-cli-providers`, `test-cli-a2a`, `test-scenarios-s1` + `-s2` + `-s3` + `-s4` from §7 Phase D, `test-mock-llm-sequence`) survive as one-file slices for change-isolated dev loops — each enforces the same `env -u MINIMAX_API_KEY` rule as the umbrella. `test-cli-a2a` is a real-LLM tier slice (needs `MINIMAX_API_KEY`). The four `test-scenarios-sN` targets are mock-LLM tier.

> **Why `--include-ignored`, not `--ignored`.** All 116 hub- or mock-LLM-gated tests are `#[ignore]`, but the 16 always-on tests (10 pure-Rust in `team_integration.rs` + `extension_packaging.rs`, plus 6 offline CLI tests in `cli_basics.rs`) are not. `cargo test … -- --ignored` would silently skip those 16. `--include-ignored` runs both — which is what we want for the umbrella targets.

**How tests opt into the real-LLM tier.** Use a runtime skip at the top of the test:

```rust
#[tokio::test]
#[ignore = "real LLM required (set MINIMAX_API_KEY)"]
async fn test_thing_that_needs_real_model() {
    if std::env::var("MINIMAX_API_KEY").is_err() { return; }
    // … test body …
}
```

Tests that work with either LLM use whichever is set — mock-first. The reference is the dual-mode logic in [tunnel_e2e.rs:63-76](../../tests/tunnel_e2e.rs#L63-L76).

---

## 9. End-State CI

Three jobs in [.github/workflows/integration.yml](../../.github/workflows/integration.yml), chained `unit → integration → integration-llm`:

| Job | Command | When | Needs |
|---|---|---|---|
| `unit` | `make test` | Every PR | — |
| `integration` | `make test-integration` | Every PR | `unit` |
| `integration-llm` | `make test-integration-llm` | Nightly cron, `[llm]` in commit msg, or `workflow_dispatch` | `integration` |

Each integration job sibling-checks-out `pekohub` next to `peko-runtime/` so the compose file's `context: ../../pekohub` resolves, brings the stack up via `make docker-up`, then polls `http://localhost:3000/health` for up to 60s before invoking cargo (PekohubBackend has only 5s of grace).

`MINIMAX_API_KEY` lives as a repo secret. Only `integration-llm` reads it. The `integration` job runs with the secret unset — that's how we guarantee the mock-LLM-tier tests stay mock-only and don't silently leak to the real provider. The Makefile recipes additionally `env -u` the *other* knob (`MINIMAX_API_KEY` in `test-integration`, `MOCK_LLM_URL` in `test-integration-llm`) as belt-and-suspenders.

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
