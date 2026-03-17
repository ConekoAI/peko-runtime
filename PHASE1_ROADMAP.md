# Pekobot Phase 1 Implementation Roadmap

**Version:** 1.0  
**Date:** 2026-03-16  
**Status:** Draft  

This document outlines the implementation plan to achieve Phase 1 of Pekobot — a production-ready agent runtime. It maps the specification requirements to concrete development milestones.

---

## Overview

Phase 1 establishes the **Core Runtime** including: agent image/instance model, daemon with HTTP API, session management, built-in tools, team composition, and event bus. The goal is a stable, performant runtime that satisfies all **[MUST]** requirements from the specification.

### Current State Summary

| Component | Status | Notes |
|-----------|--------|-------|
| CLI Framework | ✅ Partial | Commands exist but need API alignment |
| Daemon | ✅ Partial | Cron, maintenance implemented; HTTP API Milestone 1 complete |
| Agent Runtime | ✅ Complete | Agentic loop v4 with sync/async tools, SSE, WebSocket |
| Session Management | ✅ Complete | Atomic writes, sidecar indexes, branching, recovery |
| Core Runtime | ✅ Complete | Agentic loop, sync/async tools, SSE streaming, WebSocket, watch mode |
| Tools (13 built-in) | ✅ Partial | Filesystem, process, apply_patch exist; some need completion |
| MCP Support | ✅ Partial | Client exists; integration needs alignment |
| HTTP API | ✅ Milestone 1 Complete | Foundation: /health, /info endpoints, headers, middleware |
| Agent Image/Instance | ✅ Milestone 2 Complete | Image build, registry, instance lifecycle |
| Team Runtime | ❌ Not Started | Milestone 7 |
| Event Bus | ❌ Not Started | Milestone 7 |
| Registry/Packaging | ✅ Partial | Local registry complete, push/pull pending |

---

## Milestone 1: HTTP API Server Foundation ✅ COMPLETE

**Goal:** Implement the daemon HTTP API as the single control point for all runtime operations.

**Duration:** 2 weeks  
**Dependencies:** None  
**Completed:** 2026-03-16  

### Tasks

| Task | Description | Spec Ref | Status |
|------|-------------|----------|--------|
| 1.1 | Create `src/api/` module with Axum-based HTTP server | API_CONTRACT §1-2 | ✅ Complete |
| 1.2 | Implement `GET /health` and `GET /info` endpoints | API_CONTRACT §10 | ✅ Complete |
| 1.3 | Implement `X-Pekobot-Version` and `X-Request-ID` headers | API_CONTRACT §1.3-1.6 | ✅ Complete |
| 1.4 | Implement standard error envelope `{error: {code, message, request_id, details}}` | API_CONTRACT §11 | ✅ Complete |
| 1.5 | Create API request/response types with validation | DATA_MODEL §12 | ✅ Complete |
| 1.6 | Add graceful shutdown handling | REQ-DM-001 | ✅ Complete |

### Deliverables
- HTTP server listening on `127.0.0.1:11435` by default
- Health check endpoint returning structured status
- Consistent error handling across all endpoints
- Warning logged when binding to non-loopback address

### Verification
```bash
pekobot daemon start
curl http://localhost:11435/health  # Returns {"status":"ok",...}
curl http://localhost:11435/info    # Returns version, workspace, etc.
```

---

## Milestone 2: Agent Image and Instance Model ✅ COMPLETE

**Goal:** Implement the image/instance distinction with filesystem-first agent definition.

**Duration:** 2 weeks  
**Dependencies:** Milestone 1  
**Completed:** 2026-03-16  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 2.1 | Create `src/image/` module for image manifest management | DATA_MODEL §6 | ✅ Complete |
| 2.2 | Implement `config.toml` loader with validation | DATA_MODEL §2 | ✅ Complete |
| 2.3 | Implement image build (`POST /images/build`) with SHA-256 digests | REQ-AI-003 | ✅ Complete |
| 2.4 | Implement `.pekobot/registry/images/` content-addressable storage | UNIFIED_ARCH §2.3 | ✅ Complete |
| 2.5 | Refactor `src/agent/` to separate Image vs Instance concepts | REQ-AI-002 | ✅ Complete |
| 2.6 | Implement instance pinning to image digest | REQ-AI-005 | ✅ Complete |
| 2.7 | Implement `POST /agents` (create instance from image) | API_CONTRACT §3.2 | ✅ Complete |
| 2.8 | Implement `GET /agents`, `GET /agents/{id}`, `DELETE /agents/{id}` | API_CONTRACT §3 | ✅ Complete |
| 2.9 | Ensure `sessions/` is never included in image | REQ-AI-001 | ✅ Complete |

### Deliverables
- Agent images built with deterministic SHA-256 digests
- Instances created from images with pinned digests
- Image storage with layer deduplication
- Full instance lifecycle API

### Verification
```bash
pekobot build ./my-agent/ -t my-agent:v1.0
pekobot run my-agent:v1.0 --detach  # Creates instance
pekobot ps                           # List instances with digests
```

---

## Milestone 3: Session Management ✅ COMPLETE

**Goal:** Implement durable JSONL sessions with atomic writes and index files.

**Duration:** 2 weeks  
**Dependencies:** Milestone 2  
**Completed:** 2026-03-17  

### Tasks

| Task | Description | Spec Ref | Status |
|------|-------------|----------|--------|
| 3.1 | Refactor `src/session/jsonl.rs` for atomic writes (tmp + rename) | REQ-SM-001 | ✅ Complete |
| 3.2 | Implement all 13 event types in JSONL | DATA_MODEL §5.3 | ✅ Complete |
| 3.3 | Implement `.index.json` sidecar generation | REQ-SM-004 | ✅ Complete |
| 3.4 | Implement `GET /agents/{id}/sessions` | API_CONTRACT §5.1 | ✅ Complete |
| 3.5 | Implement `GET /agents/{id}/sessions/{id}/history` | API_CONTRACT §5.3 | ✅ Complete |
| 3.6 | Implement `POST /agents/{id}/sessions/{id}/branch` | REQ-SM-003 | ✅ Complete |
| 3.7 | Implement session state recovery on daemon restart | REQ-RL-003 | ✅ Complete |
| 3.8 | Auto-generate title from first assistant response | REQ-SM-004 | ✅ Complete |
| 3.9 | Add SQLite index as read-optimized cache (rebuildable from JSONL) | REQ-SM-001 | ⏸️ Deferred - sidecar index sufficient for Phase 1 |

### Deliverables
- Atomic JSONL writes with no corruption on crash
- All session events properly serialized
- Fast metadata lookup via index files
- Session branching support

### Verification
```bash
# Verify atomic writes
kill -9 $(cat .pekobot/run/daemon.pid)
# Restart daemon, verify sessions intact
```

---

## Milestone 4: Core Runtime and Agentic Loop

**Goal:** Implement the turn-based agentic loop with sync/async tool calling.

**Duration:** 2 weeks  
**Dependencies:** Milestone 3  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 4.1 | Refactor `src/engine/loop_v4.rs` to handle three input sources | REQ-AR-001 |
| 4.2 | Implement synchronous tool execution (blocking) | REQ-AR-002 |
| 4.3 | Implement asynchronous tool execution (receipt + callback) | REQ-AR-002 |
| 4.4 | Implement tool timeout handling | REQ-RL-002 |
| 4.5 | Implement tool panic isolation | REQ-RL-002 |
| 4.6 | Implement `POST /agents/{id}/chat` with SSE streaming | API_CONTRACT §4 |
| 4.7 | Implement WebSocket chat endpoint `ws://localhost:11435/agents/{id}/ws` | REQ-DM-003 |
| 4.8 | Add streaming first token latency (< 500ms target) | REQ-PF-003 |
| 4.9 | Implement watch mode (`--watch`) for development | REQ-AR-005 |
| 4.10 | Complete all 4 LLM providers: Anthropic, OpenAI, Ollama, OpenAI-compatible | REQ-AR-004 |

### Deliverables
- Agentic loop with proper turn handling
- Sync and async tool execution
- SSE streaming chat responses
- WebSocket bidirectional chat
- Watch mode for development

### Verification
```bash
pekobot run ./agent/ --watch  # Auto-reload on file changes
# In another terminal:
curl -N http://localhost:11435/agents/{id}/chat \
  -H "Content-Type: application/json" \
  -d '{"message":"Hello"}'  # Streams SSE events
```

---

## Milestone 5: Built-in Tools Completion

**Goal:** Implement all 13 required built-in tools with proper sandboxing.

**Duration:** 2 weeks  
**Dependencies:** Milestone 4  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 5.1 | Refactor `filesystem` tool with full sandbox enforcement | CAPABILITY §3.1 |
| 5.2 | Refactor `process` tool (block shells, strip env vars) | CAPABILITY §3.2 |
| 5.3 | Complete `apply_patch` tool (atomic, rollback, backup) | CAPABILITY §3.3 |
| 5.4 | Complete `agent_spawn` (sync and async) | CAPABILITY §3.4 |
| 5.5 | Implement `agent_spawn_status` | CAPABILITY §3.5 |
| 5.6 | Implement `agent_spawn_list` | CAPABILITY §3.6 |
| 5.7 | Implement `agents_list` (team-scoped) | CAPABILITY §3.7 |
| 5.8 | Implement `agent_info` | CAPABILITY §3.8 |
| 5.9 | Implement `sessions_send` (with cross-team blocking) | CAPABILITY §3.9 |
| 5.10 | Implement `sessions_list` | CAPABILITY §3.10 |
| 5.11 | Implement `sessions_history` | CAPABILITY §3.11 |
| 5.12 | Implement `session_status` | CAPABILITY §3.12 |
| 5.13 | Implement `cron` tool (all 5 sub-commands + persistence) | CAPABILITY §3.13 |
| 5.14 | Implement tool capability sandbox enforcement | REQ-CAP-005 |
| 5.15 | Add `disabled_tools` support | REQ-CAP-001 |

### Deliverables
- All 13 built-in tools functional
- Path sandboxing enforced
- Process env var stripping
- Tool disable capability

### Verification
```bash
# Test sandboxing
curl http://localhost:11435/agents/{id}/chat -d '{
  "message": "Read file at ../../../../etc/passwd"
}'  # Should get SandboxViolation
```

---

## Milestone 6: Custom Tools and MCP Integration

**Goal:** Implement custom tool discovery and MCP client support.

**Duration:** 1.5 weeks  
**Dependencies:** Milestone 5  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 6.1 | Implement `tools/` directory discovery | REQ-CAP-002 |
| 6.2 | Implement custom tool JSON protocol (stdin/stdout) | DATA_MODEL §10 |
| 6.3 | Support optional `<toolname>.json` schema sidecar | REQ-CAP-002 |
| 6.4 | Implement MCP client in `src/mcp/` | REQ-CAP-003 |
| 6.5 | Implement `mcp.json` parsing | DATA_MODEL §9 |
| 6.6 | Implement MCP tool discovery (`list_tools`) | REQ-CAP-003 |
| 6.7 | Implement MCP tool call proxying | REQ-CAP-003 |
| 6.8 | Add MCP server startup failure handling | REQ-CAP-003 |
| 6.9 | Implement capability resolution order (built-in → local → MCP) | REQ-CAP-004 |

### Deliverables
- Custom tools from `tools/` directory work
- MCP servers integrate seamlessly
- Tool name conflicts resolved per spec

---

## Milestone 7: Team Runtime and Event Bus

**Goal:** Implement multi-agent teams with shared services and A2A communication.

**Duration:** 2.5 weeks  
**Dependencies:** Milestone 6  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 7.1 | Implement `team.toml` parser | DATA_MODEL §4 |
| 7.2 | Create `src/team/` module for team management | UNIFIED_ARCH §6 |
| 7.3 | Implement `POST /teams` (deploy from config) | API_CONTRACT §6.2 |
| 7.4 | Implement `GET /teams`, `GET /teams/{id}`, `DELETE /teams/{id}` | API_CONTRACT §6 |
| 7.5 | Implement in-memory event bus backend | REQ-BUS-002 |
| 7.6 | Implement all 5 A2A message types | REQ-BUS-001 |
| 7.7 | Implement `a2a.sent` and `a2a.received` session events | REQ-BUS-003 |
| 7.8 | Implement shared file workspace | REQ-TR-002 |
| 7.9 | Implement shared MCP server reference counting | REQ-TR-002 |
| 7.10 | Implement team workspace directory structure | UNIFIED_ARCH §2.1 |
| 7.11 | Implement `POST /teams/{id}/scale` | REQ-TR-003 |
| 7.12 | Ensure unified runtime (no separate team runtime) | REQ-TR-004 |

### Deliverables
- Teams deployable from `team.toml`
- A2A communication via event bus
- Shared services (files, MCPs) working
- Horizontal scaling support

### Verification
```bash
pekobot team deploy -f team.toml
pekobot team ps
pekobot team scale my-team researcher 5
```

---

## Milestone 8: Outbound Hooks and System Events

**Goal:** Implement cron, webhook, event, and file_watch hooks.

**Duration:** 1.5 weeks  
**Dependencies:** Milestone 7  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 8.1 | Refactor existing cron implementation to match spec | REQ-DM-004 |
| 8.2 | Implement `cron.json` persistence | REQ-CAP-007 |
| 8.3 | Implement missed job handling on restart | REQ-CAP-007 |
| 8.4 | Implement webhook server in orchestration layer | REQ-DM-004 |
| 8.5 | Implement `POST /webhooks/{instance_id}/{token}` | API_CONTRACT §9 |
| 8.6 | Implement webhook token validation | REQ-UI-004 |
| 8.7 | Implement file watcher hook | REQ-DM-004 |
| 8.8 | Implement event-triggered hook (event bus integration) | REQ-DM-004 |
| 8.9 | Implement system event stream `ws://localhost:11435/events` | REQ-DM-005 |
| 8.10 | Emit all lifecycle events on system stream | API_CONTRACT §8.4 |

### Deliverables
- All 4 hook types functional
- Webhook endpoint with token auth
- System event stream WebSocket
- Cron persistence across restarts

---

## Milestone 9: Registry and Image Distribution

**Goal:** Implement image packaging, push/pull, and registry client.

**Duration:** 2 weeks  
**Dependencies:** Milestone 8  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 9.1 | Refactor `src/portable/` for OCI-inspired packaging | REQ-RG-001 |
| 9.2 | Implement layer compression (gzip tar) | DATA_MODEL §6.3 |
| 9.3 | Implement content-addressable layer storage | REQ-RG-001 |
| 9.4 | Implement `POST /images/pull` with streaming progress | API_CONTRACT §7.3 |
| 9.5 | Implement `POST /images/push` with streaming progress | API_CONTRACT §7.5 |
| 9.6 | Implement registry client with bearer token auth | REQ-RG-002 |
| 9.7 | Implement registry client with HTTP Basic auth | REQ-RG-002 |
| 9.8 | Support multiple registry sources in `runtime.toml` | DATA_MODEL §3 |
| 9.9 | Implement package signing (optional, **[SHOULD]**) | REQ-RG-003 |

### Deliverables
- Images buildable with layer deduplication
- Push/pull to registries works
- Authentication support
- Streaming progress events

### Verification
```bash
pekobot build ./agent/ -t my-agent:v1.0
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0
pekobot pull pekohub.com/agents/base:v1
```

---

## Milestone 10: CLI Completion and Interfaces

**Goal:** Complete CLI commands, TUI, and Web UI.

**Duration:** 2 weeks  
**Dependencies:** Milestone 9  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 10.1 | Refactor CLI to use HTTP API (not direct calls) | REQ-UI-001 |
| 10.2 | Ensure all commands are non-interactive | REQ-UI-001 |
| 10.3 | Implement `--output json` for all list/show commands | REQ-UI-001 |
| 10.4 | Add proper exit codes (0 success, non-zero error) | REQ-US-004 |
| 10.5 | Implement `pekobot init ./agent/` command | REQ-US-001 |
| 10.6 | Implement `pekobot session show <session-id>` | REQ-SC-003 |
| 10.7 | Create TUI (`pekobot-tui` binary) | REQ-UI-002 |
| 10.8 | Create Web UI (embedded HTML) | REQ-UI-003 |
| 10.9 | Ensure Web UI served at `/ui` | REQ-UI-003 |
| 10.10 | Complete WebSocket service endpoint | REQ-UI-005 |
| 10.11 | Add `--debug` flag for stack traces | REQ-US-003 |

### Deliverables
- Complete CLI with JSON output
- TUI for terminal interaction
- Web UI for browser interaction
- Non-interactive, scriptable commands

---

## Milestone 11: Security and Hardening

**Goal:** Implement all security requirements and sandboxing.

**Duration:** 1 week  
**Dependencies:** Milestone 10  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 11.1 | Verify `process` tool strips all sensitive env vars | REQ-SC-002 |
| 11.2 | Verify credentials never appear in sessions/logs | REQ-SC-002 |
| 11.3 | Add `config.toml` credential detection (reject API keys in config) | REQ-SC-002 |
| 11.4 | Verify filesystem path traversal rejection | REQ-SC-001 |
| 11.5 | Verify symlink handling in sandbox | CAPABILITY §7.2 |
| 11.6 | Ensure localhost-only default binding | REQ-SC-004 |
| 11.7 | Implement audit logging for all agent actions | REQ-SC-003 |
| 11.8 | Security review: no credential leakage in API responses | REQ-SC-002 |

### Deliverables
- All security requirements verified
- Sandbox properly enforced
- No credential leakage

---

## Milestone 12: Performance Optimization and Testing

**Goal:** Meet all performance targets and pass end-to-end use cases.

**Duration:** 1.5 weeks  
**Dependencies:** Milestone 11  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 12.1 | Optimize cold start to < 500ms | REQ-PF-001 |
| 12.2 | Optimize warm start to < 100ms | REQ-PF-002 |
| 12.3 | Optimize streaming first token to < 500ms | REQ-PF-003 |
| 12.4 | Optimize built-in tool latency to < 5ms | REQ-PF-004 |
| 12.5 | Test 50 concurrent instances stability | REQ-PF-006 |
| 12.6 | Test team deploy < 30 seconds | REQ-PF-007 |
| 12.7 | Implement comprehensive integration tests | REQ-RL-001 |
| 12.8 | Pass UC-001: Solo Developer use case | REQUIREMENTS §5 |
| 12.9 | Pass UC-002: Automation Engineer use case | REQUIREMENTS §5 |
| 12.10 | Pass UC-003: Research Team use case | REQUIREMENTS §5 |
| 12.11 | Pass UC-004: Platform Engineer use case | REQUIREMENTS §5 |
| 12.12 | Pass UC-005: Integrator use case | REQUIREMENTS §5 |

### Deliverables
- All performance targets met
- All use cases passing
- Comprehensive test coverage

---

## Milestone 13: Documentation and Polish

**Goal:** Complete documentation and finalize Phase 1.

**Duration:** 1 week  
**Dependencies:** Milestone 12  

### Tasks

| Task | Description | Spec Ref |
|------|-------------|----------|
| 13.1 | Write Getting Started guide | REQ-US-001 |
| 13.2 | Document all error codes with fix suggestions | REQ-US-002 |
| 13.3 | Add `--help` examples for all commands | REQ-US-003 |
| 13.4 | Create API usage examples | API_CONTRACT |
| 13.5 | Write architecture overview for contributors | - |
| 13.6 | Complete inline code documentation | - |
| 13.7 | Final CHANGELOG for Phase 1 | - |
| 13.8 | Review and defer **[SHOULD]** items if needed | PHASE1_CHECKLIST |

---

## Implementation Timeline

```
Week  1-2:  Milestone 1  - HTTP API Server Foundation
Week  3-4:  Milestone 2  - Agent Image and Instance Model
Week  5-6:  Milestone 3  - Session Management
Week  7-8:  Milestone 4  - Core Runtime and Agentic Loop
Week  9-10: Milestone 5  - Built-in Tools Completion
Week  11:   Milestone 6  - Custom Tools and MCP Integration
Week  12-13: Milestone 7 - Team Runtime and Event Bus
Week  14:   Milestone 8  - Outbound Hooks and System Events
Week  15-16: Milestone 9 - Registry and Image Distribution
Week  17-18: Milestone 10 - CLI Completion and Interfaces
Week  19:   Milestone 11 - Security and Hardening
Week  20:   Milestone 12 - Performance and Testing
Week  21:   Milestone 13 - Documentation and Polish
```

**Total Duration:** ~21 weeks (5 months) with 1 developer  
**Parallelizable:** Milestones 5, 6, and 8 can have some overlap  
**With 2 developers:** ~12-14 weeks achievable

---

## Key Architectural Decisions

### 1. HTTP API First
All runtime operations go through the HTTP API. The CLI, TUI, and Web UI are all HTTP clients. This ensures:
- Single control point (daemon)
- Language-agnostic integration
- Stable contract (API_CONTRACT.md)

### 2. JSONL as Source of Truth
Session JSONL files are the authoritative storage. SQLite is a read-optimized cache that can be rebuilt from JSONL. This ensures:
- Human-readable, git-diffable sessions
- Full recovery after crashes
- No complex database migrations

### 3. Unified Runtime
Team agents and standalone agents use the same runtime code. A team agent is simply an instance with a `team_id` field. This ensures:
- No code duplication
- Consistent behavior
- Simpler testing

### 4. Content-Addressable Images
Images use SHA-256 digests for layer storage. Identical content is stored once. This ensures:
- Efficient storage
- Deterministic builds
- Natural deduplication

---

## Risk Areas

| Risk | Mitigation |
|------|------------|
| HTTP API performance | Use Axum (already chosen), benchmark early in Milestone 4 |
| Session atomic writes | Implement early, stress test with crash injection |
| Team event bus scaling | Start with in-memory, Redis/NATS can be added later |
| MCP server stability | Implement crash detection/restart (SHOULD item, can defer) |
| 50 concurrent instances | Test memory usage early, optimize if needed |

---

## Deferred Items (Phase 2/3)

Per specification, these are explicitly out of scope for Phase 1:

| Item | Phase | Reason |
|------|-------|--------|
| Control Plane (lifecycle policies, scheduling) | Phase 2 | Runtime foundation needed first |
| Resource enforcement (cgroups) | Phase 2 | Requires control plane |
| Capability package manager (`pekobot install`) | Phase 3 | Ecosystem maturity needed |
| Auto-install from dependencies | Phase 3 | Requires package manager |
| Redis/NATS bus backends | Phase 1 (SHOULD) | In-memory sufficient for single-node |
| Session plugins | Phase 1 (SHOULD) | Can use raw sessions initially |
| Package signing | Phase 1 (SHOULD) | Verification warning mode acceptable |

---

## Success Criteria

Phase 1 is complete when:

1. ✅ All **[MUST]** items in `PHASE1_CHECKLIST.md` are checked
2. ✅ All **[SHOULD]** items are checked or have documented deferrals
3. ✅ All 5 use cases (UC-001 through UC-005) pass end-to-end
4. ✅ All performance targets are met
5. ✅ No known security issues in REQ-SC-001 through REQ-SC-004
6. ✅ CI passes with automated tests for all checked items
7. ✅ Getting Started guide validated on clean machine in < 5 minutes

---

## Appendix: Module Structure

Proposed final module structure after all milestones:

```
src/
├── api/              # HTTP API server (Axum) - NEW
│   ├── routes/       # Endpoint handlers
│   ├── middleware/   # Auth, logging, headers
│   └── websocket/    # WS handlers
├── agent/            # Agent runtime (REFACTORED)
│   ├── image.rs      # Image definition/manifest
│   ├── instance.rs   # Instance lifecycle
│   └── manager.rs    # Instance management
├── team/             # Team runtime - NEW
│   ├── mod.rs
│   ├── bus/          # Event bus backends
│   └── shared/       # Shared services fabric
├── engine/           # Agentic loop (REFACTORED)
├── session/          # Session management (REFACTORED)
├── tools/            # Built-in tools (COMPLETED)
├── mcp/              # MCP client (COMPLETED)
├── registry/         # Image registry client - NEW
├── daemon/           # Daemon (REFACTORED to use HTTP API)
├── commands/         # CLI (REFACTORED to use HTTP API)
├── tui/              # Terminal UI - NEW
└── web_ui/           # Embedded web UI - NEW
```

---

*Version: 1.0 · Last Updated: 2026-03-16 · Status: Draft*
