# Pekobot — Phase 1 Completion Checklist

**Version:** 1.0
**Date:** 2026-03-16
**Status:** Draft

Phase 1 is complete when every item in this checklist is checked. Items marked **[MUST]** are hard gates — Phase 1 does not ship without them. Items marked **[SHOULD]** are strong expectations — defer only with an explicit recorded decision. Each item links to its source requirement.

This document is intentionally short. The requirements spec has the detail; this has the gate.

---

## How to Use This Checklist

- Work through sections in order — later sections depend on earlier ones.
- Each item has a one-line test or verification method in `code style`.
- "Checked" means: implemented, tested, and the test is in CI. Not "I think it works."
- When deferring a **[SHOULD]**, add a note with the reason and a tracking issue.

---

## 1. Agent Image and Instance Model

- [ ] **[MUST]** Agent runs directly from a directory containing only `config.toml` — no other files required.
  `pekobot run ./minimal/` where `minimal/` has only `config.toml` — succeeds and responds.

- [ ] **[MUST]** All optional files (`AGENT.md`, `BOOTSTRAP.md`, `IDENTITY.md`, `SOUL.md`, `TOOLS.md`, `USER.md`, `SKILLS.md`, `mcp.json`, `tools/`, `projects/`, `memories/`, `skills/`) are loaded if present and silently skipped if absent.
  Add and remove each file individually; verify behaviour changes only when expected.

- [ ] **[MUST]** `sessions/` directory is never included in a built image and is never read from the image source.
  `pekobot build ./agent/ -t test:v1`; inspect manifest — no `sessions/` layer.

- [ ] **[MUST]** Image is immutable once built. No runtime operation writes to the image directory.
  Run instance from a packaged image; perform writes via `filesystem` tool; verify image directory unchanged.

- [ ] **[MUST]** Instance pins to image digest at creation time. Pulling a newer tag does not affect running or stopped instances.
  Create instance from `agent:v1`; push `agent:v2` with same tag; `pekobot ps` still shows original digest.

- [ ] **[MUST]** `pekobot instance upgrade <id> --to <image:tag>` is the only mechanism to change an instance's pinned digest.
  Upgrade an instance; verify `image_digest` changes and a `system` event is written to session JSONL.

- [ ] **[SHOULD]** Base image inheritance: `[base] image = "..."` loads and merges base config before local config.
  Create derived agent with `[base]`; verify base `AGENT.md` is used when local `AGENT.md` is absent; verify local `AGENT.md` replaces it when present.

- [ ] **[SHOULD]** Base merge rules are correct: scalar fields — local wins; arrays — appended and deduplicated; `[[hooks]]` — inherited, cannot be removed.
  Unit test each merge rule with a two-level inheritance chain.

---

## 2. Core Runtime and Agentic Loop

- [ ] **[MUST]** Agentic loop accepts input from three sources: user message, hook trigger, A2A bus message.
  Trigger a session via each source; verify `user.message` with correct `source` field in JSONL.

- [ ] **[MUST]** Loop continues until LLM produces a final response with no pending tool calls.
  Agent with chained tool calls (tool A result triggers tool B call); verify loop completes and `assistant.message` is written only after all tools resolve.

- [ ] **[MUST]** Tool panic/exception is caught; error returned to LLM as `tool.result` with `error` set; loop continues.
  Inject a panic into a test tool; verify session continues and LLM receives error result.

- [ ] **[MUST]** Synchronous tool: loop blocks until result before next LLM call.
  Instrument timing; verify next `llm_complete` does not begin until `tool.result` is written.

- [ ] **[MUST]** Asynchronous tool: loop returns receipt to LLM immediately; result injected into context when ready.
  Use `process` tool with `async: true`; verify receipt returned in same turn; result arrives in a later context injection.

- [ ] **[MUST]** Tool timeout produces `tool.result` with `error: "timeout"`; loop does not hang.
  Set `timeout_ms: 100` on a tool that sleeps 5 seconds; verify `tool.result` error within 200ms; loop continues.

- [ ] **[MUST]** Subagent spawn (sync): parent blocks until child session completes; child output returned as tool result.
  `agent_spawn` with `async: false`; verify parent session paused; child session created; `spawn.result` written to parent session.

- [ ] **[MUST]** Subagent spawn (async): parent receives receipt immediately; `agent_spawn_status` reflects child progress.
  `agent_spawn` with `async: true`; verify `spawn.request` written; poll `agent_spawn_status` until `completed`.

- [ ] **[MUST]** Spawned child instance is stopped and cleaned up after completion or timeout; child session is retained.
  After sync spawn completes: `pekobot ps` does not list child; `GET /agents/{child_id}/sessions` returns session.

- [ ] **[MUST]** All four LLM providers work: Anthropic, OpenAI, Ollama, OpenAI-compatible.
  Run minimal agent against each provider; verify response received and session written.

- [ ] **[MUST]** Watch mode (`--watch`): agent configuration reloads within 2 seconds of any source file change.
  `touch AGENT.md` while agent running in watch mode; next turn uses new content within 2 seconds.

---

## 3. Daemon and HTTP API

- [ ] **[MUST]** Daemon starts with zero configuration; all defaults apply without `runtime.toml` present.
  `pekobot daemon start` on a fresh system with no `.pekobot/` directory; daemon starts and responds to `GET /health`.

- [ ] **[MUST]** Daemon listens on `127.0.0.1:11434` by default.
  `ss -tlnp | grep 11434` shows `127.0.0.1:11434`, not `0.0.0.0`.

- [ ] **[MUST]** Binding to non-loopback address prints a warning to stderr.
  Set `host = "0.0.0.0"` in `runtime.toml`; start daemon; verify warning line in stderr.

- [ ] **[MUST]** All endpoints in `API_CONTRACT.md` §3–§10 are implemented and return correct schemas.
  Run the full API contract test suite; all endpoint tests pass.

- [ ] **[MUST]** Every response includes `X-Pekobot-Version` header.
  `curl -I http://localhost:11434/health` — header present.

- [ ] **[MUST]** `X-Request-ID` echoed in response if provided; generated if absent.
  Send request with header; verify echo. Send without; verify generated ID in response.

- [ ] **[MUST]** All error responses use the standard envelope `{ "error": { "code", "message", "request_id", "details" } }`.
  Trigger each error code in `API_CONTRACT.md` §11.3; verify envelope shape.

- [ ] **[MUST]** `POST /agents/{id}/chat` streams SSE events in correct order: `delta*`, `tool_call`/`tool_result` interleaved, `done`.
  Capture full SSE stream; verify event type sequence matches spec.

- [ ] **[MUST]** Non-streaming fallback: `Accept: application/json` returns complete response after turn completes.
  Send chat request with `Accept: application/json`; verify single JSON response with `message`, `tool_calls`, `usage`.

- [ ] **[MUST]** WebSocket chat endpoint `ws://localhost:11434/agents/{id}/ws` handles `message` frames and returns `delta`/`done` frames.
  Connect via WebSocket; send message frame; verify frame sequence.

- [ ] **[MUST]** First `delta` SSE event arrives within 500ms of LLM producing first token.
  Instrument LLM stream start; measure to first `delta` event at client; median < 500ms over 10 runs.

- [ ] **[MUST]** All four hook types activate a session correctly: `cron`, `webhook`, `event`, `file_watch`.
  Configure one hook of each type; trigger each; verify `hook.trigger` event written to session JSONL.

- [ ] **[MUST]** Webhook endpoint `POST /webhooks/{instance_id}/{token}` validates token; returns `202` on success, `401` on bad token.
  POST with correct token; verify `202`. POST with wrong token; verify `401`.

- [ ] **[MUST]** System event stream `ws://localhost:11434/events` emits events for all instance and team lifecycle changes.
  Subscribe to event stream; create, start, stop, remove an instance; verify all four event types received.

- [ ] **[MUST]** `GET /health` returns `200` with structured body; returns `503` when daemon is degraded.
  Healthy daemon: `200`. Simulate degraded state (e.g. disk full); verify `503`.

---

## 4. Session Management

- [ ] **[MUST]** Every session event is written as a separate JSONL line with the standard envelope (`id`, `type`, `session_id`, `ts`, `seq`).
  Run a session with tool calls; inspect raw JSONL; verify every line parses as valid JSON with required fields.

- [ ] **[MUST]** All 13 event types are written at the correct moment:
  `session.created` (first line), `user.message`, `assistant.message`, `thinking` (if enabled), `tool.call`, `tool.result`, `spawn.request`, `spawn.result`, `a2a.sent`, `a2a.received`, `hook.trigger`, `system`, `session.ended` (last line on clean close).
  Exercise each event type in a test session; verify presence, position, and field correctness.

- [ ] **[MUST]** JSONL write is atomic: partial writes from a crash do not corrupt existing events.
  `kill -9` the daemon mid-write; restart; verify all events before the kill are readable and the partial line is discarded.

- [ ] **[MUST]** Session is fully recoverable from JSONL alone after deleting `state.db` and restarting.
  Delete `state.db`; restart daemon; verify `GET /agents/{id}/sessions/{session_id}/history` returns correct events.

- [ ] **[MUST]** `seq` numbers are monotonically increasing with no gaps within a session.
  Parse JSONL; assert `seq[n] == seq[n-1] + 1` for all lines.

- [ ] **[MUST]** `.index.json` sidecar is created and updated atomically alongside each JSONL file.
  Write several events; verify `.index.json` exists and `turn_count`, `event_count`, `total_tokens` are correct after each turn.

- [ ] **[MUST]** `title` auto-generated from first assistant response (first 60 chars, newlines stripped).
  Complete one turn; verify `title` field in `.index.json` and `GET /agents/{id}/sessions/{id}`.

- [ ] **[SHOULD]** Session branching: `POST /agents/{id}/sessions/{session_id}/branch` creates new session with `parent_session_id` set and identical prior history.
  Branch a session; verify new session ID, `parent_session_id` set, history identical up to branch point, new events do not appear in parent.

- [ ] **[SHOULD]** Session plugin loaded from shared library; `on_write`/`on_read` called on each event; panic caught and logged as `system` event.
  Load a test plugin that base64-encodes content; write events; verify stored bytes are encoded; read back and verify decoded. Inject panic; verify `system` event written and session continues.

---

## 5. Built-in Tools

- [ ] **[MUST]** All 13 built-in tools are implemented: `filesystem`, `process`, `apply_patch`, `agent_spawn`, `agent_spawn_status`, `agent_spawn_list`, `agents_list`, `agent_info`, `sessions_send`, `sessions_list`, `sessions_history`, `session_status`, `cron`.
  `GET /agents/{id}/chat` with a message asking the LLM to list available tools; verify all 13 appear. Alternatively: unit test each tool's registration.

- [ ] **[MUST]** Any built-in can be disabled via `disabled_tools`; disabled tool does not appear in LLM tool list.
  Set `disabled_tools = ["process"]`; verify `process` absent from tool list in LLM calls.

- [ ] **[MUST]** `filesystem` — all six operations work: `read`, `write`, `list`, `exists`, `delete`, `move`.
  Test each operation against the instance workspace; verify correct output and JSONL events.

- [ ] **[MUST]** `filesystem` — path outside sandbox is rejected with `SandboxViolation`.
  Attempt `filesystem.read` with `path: "../../etc/passwd"`; verify error, no file access, `SandboxViolation` in tool result.

- [ ] **[MUST]** `process` — shell command (`sh`, `bash`, `zsh`, `cmd`, `powershell`) as `command` is blocked.
  Call `process` with `command: "bash"`; verify rejected at parse time, not execution.

- [ ] **[MUST]** `process` — env vars matching `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD` are stripped from child process.
  Set `TEST_API_KEY=secret` in daemon env; call `process` with `command: "env"`; verify `TEST_API_KEY` absent from stdout.

- [ ] **[MUST]** `apply_patch` — all four operations work: `create`, `modify`, `delete`, `move`.
  Apply a patch with all four ops; verify files changed correctly; verify backup files created.

- [ ] **[MUST]** `apply_patch` — if any operation fails, all are rolled back atomically.
  Apply a patch where the second `modify` has no matching context; verify first `create` is also rolled back.

- [ ] **[MUST]** `agent_spawn` — team-scoped: spawned instance created in same team as parent.
  Spawn from a team agent; verify child instance has same `team_id`.

- [ ] **[MUST]** `agents_list` — returns only instances in the same team; standalone agent receives empty list.
  Call from team agent; verify only team peers returned. Call from standalone agent; verify empty list.

- [ ] **[MUST]** `sessions_send` — blocked when target session belongs to a team peer; returns `cross_agent_send_forbidden`.
  From team agent A, call `sessions_send` targeting team agent B's session; verify `cross_agent_send_forbidden` error.

- [ ] **[MUST]** `sessions_send` — succeeds when target is a non-team session (e.g. standalone instance or different team with grant).
  Call from standalone agent targeting another standalone agent's session; verify message delivered.

- [ ] **[MUST]** `cron` — all five sub-commands work: `at`, `every`, `cron`, `idle`, `event`, plus `list` and `cancel`.
  Register one job of each type; verify in `cron.list`; cancel each; verify removed from `cron.list`.

- [ ] **[MUST]** `cron` — registrations persisted to `cron.json`; survive daemon restart; jobs rescheduled on restart.
  Register a `cron` job; stop daemon; restart; verify job present in `cron.list` with updated `next_run_at`.

- [ ] **[MUST]** `cron` — missed `at` job (past `time`, never ran) fires immediately on restart.
  Register `at` job for 1 minute in the past; stop daemon before it fires; restart; verify session triggered within 30 seconds.

---

## 6. Custom Tools and MCP

- [ ] **[MUST]** Executable files in `tools/` are auto-discovered at instance startup and registered with the LLM.
  Place `test_tool.sh` (executable) in `tools/`; start instance; verify `test_tool` appears in LLM tool list.

- [ ] **[MUST]** Custom tool invoked via stdin/stdout JSON protocol; correct request envelope sent; response parsed.
  Implement a test tool that echoes its args; call it; verify `tool.call` and `tool.result` events in JSONL with correct args and output.

- [ ] **[MUST]** Optional `<toolname>.json` sidecar used as parameter schema when present.
  Add `test_tool.json` with a schema; verify LLM receives that schema in tool definition.

- [ ] **[MUST]** MCP server declared in `mcp.json` is started at instance startup; tools discovered via `list_tools`; tool calls proxied.
  Configure a test MCP server; start instance; verify MCP tools appear alongside built-ins; call one; verify result.

- [ ] **[MUST]** MCP server startup failure aborts instance startup with clear error.
  Point `mcp.json` at a non-existent command; attempt `pekobot run`; verify `mcp_server_start_failed` error, no partial startup.

- [ ] **[SHOULD]** MCP server crash detected within 1 second; restarted up to 3 times; marked unavailable after 3 failures; `system` event written; instance continues.
  Kill MCP server process 4 times; verify restart attempts logged; verify `system` event with `event: "mcp_unavailable"`; verify instance still responding.

- [ ] **[SHOULD]** Shared MCP server in `team.toml` started once; reference-counted; torn down when last agent disconnects.
  Deploy team with shared MCP; verify one MCP process in `ps`; stop all agents; verify MCP process gone.

---

## 7. Team Runtime and Event Bus

- [ ] **[MUST]** `pekobot team deploy -f team.toml` creates team workspace, starts all instances, starts shared services; `team.ready` event emitted when all agents are `running`.
  Deploy a 3-agent team; subscribe to `/events`; verify `team.ready` received; verify all instances `running`.

- [ ] **[MUST]** Per-agent workspace created at `.pekobot/teams/<team>/agents/<name>/`.
  Deploy team; verify directory structure on disk.

- [ ] **[MUST]** Shared file workspace at `.pekobot/teams/<t>/shared/files/` accessible to all agents via `filesystem` tool.
  Agent A writes `shared/files/test.txt`; Agent B reads it; verify content matches.

- [ ] **[MUST]** All A2A communication goes through the bus; `a2a.sent` and `a2a.received` events written to sender and receiver sessions respectively.
  Send a `Direct` message from agent A to agent B; verify `a2a.sent` in A's session and `a2a.received` in B's session.

- [ ] **[MUST]** All five A2A message types work: `Direct`, `Task`, `TaskResult`, `Broadcast`, `Subscribe`.
  Test each message type; verify delivery and correct session event written for each.

- [ ] **[MUST]** In-memory bus backend works for single-process teams.
  Deploy team with `backend = "in-memory"`; send messages; verify delivery.

- [ ] **[SHOULD]** Redis Streams backend works for multi-process teams.
  Deploy team with `backend = "redis"` pointing at a local Redis; send messages; restart one agent; verify pending messages delivered on reconnect.

- [ ] **[SHOULD]** NATS backend works.
  Deploy team with `backend = "nats"`; run basic A2A message test.

- [ ] **[SHOULD]** `pekobot team scale <team> <agent> <count>` adds or removes instances within 5 seconds without restarting the team.
  Deploy team with 2 researchers; scale to 5; verify 3 new instances start and join the bus within 5 seconds; scale to 1; verify 4 instances stop gracefully.

- [ ] **[MUST]** Team and standalone agents share the same daemon and runtime code path. No separate team-only runtime.
  Code review verification: single `AgentInstance` type handles both; no `if in_team` branching on core loop logic.

---

## 8. Registry

- [ ] **[MUST]** `pekobot build ./agent/ -t name:version` produces a package with a SHA-256 manifest digest.
  Build an image; verify manifest at `.pekobot/registry/images/<digest>/manifest.json`.

- [ ] **[MUST]** Build is deterministic: same source directory produces same digest.
  Build the same agent twice; verify both digests are identical.

- [ ] **[MUST]** Identical layers across different images are stored once (content-addressable deduplication).
  Build two agents sharing a large `projects/` directory; verify only one copy of the layer on disk.

- [ ] **[MUST]** `pekobot push <local> <remote>` uploads image to registry; `pekobot pull <remote>` downloads it; resulting image is runnable.
  Push to a test registry; pull on a fresh machine (or cleared cache); run the pulled image; verify response.

- [ ] **[MUST]** Pull streams progress events layer-by-layer; `done` event contains image object.
  Pull an image; capture SSE stream; verify `progress` events per layer, then `done`.

- [ ] **[MUST]** Registry auth: bearer token and HTTP Basic auth work for private registries.
  Configure a private registry with token auth; pull an image requiring auth; verify success. Provide wrong credentials; verify `registry_auth_failed`.

- [ ] **[SHOULD]** Package signing: `pekobot sign <image:tag>` signs manifest; signature verified on pull when configured.
  Sign an image; pull it; verify verification passes. Tamper with manifest; verify verification fails.

---

## 9. Interfaces

- [ ] **[MUST]** All CLI commands exit `0` on success and non-zero on error. No command prompts for input.
  Run every CLI command with `--help`; verify no interactive prompts. Run failing commands; verify non-zero exit.

- [ ] **[MUST]** All list/show commands support `--output json`; output is valid JSON on stdout; errors on stderr.
  `pekobot ps --output json | jq .` — succeeds without error.

- [ ] **[MUST]** `pekobot session show <session-id>` renders a human-readable timeline from JSONL.
  Run a session with tool calls; `pekobot session show <id>`; verify human-readable output includes all turns and tool calls.

- [ ] **[MUST]** `pekobot init ./agent/` creates minimal structure and `.gitignore` excluding `sessions/`, `workspace/`, `memories/`, `cron.json`.
  `pekobot init ./test-agent/`; verify directory structure; `cat .gitignore`; verify excluded paths.

- [ ] **[MUST]** Inbound webhook `POST /webhooks/{instance_id}/{token}` delivers payload as `hook.trigger` session event within the agent's active or new session.
  POST to webhook endpoint; verify `hook.trigger` event written with correct payload.

- [ ] **[SHOULD]** TUI (`pekobot-tui`) connects to daemon, displays running instances, allows interactive chat.
  Launch TUI; verify instance list shown; send a message; verify response displayed.

- [ ] **[SHOULD]** Web UI served at `http://localhost:11434/ui`; loads in browser; shows instance list; chat works.
  Open in browser; verify page loads; chat with an instance; verify response displayed.

- [ ] **[SHOULD]** WebSocket `ws://localhost:11434/agents/{id}/ws` sends `hello` on connect; accepts `message` frames; streams `delta`/`done` frames.
  Connect with `websocat`; send message frame; verify frame sequence matches spec.

---

## 10. Security

- [ ] **[MUST]** `process` tool strips all `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD` vars from child process environment.
  Set `ANTHROPIC_API_KEY=test` in daemon env; call `process` with `command: "env"`; verify key absent in output.

- [ ] **[MUST]** `filesystem` path traversal rejected after canonicalisation.
  Call `filesystem.read` with `../../../../etc/passwd`; verify `SandboxViolation` error; verify no file read occurred.

- [ ] **[MUST]** No credential appears in any session event, log line, or API response body.
  Run a session that triggers an LLM call; inspect full JSONL and all API responses; grep for `ANTHROPIC_API_KEY` value; verify absent.

- [ ] **[MUST]** `config.toml` validation rejects credential-shaped values in non-env fields.
  Set `[provider] api_key = "sk-..."` in `config.toml`; attempt `pekobot run`; verify startup refused with actionable error.

- [ ] **[MUST]** Daemon binds to `127.0.0.1` by default; `0.0.0.0` requires explicit config and prints a warning.
  Fresh start: `ss -tlnp | grep 11434` shows loopback only. Set `host = "0.0.0.0"`; restart; verify warning in stderr.

---

## 11. Reliability

- [ ] **[MUST]** Session event written before crash is fully recoverable after daemon restart.
  Write 100 events; `kill -9` daemon; restart; verify all 100 events readable from JSONL.

- [ ] **[MUST]** Instances in `starting`/`stopping` at crash time move to `stopped` with `error: "daemon_crash"` on restart.
  Kill daemon while instance is starting; restart; verify instance status `stopped`, error field set.

- [ ] **[MUST]** Cron jobs rescheduled from `cron.json` on restart; no job is silently lost.
  Register 3 cron jobs; kill daemon; restart; verify all 3 present in `cron.list` with updated `next_run_at`.

- [ ] **[MUST]** Tool failure does not crash the agentic loop; session continues.
  Tool that always panics; call it; verify session continues with `tool.result` error; send follow-up message; verify response.

- [ ] **[MUST]** Tool timeout does not hang the loop.
  Tool that sleeps forever with `timeout_ms: 200`; verify `tool.result` error within 500ms; loop continues.

---

## 12. Performance

- [ ] **[MUST]** Cold start: median < 500ms from `pekobot run` to first LLM call over 10 runs.
  `time pekobot run ./minimal-agent/ --detach`; measure 10 times; median < 500ms.

- [ ] **[MUST]** Warm start: < 100ms from `POST /agents` to `instance.started` event on system stream.
  Instrument daemon; measure `POST /agents` receipt to `instance.started` timestamp; 10 runs; median < 100ms.

- [ ] **[MUST]** Fast built-in tool latency: `filesystem.read` and `session_status` complete in < 5ms.
  Measure tool invocation to result; 100 calls each; p95 < 5ms.

- [ ] **[SHOULD]** In-memory bus round-trip < 1ms: `a2a.sent` to `a2a.received` timestamp delta in JSONL.
  Send 1000 Direct messages between two agents on in-memory bus; compute p95 latency from JSONL timestamps; p95 < 1ms.

- [ ] **[SHOULD]** 50 concurrent idle instances: memory usage and chat response time stable vs. 1 instance.
  Start 50 idle instances; chat with one; compare median response time and RSS to baseline of 1 instance; neither degrades by > 20%.

- [ ] **[SHOULD]** 5-agent team deploys in < 30 seconds including shared service startup.
  `time pekobot team deploy -f team.toml`; measure to `team.ready` event; < 30 seconds.

---

## 13. Documentation and Usability

- [ ] **[MUST]** Getting started guide exists and is accurate: fresh install → 4-line `config.toml` → `pekobot run` → first response in under 5 minutes.
  Walk through the guide on a clean machine; time it; verify < 5 minutes.

- [ ] **[MUST]** Every error code in `API_CONTRACT.md` §11.3 has a human-readable message with a suggested fix.
  Audit error message templates; verify 100% coverage.

- [ ] **[MUST]** Stack traces suppressed by default; shown only with `--debug` or `PEKOBOT_DEBUG=1`.
  Trigger a runtime error; verify no stack trace in default output. Repeat with `--debug`; verify stack trace shown.

- [ ] **[MUST]** `pekobot run` with malformed `config.toml` produces an actionable error, not a stack trace or panic.
  Write invalid TOML; `pekobot run`; verify human-readable error with line number and suggested fix.

- [ ] **[SHOULD]** All CLI commands have `--help` text with examples.
  `pekobot <command> --help` for every command; verify description and at least one example.

---

## Phase 1 Ship Gate

All **[MUST]** items above are checked.

All **[SHOULD]** items are either checked or have a recorded deferral decision with a tracking issue.

The following use cases from `REQUIREMENTS_SPEC.md` §5 pass end-to-end manually:
- [ ] UC-001: Solo Developer — Personal Assistant (P1 persona)
- [ ] UC-002: Automation Engineer — Cron-Triggered Pipeline (P2 persona)
- [ ] UC-003: Research Team — Multi-Agent Pipeline (P3 persona)
- [ ] UC-004: Platform Engineer — Internal Agent Infrastructure (P4 persona)
- [ ] UC-005: Integrator — Game NPC via WebSocket (S3 persona)

All checklist items that are checked are covered by automated tests in CI (not just manual verification).

No known security issues in categories covered by REQ-SC-001 through REQ-SC-004.

---

## Deferred Items Log

Use this table to record any **[SHOULD]** items deferred at ship time.

| Item | Section | Reason | Tracking Issue | Target |
|------|---------|--------|----------------|--------|
| | | | | |

---

*Version: 1.0 · Last Updated: 2026-03-16 · Status: Draft*
