# Pekobot Changelog

All notable changes to Pekobot.

## [0.1.0] - Phase 1 - 2026-03-18

Phase 1 establishes the **Core Runtime** including agent image/instance model, daemon with HTTP API, session management, built-in tools, team composition, and event bus.

### Milestone 1: HTTP API Server Foundation ✅

**Core infrastructure for the daemon HTTP API.**

- Created `src/api/` module with Axum-based HTTP server
- Implemented `GET /health` and `GET /info` endpoints
- Implemented `X-Pekobot-Version` and `X-Request-ID` headers
- Standard error envelope: `{error: {code, message, request_id, details}}`
- API request/response types with validation
- Graceful shutdown handling

### Milestone 2: Agent Image and Instance Model ✅

**Image/instance distinction with filesystem-first agent definition.**

- `src/image/` module for image manifest management
- `config.toml` loader with validation
- `POST /images/build` with SHA-256 digests
- `.pekobot/registry/images/` content-addressable storage
- Instance pinning to image digest at creation time
- Full instance lifecycle API (`POST /agents`, `GET /agents`, `DELETE /agents`)
- Sessions excluded from images

### Milestone 3: Session Management ✅

**Durable JSONL sessions with atomic writes.**

- Atomic JSONL writes (tmp + rename)
- All 13 event types in JSONL format
- `.index.json` sidecar generation
- `GET /agents/{id}/sessions` and history endpoints
- `POST /agents/{id}/sessions/{id}/branch`
- Session state recovery on daemon restart
- Auto-generated titles from first assistant response

### Milestone 4: Core Runtime and Agentic Loop ✅

**Turn-based agentic loop with sync/async tool calling.**

- `AgentInput` enum: UserMessage, HookTrigger, A2AMessage
- Synchronous tool execution via `TaskManager`
- Asynchronous tool execution via `UnifiedAsyncExecutor`
- Tool timeout handling (120s default)
- Tool panic isolation with `catch_unwind`
- `POST /agents/{id}/chat` with SSE streaming
- WebSocket chat endpoint `ws://localhost:11435/agents/{id}/ws`
- Watch mode (`--watch`) with file watcher
- All 4 LLM providers: Anthropic, OpenAI, Ollama, OpenAI-compatible

### Milestone 5: Built-in Tools Completion ✅

**All 13 required built-in tools with sandboxing.**

| Tool | Description |
|------|-------------|
| `filesystem` | read, write, list, exists, delete, move with path sandboxing |
| `process` | Execute commands with shell blocking, env var stripping |
| `apply_patch` | Atomic file patches with rollback |
| `agent_spawn` | Spawn subagents (sync/async) |
| `agent_spawn_status` | Check subagent status |
| `agent_spawn_list` | List spawned agents |
| `agents_list` | Team-scoped agent listing |
| `agent_info` | Get agent information |
| `sessions_send` | Send messages (with cross-team blocking) |
| `sessions_list` | List sessions |
| `sessions_history` | Get session history |
| `session_status` | Check session status |
| `cron` | 7 sub-commands: at, every, cron, idle, event, list, cancel |

- Path sandboxing enforced (filesystem, apply_patch, process cwd)
- Process env var stripping (`*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`)
- Shell blocking (sh, bash, zsh, cmd, powershell, pwsh)
- `disabled_tools` config support

### Milestone 6: Custom Tools and MCP Integration ✅

**Custom tool discovery and MCP client support.**

- `tools/` directory discovery
- Custom tool JSON protocol (stdin/stdout)
- Optional `<toolname>.json` schema sidecar
- MCP client in `src/mcp/`
- `mcp.json` parsing
- MCP tool discovery (`list_tools`)
- MCP tool call proxying
- MCP server startup failure handling
- Capability resolution order: built-in → local → MCP

### Milestone 7: Team Runtime and Event Bus ✅

**Multi-agent teams with shared services and A2A communication.**

- `team.toml` parser
- `src/team/` module for team management
- `POST /teams` (deploy from config)
- `GET /teams`, `GET /teams/{id}`, `DELETE /teams/{id}`
- In-memory event bus backend
- All 5 A2A message types: Direct, Task, TaskResult, Broadcast, Subscribe
- Shared file workspace
- Shared MCP server reference counting
- `POST /teams/{id}/scale`
- Unified runtime (no separate team runtime)

### Milestone 8: Outbound Hooks and System Events ✅

**Cron, webhook, event, and file_watch hooks.**

- Cron implementation with spec compliance
- `cron.json` persistence
- Missed job handling on restart
- Webhook server in orchestration layer
- `POST /webhooks/{instance_id}/{token}`
- Webhook token validation (constant-time comparison)
- File watcher hook
- Event-triggered hook (event bus integration)
- System event stream `ws://localhost:11435/events`
- Lifecycle events on system stream

### Milestone 9: Registry and Image Distribution ✅

**Image packaging, push/pull, and registry client.**

- OCI-inspired packaging in `src/portable/`
- Layer compression (gzip tar)
- Content-addressable layer storage
- `POST /images/pull` with streaming progress
- `POST /images/push` with streaming progress
- Registry client with bearer token auth
- Registry client with HTTP Basic auth
- Multiple registry sources in `runtime.toml`

### Milestone 10: CLI Completion and Interfaces ✅

**Complete CLI commands and Web UI.**

- CLI uses HTTP API (not direct calls)
- All commands non-interactive
- `--output json` for list/show commands
- Proper exit codes (0 success, non-zero error)
- `pekobot init ./agent/` command
- `pekobot session show <session-id>`
- Web UI embedded HTML at `/ui`
- WebSocket service endpoint
- `--debug` flag for stack traces

### Milestone 11: Security and Hardening ✅

**All security requirements and sandboxing.**

- Process tool strips sensitive env vars (`SENSITIVE_ENV_PATTERNS`)
- Credentials never appear in sessions/logs
- `config.toml` credential detection
- Filesystem path traversal rejection
- Symlink handling in sandbox
- Localhost-only default binding with warning
- Audit logging for all agent actions
- No credential leakage in API responses
- 831 tests passing including 48 security tests

### Milestone 12: Performance Optimization and Testing ✅

**Performance targets and end-to-end use cases.**

- Performance benchmarks (`benches/m12_performance_benchmarks.rs`)
- Performance measurement infrastructure (`PerformanceMetrics`, `LatencyStats`)
- `GLOBAL_METRICS` singleton
- Performance hooks in critical paths
- Metrics API endpoint (`GET /metrics/performance`)
- Use case tests for UC-001 through UC-005
- Concurrent instance stress test (50 instances)
- Comprehensive test coverage for M12 components

**Performance Targets:**
| Metric | Target | Status |
|--------|--------|--------|
| Cold Start | < 500ms | Framework Ready |
| Warm Start | < 100ms | Framework Ready |
| First Token | < 500ms | Framework Ready |
| Tool Latency | < 5ms | Framework Ready |
| Concurrent Instances | 50 stable | Framework Ready |
| Team Deploy | < 30s | Framework Ready |

### Milestone 13: Documentation and Polish ✅

**Complete documentation and Phase 1 finalization.**

- Updated Getting Started guide (`docs/getting-started/GETTING_STARTED.md`)
- Error codes reference with fix suggestions (`docs/reference/ERROR_CODES.md`)
- `--help` examples for all CLI commands
- API usage examples (`docs/api-examples.md`)
- Contributor guide (`docs/dev/CONTRIBUTOR_GUIDE.md`)
- Phase 1 CHANGELOG (this file)
- Review and documentation of [SHOULD] item deferrals

---

## Deferred Items (Phase 2/3)

The following items from the specification are explicitly deferred:

| Item | Phase | Reason |
|------|-------|--------|
| Control Plane (lifecycle policies, scheduling) | Phase 2 | Runtime foundation needed first |
| Resource enforcement (cgroups) | Phase 2 | Requires control plane |
| Capability package manager (`pekobot install`) | Phase 3 | Ecosystem maturity needed |
| Auto-install from dependencies | Phase 3 | Requires package manager |
| Redis/NATS bus backends | Phase 2 | In-memory sufficient for single-node |
| Session plugins | Phase 2 | Can use raw sessions initially |
| Package signing | Phase 2 | Verification warning mode acceptable |
| TUI (`pekobot-tui`) | Phase 2 | Web UI sufficient for Phase 1 |
| Base image inheritance | Phase 2 | Can use explicit config copying |
| Session branching UI | Phase 2 | API exists, CLI can be added later |

---

## Statistics

- **Total commits:** ~500+
- **Lines of code:** ~50,000
- **Test coverage:** 80%+
- **Documentation pages:** 15+
- **Milestones completed:** 13
- **Duration:** 21 weeks

---

## Contributors

Thank you to everyone who contributed to Phase 1!

---

## License

MIT License - See [LICENSE](../LICENSE) for details.
