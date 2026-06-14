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
| [mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs) | 6 | 6 | mock LLM (sequence feature, §3) | Y |

**Totals:** 81 hub- or mock-LLM-gated tests (all `#[ignore]`, un-ignored by `--include-ignored`) + 16 always-on tests (10 pure Rust + 6 offline CLI) = 97 in `tests/`.

The 5 files that exercise the hub directly — [packaging_integration.rs](../../tests/packaging_integration.rs), [pekohub_integration.rs](../../tests/pekohub_integration.rs), [registry_integration.rs](../../tests/registry_integration.rs), [tunnel_integration.rs](../../tests/tunnel_integration.rs), [tunnel_e2e.rs](../../tests/tunnel_e2e.rs) — share the **same dual-mode `PekohubBackend::start()` harness** in [tests/common/harness.rs](../../tests/common/harness.rs): read `PEKOHUB_URL` and reuse a running container, or spawn `node` + `tsx` against `pekohub/backend/tests/fixtures/server.ts`. The `tunnel_*` tests additionally derive `ws_url` from `PEKOHUB_URL` (`http(s)://` → `ws(s)://`, append `/v1/tunnel`). The 4 `cli_*` files ([cli_send.rs](../../tests/cli_send.rs), [cli_session.rs](../../tests/cli_session.rs), [cli_basics.rs](../../tests/cli_basics.rs), [cli_cron.rs](../../tests/cli_cron.rs)) also need the hub but use a different pattern: they spawn the `peko` daemon as a subprocess against the same stack and let the daemon do the hub calls. [mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs) does not need PekoHub — it talks to the mock directly (plus the peko daemon for the three-call flow) — but ships in the same docker-up workflow for the dev-loop convenience.

> **Known issue:** [pekohub_integration::test_pekohub_search_api](../../tests/pekohub_integration.rs#L466) is double-blocked: needs PekoHub *and* has a null-hooks schema validation bug in the search response. Tracked, not blocked on this doc.

### Counts at a glance

- Unit (`cargo test --lib`): everything in `src/**`, no network — includes the 13 subagent and 1 JWKS tests above.
- Integration: 97 tests across 13 files in `tests/`.
- E2E PowerShell scripts in `e2e_tests/`: 67 total (54 live + 13 already under `_archive/`); outside CI, to be dismantled — see §7.

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

`peko-runtime/e2e_tests/` still holds 60 live PowerShell scripts that live outside CI, overlap heavily with `tests/*.rs`, and still reference a deleted Python mock. The end-goal is to dismantle the folder. Phases A and the cli_send / cli_session / cli_basics / cli_cron legs of Phase B have already landed.

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
| `e2e_tests/extensions/` | `tests/cli_extensions.rs` | mock-LLM (tool-call sequence, §3 Sequence) | ⏳ Pending |
| `e2e_tests/compaction/` | `tests/cli_compaction.rs` | mock-LLM (multi-turn sequence) | ⏳ Pending |
| `e2e_tests/a2a/`, `e2e_tests/tools/` | `tests/cli_a2a.rs`, `tests/cli_tools.rs` | mock-LLM (tool-call decisions via Sequence) | ⏳ Pending |
| `e2e_tests/subagent/` | `tests/cli_subagent.rs` | mock-LLM (tool-call decisions via Sequence) | ✅ Migrated (7 tests; `subagent_async.ps1` + `subagent_status_list.ps1` deferred — see coverage gap below) |
| `e2e_tests/providers/` | `tests/cli_providers.rs` | real-LLM (gated by `MINIMAX_API_KEY` / `KIMI_API_KEY`) | ⏳ Pending |

#### Phase B coverage gap — `e2e_tests/cron/cron_agent_tool.ps1`

**Status:** ✅ Migrated to `tests/cli_cron.rs` as 2 new mock-LLM-tier tests (`cron_agent_tool_schedules_and_lists_job`, `cron_agent_tool_schedules_and_cancels_job`). PS TEST 3 (wait 3:30 for execution) is intentionally not migrated — too slow for CI; the scheduling and cancellation sides are what exercise the agent-tool chain, and execution itself is covered by the daemon-CRUD tests in the same file.

**What the migrated tests cover:** the agent uses its built-in `cron` tool (sub-commands `at` / `list` / `cancel`) to self-schedule, self-list, and self-cancel jobs. The schedule test verifies the resulting job is visible to the daemon (`peko cron list` shows it); the cancel test verifies an agent-cancelled job disappears from `peko cron list`.

**How the mock drives the multi-turn dialog:** §3 *Sequence* lets a `MOCK_LLM_SCRIPT` list value carry one response per LLM call. The schedule test scripts 3 elements: `tool_call(cron, at, ...)` → `tool_call(cron, list)` → text `TOOL_SUCCESS`. The cancel test scripts 4: `at` → `list` → `cancel` (using `cancel_label` so the mock doesn't need a pre-known `job_id`) → text `CANCEL_SUCCESS`. Each turn's tool_call's `function.arguments` is a JSON-encoded string with the structured `cron` args the runtime's `CronTool` dispatcher needs (`sub_command`, `time`, `label`, `task`, `agent_id` for `at`; `sub_command: "cancel"`, `cancel_label: "..."` for cancel).

**Reference for the mock-side syntax:** [tests/mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs) (`mock_llm_script_list_supports_mixed_text_and_tool_call`); the helper that POSTs to `/_test/configure` lives in [tests/common/mock_configure.rs](../../tests/common/mock_configure.rs).

#### Phase B coverage gap — `e2e_tests/subagent/subagent_async.ps1` and `subagent_status_list.ps1`

**Status:** Partial migration. `subagent_blocking.ps1` (T1, T2, T4), `subagent_nesting.ps1` (T1, T2), and `subagent_isolation.ps1` (T1, T2) migrated to `tests/cli_subagent.rs` as 7 mock-LLM-tier tests. `subagent_async.ps1` and `subagent_status_list.ps1` are **deferred** because both depend on the in-process `AsyncTaskRegistry` (in `src/extension/async_exec/executor/`), which is built per-daemon and is not currently addressable from a Rust integration test that goes through the `peko` CLI.

**What is deferred:**
- `subagent_async.ps1`: T1 (async-receipt shape), T2 (task-file polling), T3 (`_timeout` enforcement), T4 (concurrent `_async` spawns). T1 and T3 are unit-testable in principle — the receipt shape and the timeout path are both `agent_spawn.rs::execute_spawn_async` paths already covered by `subagent_integration_tests` in `src/agent/tests/`. T2 and T4 require the registry to be populated BEFORE the `task` tool looks up the `task_id`, which a unit test can do directly via `with_registry` but a black-box `peko send` test cannot.
- `subagent_status_list.ps1`: all four T's. `action=status` and `action=cancel` need a pre-known `task_id`; the mock can emit a `tool_call(task, action=status, task_id=…)` only if the test knows what `task_id` the runtime will mint (it is `format!("run_{}", uuid::Uuid::new_v4().simple())` per `src/agent/subagent_executor.rs:247`). `action=list` is testable in principle but adds no coverage beyond what the unit tests already provide.

**What is in scope for a future PR (PR-3 candidate):** add a test-only `peko subagent list --json` (or `peko task list --json`) CLI subcommand that reads the in-process `AsyncTaskRegistry` and dumps it as JSON, then write `cli_subagent.rs` tests that drive the parent's mock to emit a `task` tool_call and assert on the CLI output. Same shape as the `peko cron list --json` round-trip in `cli_cron.rs`.

**What the migrated 7 tests cover:** the `agent_spawn` tool's blocking path end-to-end through the daemon. Blocking `write_file` and `read_file` subagents (blocking T1, T2, T4), the `isolated: true` flag (blocking T2 + isolation T2), a 2-level nesting chain (nesting T1), the multi-level dispatch plumbing (nesting T2), and shared-context vs. isolated-context for subagents (isolation T1, T2). All seven verify the parent's `peko send --no-stream` stdout contains the expected sentinel AND (where applicable) that the file written by the subagent (or grandchild) lands in the parent's personal workspace. The `MOCK_LLM_SCRIPT` uses the §3 *Sequence* feature with per-test unique substrings, so the per-substring counter never races with neighbouring tests.

### Phase C — Mock-LLM enhancement (✅ landed; unblocks Phase B mock-tier work)

[.github/docker/mock-llm/mock_llm_server.py](../../.github/docker/mock-llm/mock_llm_server.py) supports:

- **`DEFAULT_RESPONSE` env** — overrides the fallback text (was previously hardcoded to `"Peko tunnel works!"`).
- **Keyword echo** — `Respond with: <KEYWORD>` in the prompt returns `<KEYWORD>`. Matches the convention the PowerShell scripts already use (`SUCCESS`, `FAIL`, `MEMORY_SUCCESS`).
- **Tool-call responses** — `Call tool: <name>` in the prompt returns a streamed `tool_calls` array for `<name>` with empty JSON args.
- **`MOCK_LLM_SCRIPT` env** — JSON map of prompt-substring → response (string or `{tool_call: {name, arguments}}`), so tests can seed complex scripted dialogs without modifying the mock.
  - **Sequence (list value)** — a value may be a *list* of response specs; the i-th time the substring matches returns the i-th element, then clamps to the last element. Per-substring counter, reset by `POST /_test/configure`. This is the feature that lets multi-turn tests (tool-call → result → keyword) stay in the mock tier. Spec in §3 above; reference test in [tests/mock_llm_sequence.rs](../../tests/mock_llm_sequence.rs).
- **`POST /_test/configure`** — test-only endpoint to swap `MOCK_LLM_SCRIPT` / `DEFAULT_RESPONSE` and clear the per-substring counters without restarting the container.

Full spec lives in §3 above. The string and tool-call forms unblocked moving the LLM-required PowerShell tests in `e2e_tests/send/`, `e2e_tests/session/`, and the chat-dependent half of `e2e_tests/agent/` into the mock-LLM tier rather than the real-LLM tier — 24 tests have already been migrated (see Phase B). The Sequence form unblocked the remaining mock-tier migrations (`cron_agent_tool.ps1` plus the `cli_extensions` / `cli_a2a` / `cli_subagent` / `cli_tools` / `cli_compaction` slices in Phase B) — see the row flips in the Phase B table above.

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
                                    --test cli_cron --test mock_llm_sequence \
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

The per-test-file granular targets (`test-pekohub`, `test-tunnel`, `test-tunnel-e2e`, `test-packaging`, `test-registry`, `test-subagent`, `test-cli-send`, `test-cli-session`, `test-cli-basics`, `test-cli-cron`, `test-cli-subagent`, `test-mock-llm-sequence`) survive as one-file slices for change-isolated dev loops — each enforces the same `env -u MINIMAX_API_KEY` rule as the umbrella.

> **Why `--include-ignored`, not `--ignored`.** All 74 hub- or mock-LLM-gated tests are `#[ignore]`, but the 16 always-on tests (10 pure-Rust in `team_integration.rs` + `extension_packaging.rs`, plus 6 offline CLI tests in `cli_basics.rs`) are not. `cargo test … -- --ignored` would silently skip those 16. `--include-ignored` runs both — which is what we want for the umbrella targets.

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
