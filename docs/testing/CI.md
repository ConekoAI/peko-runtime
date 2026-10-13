# Runtime CI tests

The workflow is `.github/workflows/integration.yml`. Test selection lives in
the Makefile; adding a suite requires updating the appropriate target list
and, for Docker tests, the workflow's `mock_tests` path filter.

| Target | Dependencies | Coverage |
| --- | --- | --- |
| `make test` | Rust | Workspace library tests |
| `make test-native` | Rust, CLI/daemon binaries | CLI basics, snapshot inspection/import, permission IPC, CLI streaming/model override, provider stream termination, local credential/identity storage |
| `make test-integration-mock` | Rust, Docker, sibling `../pekohub` | CLI send/DM persistence, Agent delegation, file/shell tool wiring, blocking CLI creation/genesis, mock sequencing, Hub permissions and tunnel protocol/chat |
| `make test-integration` | Both native and mock dependencies | Native plus mock integration suites |
| `make test-integration-llm` | Native dependencies, Docker, real provider environment | Native, tunnel protocol and real-provider CLI/tunnel chat tests (local opt-in) |

The Linux job runs library tests, daemon shutdown tests and `test-native`.
The Docker job follows it and runs only `test-integration-mock`. Native-only
test edits skip Docker. Mock/Hub tests, shared fixtures, tool sources, Docker
assets and workflow/Makefile changes trigger that job; schedule and manual
dispatch run both tiers. A skipped native job does not suppress a requested
Docker job, but a failed or cancelled prerequisite does.

Mock targets unset `LLM_API_KEY`. Native targets also unset `MOCK_LLM_URL` and
`PEKOHUB_URL`; they do not need either server. Real-provider tests require
`LLM_API_KEY`, `LLM_BASE_URL`, `LLM_MODEL` and optionally `LLM_API_FORMAT`.
All subprocess fixtures use isolated passphrase vaults. Credential tests use
explicit encrypted-file storage; the old headless check that accepted either
success or failure (and could touch the OS keychain) was removed.

## Fixtures and assertions

Ordinary mock/IPC fixtures write a ready principal definition with
`boot_state = "organized"`, pin its model and owner, and seed the production
root role. The guarded daemon loads it and generates its persistent identity
through the production vault path. No bootstrap cron jobs or unguarded
sidecars run during fixture setup. `cli_basics` still checks CLI provisioning;
`cli_genesis` is the one mock-tier test that intentionally waits for the
scheduled genesis turn, checking successful finalized history, organized
state and one-shot reaping.

The Agent daemon suite retains directory-role child writes, flat-role child
writes and a grandchild write. Each checks files plus successful Agent tool
receipts and exact child outputs in the session JSONL. Retired isolation
variants and tests that only checked scripted final text were removed.
Response indexing, exhaustion, reset, mixed tool/text and independent
counters are checked directly against the mock HTTP server.

`tiered_prompt`, `cli_log` and `cli_session_manage` remain explicit developer
suites rather than default CI gates. The real-provider suite is selected
only by the real-LLM target; unset credentials must not masquerade as passed
mock-tier coverage.

## Running locally

```sh
make test-native
make test-integration-mock
make docker-down
```

`test-integration-mock` starts its own stack. If servers are already running,
skip that prerequisite without changing test selection:

```sh
make -o docker-up test-integration-mock \
  MOCK_LLM_URL=http://127.0.0.1:8080 PEKOHUB_URL=http://127.0.0.1:3000
```

Before this split, successful October 12–13 CI jobs took about 29.5 minutes:
Agent cases consumed 11.3 minutes, permission IPC 3.9 minutes and mock fixture
sequencing 2.9 minutes. Repeated blocking CLI creation paid a 60-second genesis
delay. Compare future CI runs against these job and suite timings; local
warm-build timings exclude runner, compilation and Docker startup costs.
