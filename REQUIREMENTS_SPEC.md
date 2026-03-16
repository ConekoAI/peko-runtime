# Pekobot — Requirements Specification

**Version:** 2.0
**Date:** 2026-03-16
**Status:** Draft
**Supersedes:** v1.0 (2026-03-15)
**Companion docs:** `UNIFIED_ARCHITECTURE_SPEC.md` v4.0, `API_CONTRACT.md` v1.0, `DATA_MODEL.md` v1.0, `CAPABILITY_INTERFACE.md` v1.0

This document translates business and design requirements into testable technical requirements for Pekobot — a filesystem-first runtime for AI agent systems. Requirements are organised by domain and tagged with a phase (Phase 1 = runtime, Phase 2 = control plane, Phase 3 = capability ecosystem).

Changes from v1.0 are summarised in the [Revision Notes](#revision-notes).

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [User Personas](#2-user-personas)
3. [Functional Requirements](#3-functional-requirements)
   - [3.1 Agent Image Model](#31-agent-image-model-req-ai)
   - [3.2 Agent Instance and Runtime](#32-agent-instance-and-runtime-req-ar)
   - [3.3 Daemon and HTTP API](#33-daemon-and-http-api-req-dm)
   - [3.4 Session Management](#34-session-management-req-sm)
   - [3.5 Built-in Capability System](#35-built-in-capability-system-req-cap)
   - [3.6 Team Runtime](#36-team-runtime-req-tr)
   - [3.7 Event Bus](#37-event-bus-req-bus)
   - [3.8 Registry](#38-registry-req-rg)
   - [3.9 Interfaces](#39-interfaces-req-ui)
   - [3.10 Control Plane (Phase 2)](#310-control-plane-req-cp)
   - [3.11 Capability Ecosystem (Phase 3)](#311-capability-ecosystem-req-ce)
4. [Non-Functional Requirements](#4-non-functional-requirements)
5. [Use Cases](#5-use-cases)
6. [Success Criteria](#6-success-criteria)
7. [Constraints](#7-constraints)
8. [Out of Scope](#8-out-of-scope)
9. [Glossary](#9-glossary)
10. [Revision Notes](#revision-notes)

---

## 1. Introduction

### 1.1 Purpose

This document defines what Pekobot must do, organised so that each requirement is independently testable and traceable to a specific design decision. It is the basis for implementation planning, acceptance testing, and phase gating.

### 1.2 Scope

Phase 1 (runtime) requirements are fully specified and represent the acceptance criteria for the initial release. Phase 2 (control plane) and Phase 3 (capability ecosystem) requirements are specified at a level sufficient to ensure Phase 1 does not foreclose them — they will be expanded in dedicated specs before implementation begins.

### 1.3 Requirement Priorities

| Priority | Meaning |
|----------|---------|
| **Must Have** | Phase cannot ship without this. Blocking. |
| **Should Have** | High value; included unless time-constrained. |
| **Could Have** | Nice to have; deferred if needed. |
| **Phase 2 / 3** | Explicitly deferred; architecture must not preclude it. |

---

## 2. User Personas

### 2.1 Primary Users

| ID | Persona | Description | Key Needs |
|----|---------|-------------|-----------|
| P1 | Solo Developer | Engineers building personal agents, automation tools, or conversational interfaces | Fast iteration, minimal config, zero friction |
| P2 | Automation Engineer | Professionals creating multi-step agentic workflows and process automation | Reliability, cron scheduling, observability |
| P3 | Research Team Member | Groups running collaborative literature review, data analysis, or experimental pipelines | Multi-agent coordination, shared context, reproducibility |
| P4 | Platform Engineer | Teams building internal AI infrastructure for their organisations | Standardisation, governance, lifecycle management |

### 2.2 Secondary Users

| ID | Persona | Description | Key Needs |
|----|---------|-------------|-----------|
| S1 | Domain Expert | Subject matter experts using pre-built agents via Web UI or external channel | Ease of use, no CLI knowledge required |
| S2 | OSS Contributor | Community members building and sharing agents and capabilities | Distribution, versioning, discoverability |
| S3 | Integrator | Developers connecting Pekobot to games, services, or external channels | Stable HTTP/WebSocket API, webhook support |

---

## 3. Functional Requirements

---

### 3.1 Agent Image Model (REQ-AI)

#### REQ-AI-001: Filesystem-First Image Definition
**Priority:** Must Have | **Phase:** 1

An agent image shall be a directory with a single required file (`config.toml`). All other content is optional and loaded dynamically.

**Acceptance criteria:**
- Only `config.toml` is required; absence of any other file must not prevent startup
- `sessions/` is system-managed and never part of the image
- The runtime must run an agent directly from a source directory without any build step

---

#### REQ-AI-002: Image vs. Instance Distinction
**Priority:** Must Have | **Phase:** 1

The system shall maintain a strict separation between an agent image (frozen definition) and an agent instance (living runtime).

**Acceptance criteria:**
- An image is immutable once built; no runtime operation may modify image content
- An instance pins to an exact image digest at creation time
- Modifying files in an instance's workspace must not affect the source image
- `pekobot commit` is the only mechanism to promote instance changes back to an image

---

#### REQ-AI-003: Image Packaging for Distribution
**Priority:** Must Have | **Phase:** 1

The system shall support packaging agent images for distribution as content-addressable archives.

**Acceptance criteria:**
- `pekobot build ./agent/ -t name:version` produces a distributable package
- Package layers are SHA-256 content-addressable and deduplicated
- Running from a pulled package is functionally identical to running from source
- Build is deterministic: same source directory always produces the same digest

---

#### REQ-AI-004: Base Image Inheritance
**Priority:** Should Have | **Phase:** 1

Agents shall support inheriting configuration and content from a base image.

**Acceptance criteria:**
- `[base] image = "..."` in `config.toml` loads base before local config
- Merge rules: scalar fields — local overwrites base; arrays — local appended to base, deduplicated; markdown files — local replaces base file of same name
- Base `[[hooks]]` entries are inherited and cannot be removed by a derived image
- At least two levels of inheritance must be supported

---

#### REQ-AI-005: Explicit Instance Upgrade
**Priority:** Must Have | **Phase:** 1

Updating a running or stopped instance to a newer image version shall be an explicit operator action, never automatic.

**Acceptance criteria:**
- `pekobot instance upgrade <id> --to <image:tag>` is the only mechanism to change an instance's pinned digest
- Pulling a newer tag for an image must not silently affect any existing instance
- Session history is preserved across upgrades
- The instance's previous image digest is logged as a `system` session event on upgrade

---

### 3.2 Agent Instance and Runtime (REQ-AR)

#### REQ-AR-001: Agentic Loop
**Priority:** Must Have | **Phase:** 1

The runtime shall implement a turn-based agentic loop: receive input → LLM completion → tool execution → respond.

**Acceptance criteria:**
- Loop handles input from three sources: user message, hook trigger, A2A bus message
- Loop continues until the LLM produces a final response with no pending tool calls
- Unhandled panic in tool execution must not crash the loop; error is returned to the LLM as a `tool.result` event with `error` set

---

#### REQ-AR-002: Synchronous and Asynchronous Tool Calling
**Priority:** Must Have | **Phase:** 1

The runtime shall support both synchronous (blocking) and asynchronous (fire-and-forget) tool execution within a single turn.

**Acceptance criteria:**
- Sync tool: runtime blocks until result is available; result is fed back to LLM before next completion
- Async tool: runtime returns a receipt to the LLM immediately; result is injected into context when the tool completes
- Both modes must be available within a single turn (a turn may mix sync and async calls)
- Async tool timeout triggers a `tool.result` with `error: "timeout"` injected into context

---

#### REQ-AR-003: Subagent Spawning
**Priority:** Must Have | **Phase:** 1

An agent shall be able to spawn child agent instances to execute isolated subtasks.

**Acceptance criteria:**
- `agent_spawn` built-in tool creates a new instance from a named image and starts a session with the given task
- Sync spawn: parent blocks until child session completes; child output is returned as tool result
- Async spawn: parent receives a receipt; polls via `agent_spawn_status`
- Child instance is automatically stopped and cleaned up after completion or timeout
- Child session history is retained and accessible after cleanup

---

#### REQ-AR-004: Multi-Provider LLM Support
**Priority:** Must Have | **Phase:** 1

The runtime shall support multiple LLM providers, switchable via configuration only.

**Acceptance criteria:**
- Supported providers: Anthropic, OpenAI, Ollama, OpenAI-compatible endpoints
- Provider change requires only editing `[provider]` in `config.toml`; no code change
- Common interface: all providers support streaming completions and tool calling
- API credentials are read from environment variables; never stored in config files

---

#### REQ-AR-005: Watch Mode for Development
**Priority:** Must Have | **Phase:** 1

The runtime shall support a development watch mode that reloads the agent on source file changes.

**Acceptance criteria:**
- `pekobot run ./agent/ --watch` reloads the agent configuration on changes to any file in the agent directory
- Reload must complete within 2 seconds of file change
- Active sessions persist across reload; the new configuration takes effect on the next turn
- Watch mode is restricted to filesystem-source agents; packaged instances do not support watch

---

### 3.3 Daemon and HTTP API (REQ-DM)

#### REQ-DM-001: Single Daemon Process
**Priority:** Must Have | **Phase:** 1

All runtime operations shall be mediated by a single local daemon process. No client (CLI, TUI, external integration) shall communicate with agent internals directly.

**Acceptance criteria:**
- Daemon listens on `localhost:11435` by default (configurable)
- Daemon starts with zero configuration required; all settings have sensible defaults
- `pekobot daemon start` starts the daemon; `pekobot daemon stop` stops it gracefully
- PID file written to `.pekobot/run/daemon.pid` on start

---

#### REQ-DM-002: HTTP API
**Priority:** Must Have | **Phase:** 1

The daemon shall expose a complete HTTP API covering all runtime operations.

**Acceptance criteria:**
- API surface as defined in `API_CONTRACT.md` v1.0 is fully implemented
- All responses include `X-Pekobot-Version` header
- All requests accept an optional `X-Request-ID` header; daemon echoes it in the response
- Consistent error envelope `{ "error": { "code", "message", "request_id", "details" } }` on all error responses

---

#### REQ-DM-003: Streaming Responses
**Priority:** Must Have | **Phase:** 1

The daemon shall support streaming agent responses to clients.

**Acceptance criteria:**
- `POST /agents/{id}/chat` streams via Server-Sent Events by default
- Event types: `delta`, `tool_call`, `tool_result`, `thinking`, `done`, `error`
- Non-streaming fallback available via `Accept: application/json`
- WebSocket endpoint `ws://localhost:11435/agents/{id}/ws` for bidirectional use cases
- Stream must begin emitting `delta` events within 500ms of the LLM producing first tokens

---

#### REQ-DM-004: Outbound Hooks
**Priority:** Must Have | **Phase:** 1

The daemon shall activate agent sessions in response to external triggers without requiring a human to initiate.

**Acceptance criteria:**
- Supported hook types: `cron` (time-based), `webhook` (inbound HTTP), `event` (bus topic), `file_watch` (filesystem change)
- Hook trigger creates a new session (or injects into the active session if `session = "active"`)
- Trigger payload is delivered as the first user message with `source: "hook"`
- Inbound webhook endpoint: `POST /webhooks/{instance_id}/{token}`

---

#### REQ-DM-005: System Event Stream
**Priority:** Must Have | **Phase:** 1

The daemon shall emit a real-time stream of system events that clients can subscribe to.

**Acceptance criteria:**
- WebSocket endpoint `ws://localhost:11435/events`
- Client can filter by resource type, resource ID, and event type on connect
- Event types cover all instance, team, and image lifecycle changes as defined in `API_CONTRACT.md` §8.4
- Bus messages optionally included via opt-in filter (due to potential volume)

---

### 3.4 Session Management (REQ-SM)

#### REQ-SM-001: Durable JSONL Sessions
**Priority:** Must Have | **Phase:** 1

All session events shall be durably persisted to JSONL files on disk.

**Acceptance criteria:**
- Each event is written as a separate line: `{ id, type, session_id, ts, seq, ...fields }`
- Write is atomic: new events written to `.tmp` then renamed; partial `.tmp` files on restart must be discarded
- File survives daemon restart; session history is fully recoverable from the JSONL file alone
- SQLite state database is a read index over JSONL, not the source of truth; must be rebuildable by scanning JSONL files

---

#### REQ-SM-002: Full Event Type Coverage
**Priority:** Must Have | **Phase:** 1

The session JSONL must capture all meaningful runtime events, not just user and assistant messages.

**Acceptance criteria:**
- All event types from `DATA_MODEL.md` §5.3 must be written: `session.created`, `user.message`, `assistant.message`, `thinking`, `tool.call`, `tool.result`, `spawn.request`, `spawn.result`, `a2a.sent`, `a2a.received`, `hook.trigger`, `system`, `session.ended`
- `tool.call` and `tool.result` events are always written regardless of `include_tool_calls` API parameter (that parameter only controls API response filtering)
- `session.ended` is written on clean shutdown; its absence indicates unclean termination

---

#### REQ-SM-003: Session Branching
**Priority:** Should Have | **Phase:** 1

Users shall be able to branch a session at any point to explore alternative continuations.

**Acceptance criteria:**
- `POST /agents/{id}/sessions/{session_id}/branch` creates a new session with the same history up to the branch point
- Branched session has `parent_session_id` set
- Changes to the branch do not affect the parent session
- Branch is immediately available for chat

---

#### REQ-SM-004: Session Index
**Priority:** Must Have | **Phase:** 1

A sidecar index file shall be maintained alongside each JSONL file for fast metadata lookup.

**Acceptance criteria:**
- `.index.json` file maintained at the same path as the JSONL
- Contains: `session_id`, `instance_id`, `created_at`, `updated_at`, `turn_count`, `event_count`, `total_tokens`, `parent_session_id`, `trigger`, `ended`, `title`
- Index is updated atomically on every event write
- `title` is auto-generated from the first assistant response (first 60 characters); user-settable thereafter

---

#### REQ-SM-005: Session Plugin Interface
**Priority:** Should Have | **Phase:** 1

The session write/read path shall support pluggable transformations via the `SessionPlugin` interface.

**Acceptance criteria:**
- Plugin declared via `[capabilities.session] plugin = "..."` in `config.toml`
- Plugin loaded as a shared library implementing the `SessionPlugin` trait (`on_write`, `on_read`, `on_context_build`)
- Panic in plugin is caught; runtime falls back to raw events and logs a `system` event
- Only one plugin active per session

---

### 3.5 Built-in Capability System (REQ-CAP)

#### REQ-CAP-001: Built-in Tool Set
**Priority:** Must Have | **Phase:** 1

The runtime shall provide a complete set of built-in tools compiled into the binary.

**Acceptance criteria:**
- All 13 tools from `CAPABILITY_INTERFACE.md` §3 must be implemented: `filesystem`, `process`, `apply_patch`, `agent_spawn`, `agent_spawn_status`, `agent_spawn_list`, `agents_list`, `agent_info`, `sessions_send`, `sessions_list`, `sessions_history`, `session_status`, `cron`
- Each tool implements the `Tool` trait with correct sync/async support as specified
- Any built-in tool can be disabled per-agent via `[capabilities] disabled_tools = [...]`

---

#### REQ-CAP-002: Custom Tool Discovery
**Priority:** Must Have | **Phase:** 1

The runtime shall discover and register custom tools from the agent image's `tools/` directory.

**Acceptance criteria:**
- Executable files in `tools/` are auto-discovered at instance startup
- Each executable is invoked via the stdin/stdout JSON protocol defined in `DATA_MODEL.md` §10
- Optional `<toolname>.json` sidecar provides the tool's parameter schema to the LLM
- Without a sidecar, a minimal schema is generated from the tool name

---

#### REQ-CAP-003: MCP Integration
**Priority:** Must Have | **Phase:** 1

The runtime shall act as an MCP client, proxying tool calls to external MCP server processes.

**Acceptance criteria:**
- MCP servers declared in `mcp.json` are started as subprocesses at instance startup
- Runtime discovers tools via MCP `list_tools`; registers them alongside built-in and custom tools
- Tool calls to MCP tools are proxied transparently; LLM cannot distinguish MCP from built-in tools
- MCP server startup failure aborts instance startup with a clear error

---

#### REQ-CAP-004: Capability Resolution Order
**Priority:** Must Have | **Phase:** 1

The runtime shall resolve capability names in a defined, deterministic order.

**Acceptance criteria:**
- Resolution order: built-in → agent-local `tools/` → installed capabilities → auto-install (if enabled)
- Built-in tool takes precedence over same-named custom or installed tool; warning is logged
- Version pinning syntax supported: `tool@1.2.0` (exact) and `tool@^1.0` (semver range)
- Instance startup aborts with `capability_not_installed` error if a declared capability cannot be resolved and `auto_install` is false

---

#### REQ-CAP-005: Capability Sandbox Enforcement
**Priority:** Must Have | **Phase:** 1

The runtime shall enforce filesystem and process sandboxing for all tool invocations.

**Acceptance criteria:**
- Default sandbox level is `paths`: filesystem tools restricted to instance workspace, team shared workspace, and explicit grants
- Path traversal attempts are rejected after canonicalisation with `SandboxViolation`
- `process` tool blocks shell invocation (`sh`, `bash`, `zsh`, `cmd`, `powershell` as command)
- `process` tool strips all env vars matching `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD` from child process environment
- Sandbox level configurable to `none`, `paths`, `namespace`, `container` in `[resources] sandbox`

---

#### REQ-CAP-006: Agent Tool Scoping
**Priority:** Must Have | **Phase:** 1

Agent introspection and communication tools shall be scoped to the current team by default.

**Acceptance criteria:**
- `agents_list`, `agent_info`, `agent_spawn` return/target only instances within the same team
- Standalone agents (not in a team) receive empty results from `agents_list`
- `sessions_send` is rejected with `cross_agent_send_forbidden` when the target session belongs to another agent in the same team
- Cross-team access requires explicit `[capabilities.grants] cross_team_agent_access = true`

---

#### REQ-CAP-007: Cron Job Persistence
**Priority:** Must Have | **Phase:** 1

Cron jobs registered via the `cron` tool shall be persisted and survive instance restarts.

**Acceptance criteria:**
- Cron state written to `<instance-workspace>/cron.json` on every registration or cancellation
- Write is atomic (`.tmp` + rename)
- On restart, daemon loads `cron.json` and reschedules all active jobs
- Missed `at` (one-shot) jobs that never ran are executed immediately on restart
- Missed `every`/`cron` (repeating) jobs fire immediately on restart, then resume schedule
- Cron jobs are owned by the instance; cannot target or be visible to other instances

---

### 3.6 Team Runtime (REQ-TR)

#### REQ-TR-001: Declarative Team Composition
**Priority:** Must Have | **Phase:** 1

Multi-agent teams shall be defined declaratively in a single `team.toml` file and deployed with a single command.

**Acceptance criteria:**
- `team.toml` defines agents, instance counts, roles, shared services, and bus backend
- `pekobot team deploy -f team.toml` deploys all agents and starts shared services
- Team workspace created at `.pekobot/teams/<name>/` with per-agent subdirectories
- File format: TOML (`team.toml`); YAML (`team.yaml`) also accepted

---

#### REQ-TR-002: Shared Services Fabric
**Priority:** Must Have | **Phase:** 1

Teams shall provision shared infrastructure accessible to all member agents, with automatic lifecycle management.

**Acceptance criteria:**
- Shared services: event bus, vector memory, file workspace, MCP servers
- Each shared service is started once and reference-counted; torn down when last consumer disconnects
- Shared file workspace at `.pekobot/teams/<name>/shared/files/` is accessible to all agents in the team via `filesystem` tool
- Shared MCP servers are proxied by the runtime; agents do not communicate with them directly

---

#### REQ-TR-003: Horizontal Scaling
**Priority:** Should Have | **Phase:** 1

Individual agent types within a team shall be horizontally scalable without restarting the team.

**Acceptance criteria:**
- `pekobot team scale <team> <agent-name> <count>` adds or removes instances
- Scaling up starts new instances from the same pinned image digest as existing instances of that type
- Scaling down gracefully stops excess instances (current turn completes before stop)
- Scale operation completes within 5 seconds

**Session ownership during scale-down:**
- Sessions from stopped instances are preserved in their original location (not migrated)
- Sessions are marked with `session.ended` event with reason `instance_stopped`
- Sessions remain queryable via API for stopped instances
- Sessions cannot be resumed; branching from session history is supported
- Purging a stopped instance with `DELETE /agents/{id}?purge=true` permanently deletes its sessions

---

#### REQ-TR-004: Unified Runtime for Teams and Standalone Agents
**Priority:** Must Have | **Phase:** 1

There shall be a single runtime codebase for both standalone agents and team agents. No separate "team runtime."

**Acceptance criteria:**
- The same daemon process handles both standalone and team instances
- A team agent is an instance with a `team_id` field set; runtime behaviour is otherwise identical
- All instance API endpoints (`GET /agents/{id}`, etc.) work identically for team and standalone instances

---

### 3.7 Event Bus (REQ-BUS)

#### REQ-BUS-001: Bus-Mediated A2A Communication
**Priority:** Must Have | **Phase:** 1

All agent-to-agent communication within a team shall be mediated by the team event bus. Direct agent-to-agent calls are not permitted.

**Acceptance criteria:**
- No runtime mechanism exists for agent A to directly call agent B without going through the bus
- All bus messages are logged as `a2a.sent` / `a2a.received` session events
- A2A message types: `Direct`, `Task`, `TaskResult`, `Broadcast`, `Subscribe`
- Bus is created per team; standalone agents have no bus

---

#### REQ-BUS-002: Pluggable Bus Backend
**Priority:** Must Have | **Phase:** 1

The bus backend shall be pluggable via configuration, enabling deployment from a single process to distributed multi-host teams.

**Acceptance criteria:**
- Supported backends: `in-memory` (default), `redis`, `nats`
- Backend declared in `[shared.bus] backend` in `team.toml`
- In-memory backend requires no external dependencies; used for all single-node deployments
- Redis backend requires Redis 6.2+ with Streams; NATS backend requires NATS 2.2+ with JetStream
- Switching backend requires only configuration change; no agent code changes

---

#### REQ-BUS-003: Bus Message Observability
**Priority:** Must Have | **Phase:** 1

All bus messages shall be observable without modifying agent code.

**Acceptance criteria:**
- Every message transiting the bus is logged as `a2a.sent` in the sender's session and `a2a.received` in the recipient's session
- Bus messages emitted on the system event stream (`/events`) when opted in
- Bus messages queryable via session history API

---

### 3.8 Registry (REQ-RG)

#### REQ-RG-001: Image Registry
**Priority:** Must Have | **Phase:** 1

The system shall support pushing and pulling agent images to and from remote registries.

**Acceptance criteria:**
- `pekobot pull <ref>` downloads image to local cache at `.pekobot/registry/images/`
- `pekobot push <local-ref> <remote-ref>` uploads image to registry
- Pull streams progress events (layer-by-layer) to the client
- Local cache deduplicates layers by SHA-256 digest; identical layers across images are stored once

---

#### REQ-RG-002: Registry Authentication
**Priority:** Must Have | **Phase:** 1

The registry client shall support authenticated access to private registries.

**Acceptance criteria:**
- Auth types: bearer token, HTTP Basic
- Credentials read from environment variables; never stored in config files
- Multiple registry sources configurable in `runtime.toml` with per-source auth and priority order
- Unauthenticated public access supported (no credentials required)

---

#### REQ-RG-003: Package Signing
**Priority:** Could Have | **Phase:** 1

Images shall support optional cryptographic signing for integrity verification.

**Acceptance criteria:**
- `pekobot sign <image:tag>` signs the image manifest with a private key
- Signature is verified on pull if signing is configured; verification failure logs a warning (not a hard failure) by default
- Hard failure mode configurable in `runtime.toml`

---

### 3.9 Interfaces (REQ-UI)

#### REQ-UI-001: Non-Interactive CLI
**Priority:** Must Have | **Phase:** 1

The CLI shall be fully scriptable with no interactive prompts.

**Acceptance criteria:**
- No command ever prompts the user for input; all required values are passed as arguments or flags
- All commands exit with code `0` on success and non-zero on error
- All commands support `--output json` for machine-readable output
- CLI talks to daemon via HTTP; daemon must be running for all commands except `pekobot daemon start`

---

#### REQ-UI-002: TUI
**Priority:** Should Have | **Phase:** 1

A terminal user interface shall be available as a separate binary over the HTTP API.

**Acceptance criteria:**
- `pekobot-tui` binary, separate from the main `pekobot` binary
- Provides: live instance/team view, interactive chat with any instance, event bus monitor, log tail
- No privileged access to agent internals; communicates only via the HTTP API
- Functions correctly over SSH

---

#### REQ-UI-003: Web UI
**Priority:** Should Have | **Phase:** 1

A lightweight web UI shall be served by the daemon.

**Acceptance criteria:**
- Served at `http://localhost:11435/ui`
- Single static HTML file embedded in the daemon binary; no separate build or file serving
- Functional parity with TUI: instance list, chat, event stream, logs
- Works in modern browsers without any browser extensions

---

#### REQ-UI-004: Webhook Channel API
**Priority:** Must Have | **Phase:** 1

The daemon shall provide a generic inbound webhook endpoint for third-party channel integrations.

**Acceptance criteria:**
- Endpoint: `POST /webhooks/{instance_id}/{token}`
- Token validated against agent's hook config; 401 returned on mismatch
- Request body delivered as trigger payload to the agent session
- `202 Accepted` returned immediately; processing is async
- First-party Discord/Slack/email integrations are explicitly out of scope; the webhook endpoint is sufficient

---

#### REQ-UI-005: WebSocket Service Integration
**Priority:** Should Have | **Phase:** 1

The daemon shall provide a WebSocket endpoint for bidirectional integration with games and external services.

**Acceptance criteria:**
- Endpoint: `ws://localhost:11435/agents/{id}/ws`
- Wire format: JSON envelope with typed frames (`hello`, `message`, `delta`, `tool_call`, `done`, `error`, `ping`, `pong`, `ack`)
- Connection drop mid-turn: agent completes turn; result available via session history API
- No authentication required for localhost connections (consistent with HTTP API)

---

### 3.10 Control Plane (REQ-CP)

> **Phase 2.** These requirements define what Phase 1 must not preclude. Detailed spec will be written before Phase 2 begins.

#### REQ-CP-001: Runtime Observability Hook Points
**Priority:** Must Have | **Phase:** 1 (prerequisite)

Phase 1 must expose enough observability for a control plane to operate without modifying runtime internals.

**Acceptance criteria:**
- All instance lifecycle events emitted on the system event stream
- All session events include `instance_id` and `team_id`
- Health check endpoint (`GET /health`) returns structured status including instance count and uptime
- Daemon log format supports `json` output mode for log aggregation

---

#### REQ-CP-002: Agent Lifecycle Management
**Priority:** Phase 2

The control plane shall manage instance lifecycle (start, stop, restart, upgrade) with configurable policies.

**Acceptance criteria (Phase 2):**
- Restart policies: `never`, `on-failure`, `always`
- Configurable restart backoff (exponential with jitter)
- Upgrade policy: `manual` (default) | `on-restart` | `auto`
- All lifecycle actions audit-logged

---

#### REQ-CP-003: Resource Allocation
**Priority:** Phase 2

The control plane shall enforce per-instance resource budgets.

**Acceptance criteria (Phase 2):**
- CPU and memory limits enforced via cgroups (Linux)
- Limits declared per-agent in `config.toml` under `[resources]`
- Instance killed and restarted if memory limit exceeded; event emitted

---

### 3.11 Capability Ecosystem (REQ-CE)

> **Phase 3.** These requirements define what Phase 1 must not preclude.

#### REQ-CE-001: Capability Install Command
**Priority:** Phase 3

A package manager for capabilities shall allow installing, publishing, and listing capabilities.

**Acceptance criteria (Phase 3):**
- `pekobot install tool:web-browser` installs a versioned tool capability
- `pekobot install skill:researcher` installs a skill
- `pekobot install mcp:vector-store-memory` installs an MCP server capability
- `pekobot install session:lossless-compression` installs a session plugin
- `pekobot capability list` shows installed capabilities
- `pekobot capability publish ./my-tool/ -t pekohub.com/tools/my-tool:v1.0` publishes

---

#### REQ-CE-002: Auto-Install from Declared Dependencies
**Priority:** Phase 3

The runtime shall automatically install declared capability dependencies before starting an instance.

**Acceptance criteria (Phase 3):**
- If `[capabilities.options] auto_install = true` and a declared capability is not installed, the runtime fetches it from the configured registry before startup
- Install is idempotent; already-installed capabilities are not re-downloaded
- Install failures produce clear error output listing what failed and why

---

## 4. Non-Functional Requirements

### 4.1 Performance (REQ-PF)

#### REQ-PF-001: Instance Cold Start
**Priority:** Must Have | **Phase:** 1
**Target:** < 500ms from `pekobot run` to first LLM call
**Measurement:** `time pekobot run ./minimal-agent/ --detach`, measured 10 times, median < 500ms

---

#### REQ-PF-002: Instance Warm Start
**Priority:** Must Have | **Phase:** 1
**Target:** < 100ms from daemon receiving `POST /agents` to instance `status: "running"`
**Measurement:** API request timestamp to `instance.started` event timestamp

---

#### REQ-PF-003: Streaming First Token
**Priority:** Must Have | **Phase:** 1
**Target:** First `delta` SSE event within 500ms of LLM producing first token
**Measurement:** Time delta between LLM stream start and first `delta` event received by client

---

#### REQ-PF-004: Built-in Tool Latency
**Priority:** Must Have | **Phase:** 1
**Target:** < 5ms for `filesystem.read`, `filesystem.exists`, `session_status`
**Measurement:** Tool invocation to result, excluding LLM processing time

---

#### REQ-PF-005: In-Memory Bus Latency
**Priority:** Should Have | **Phase:** 1
**Target:** < 1ms round-trip for a Direct message between two agents on the in-memory bus
**Measurement:** `a2a.sent` timestamp to `a2a.received` timestamp in session JSONL

---

#### REQ-PF-006: Concurrent Instances
**Priority:** Should Have | **Phase:** 1
**Target:** 50+ concurrent running instances per daemon process without degradation
**Measurement:** Memory usage and median response time remain stable with 50 idle instances

---

#### REQ-PF-007: Team Deploy Time
**Priority:** Should Have | **Phase:** 1
**Target:** < 30 seconds for a 5-agent team including shared service startup
**Measurement:** `pekobot team deploy` invocation to `team.ready` event

---

### 4.2 Reliability (REQ-RL)

#### REQ-RL-001: Session Durability
**Priority:** Must Have | **Phase:** 1

No session event that has been acknowledged by the runtime shall be lost on crash.

**Acceptance criteria:**
- Atomic JSONL write (`.tmp` + rename) guarantees no partial event on crash
- Session is fully recoverable from JSONL alone after daemon restart
- SQLite index is rebuilt from JSONL on startup if state.db is missing or corrupt

---

#### REQ-RL-002: Tool Failure Isolation
**Priority:** Must Have | **Phase:** 1

A tool execution failure must not crash the agentic loop or corrupt the session.

**Acceptance criteria:**
- Unhandled exception/panic in any tool is caught by the runtime
- Error is returned to the LLM as a `tool.result` event with `error` set
- Session continues normally after a tool failure
- Tool timeout triggers the same error path, not a loop hang

---

#### REQ-RL-003: Daemon Crash Recovery
**Priority:** Must Have | **Phase:** 1

The daemon shall recover cleanly from a crash without requiring manual intervention.

**Acceptance criteria:**
- On restart, daemon rebuilds in-memory state from JSONL files and SQLite
- Instances in `starting` or `stopping` status at crash time are moved to `stopped` with `error: "daemon_crash"` on restart
- Sessions with no `session.ended` event are marked as `ended: false`; user can branch or continue them
- Cron jobs are rescheduled from `cron.json` on restart

---

#### REQ-RL-004: MCP Server Recovery
**Priority:** Should Have | **Phase:** 1

If a managed MCP server process exits unexpectedly, the runtime shall attempt to restart it.

**Acceptance criteria:**
- MCP server crash detected within 1 second
- Restart attempted up to 3 times with 5-second backoff
- After 3 failed restarts, the tool(s) provided by that MCP are marked unavailable; the LLM is notified via a `system` session event
- Instance continues running with remaining tools

---

### 4.3 Security (REQ-SC)

#### REQ-SC-001: Filesystem Sandbox
**Priority:** Must Have | **Phase:** 1

Already covered by REQ-CAP-005. Listed here for cross-reference.

---

#### REQ-SC-002: Credential Isolation
**Priority:** Must Have | **Phase:** 1

LLM API credentials and other secrets shall never be accessible to tool processes or agent sessions.

**Acceptance criteria:**
- `process` tool strips `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD` env vars from child environment
- Credentials are not included in any session event, log line, or API response
- `config.toml` must not contain credentials; validation warns and refuses to start if a credential-shaped value is found in a non-env field

---

#### REQ-SC-003: Full Audit Trail
**Priority:** Must Have | **Phase:** 1

All agent actions shall be auditable via session JSONL.

**Acceptance criteria:**
- Every tool call (name, args, result) is written as session events
- Every A2A message sent and received is written as session events
- Every hook trigger is written as a `hook.trigger` session event
- `pekobot session show <session-id>` renders a human-readable timeline from the JSONL

---

#### REQ-SC-004: Localhost-Only Default
**Priority:** Must Have | **Phase:** 1

The daemon shall listen on localhost only by default, preventing remote access without explicit configuration.

**Acceptance criteria:**
- Default bind address: `127.0.0.1`
- Binding to `0.0.0.0` requires explicit `[daemon] host = "0.0.0.0"` in `runtime.toml`
- Warning printed to stderr when binding to non-loopback address

---

### 4.4 Usability (REQ-US)

#### REQ-US-001: Time to First Agent
**Priority:** Must Have | **Phase:** 1
**Target:** A developer with no prior Pekobot experience must be able to go from fresh install to a running, responding agent in under 5 minutes.

**Acceptance criteria:**
- Getting started guide covers: install → create minimal `config.toml` → `pekobot run` → first response
- Minimal `config.toml` is 4 lines (name, version, provider_type, model)
- `pekobot run ./agent/` with a missing or malformed config produces an actionable error, not a stack trace

---

#### REQ-US-002: Actionable Error Messages
**Priority:** Must Have | **Phase:** 1

All user-facing errors shall be actionable: they say what went wrong and how to fix it.

**Acceptance criteria:**
- Every error code in `API_CONTRACT.md` §11.3 has a corresponding human-readable message template
- Validation errors include the field name and the rule that was violated
- Stack traces are suppressed in default mode; shown only with `--debug` flag or `PEKOBOT_DEBUG=1`

---

#### REQ-US-003: Git-Friendly Source Layout
**Priority:** Must Have | **Phase:** 1

Agent source directories shall be git-friendly by default.

**Acceptance criteria:**
- `pekobot init ./my-agent/` generates a `.gitignore` that excludes `sessions/`, `workspace/`, `memories/`, `cron.json`
- All configuration files (`config.toml`, markdown files) produce meaningful diffs
- No binary blobs in the agent image source directory; `tools/` may contain executables but these are tracked by git normally

---

#### REQ-US-004: Structured CLI Output
**Priority:** Should Have | **Phase:** 1

CLI commands shall support structured output for scripting and CI integration.

**Acceptance criteria:**
- All list and show commands support `--output json` and `--output table` (default)
- `--output json` outputs valid JSON to stdout, suitable for piping to `jq`
- Non-zero exit code on any error; error details on stderr, not stdout

---

## 5. Use Cases

### UC-001: Solo Developer — Personal Assistant

**Actor:** P1 (Solo Developer)
**Scenario:** Build and iterate on a personal assistant agent.

**Flow:**
1. `pekobot init ./my-assistant/` creates minimal structure and `.gitignore`
2. Edit `config.toml` (4 lines: name, version, provider, model)
3. Add `AGENT.md` describing desired behaviour
4. `pekobot run ./my-assistant/ --watch` starts the agent and watches for changes
5. Chat with the agent; edit `AGENT.md`; changes take effect within 2 seconds
6. `pekobot build ./ -t my-assistant:v1.0` packages for sharing

**Success criteria:**
- Steps 1–4 take under 5 minutes for a new user
- File change to updated agent behaviour under 2 seconds in watch mode
- Package built in under 10 seconds

---

### UC-002: Automation Engineer — Cron-Triggered Pipeline

**Actor:** P2 (Automation Engineer)
**Scenario:** Agent that wakes up every morning, checks for new files in an inbox, processes them, and sends a summary.

**Flow:**
1. Agent config declares a `cron` hook (`0 8 * * 1-5`) and a `file_watch` hook on `inbox/`
2. `pekobot run ./pipeline-agent/ --detach`
3. At 8am, daemon triggers a new session; agent scans inbox, processes files with `filesystem` and `process` tools, writes output to `workspace/`
4. Agent calls `cron` tool with `at` sub-command to schedule a one-time follow-up
5. Session ends; session history queryable for audit

**Success criteria:**
- Cron fires within 60 seconds of scheduled time
- Session history for each run is independently queryable
- Cron jobs survive daemon restart

---

### UC-003: Research Team — Multi-Agent Pipeline

**Actor:** P3 (Research Team Member)
**Scenario:** Coordinator delegates research tasks to worker agents in parallel; writer synthesises results.

**Flow:**
1. `team.toml` defines coordinator (×1), researcher (×4), writer (×1) with shared `mcp-browser` and in-memory bus
2. `pekobot team deploy -f team.toml`
3. Coordinator receives task, sends `Task` messages to researcher instances via bus
4. Researchers use `mcp-browser` (shared, one process) to gather information; send `TaskResult` back via bus
5. Coordinator aggregates, sends to writer; writer produces final document in shared file workspace
6. `pekobot team scale research-team researcher 8` doubles researchers mid-run

**Success criteria:**
- Team deploys in under 30 seconds
- Shared browser MCP started once; all researchers use it without duplication
- Scale operation completes within 5 seconds

---

### UC-004: Platform Engineer — Internal Agent Infrastructure

**Actor:** P4 (Platform Engineer)
**Scenario:** Deploy Pekobot as shared infrastructure; multiple teams run agents against it.

**Flow:**
1. Set `[daemon] host = "0.0.0.0"` in `runtime.toml` (warns on start)
2. Each team maintains their own `team.toml`; platform engineer deploys via CI
3. `--output json` used in all CI commands for reliable parsing
4. Session JSONL files shipped to central log aggregator via syslog/file watcher
5. Phase 2 control plane manages lifecycle and restarts

**Success criteria:**
- All CI commands exit with structured JSON and correct exit codes
- Daemon log in JSON format (`log_format = "json"`) parseable by aggregator
- Phase 1 audit trail sufficient for compliance review without Phase 2

---

### UC-005: Integrator — Game NPC via WebSocket

**Actor:** S3 (Integrator)
**Scenario:** Game connects to a Pekobot agent as an NPC with persistent memory.

**Flow:**
1. Game creates an instance: `POST /agents` with NPC character's agent image
2. Game connects to `ws://localhost:11435/agents/{id}/ws`
3. Game sends player input as `message` frames; receives `delta` frames in real time
4. Agent uses `filesystem` tool to persist NPC state to workspace between sessions
5. On game restart, same instance is resumed; session history provides memory continuity

**Success criteria:**
- WebSocket connects within 100ms of daemon receiving upgrade request
- `delta` frames begin within 500ms of player input
- NPC state persists across game restarts via instance workspace

---

## 6. Success Criteria

### 6.1 Developer Experience

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Time to first running agent | < 5 minutes | New user timed walkthrough |
| Minimal `config.toml` length | ≤ 6 lines | Count lines in minimal example |
| Watch mode reload latency | < 2 seconds | `touch AGENT.md` to next turn using new content |
| Time to package and push | < 30 seconds | `pekobot build` + `pekobot push` combined |
| Error message actionability | 100% of error codes have fix suggestions | Audit error template coverage |

### 6.2 Performance

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Cold start (run to first LLM call) | < 500ms | Median of 10 runs, minimal agent |
| Warm start (API to `running`) | < 100ms | `POST /agents` to `instance.started` event |
| First streaming token to client | < 500ms after LLM first token | SSE event timestamp delta |
| Built-in tool latency (fast ops) | < 5ms | `filesystem.read`, `session_status` |
| In-memory bus round-trip | < 1ms | `a2a.sent` to `a2a.received` in JSONL |
| Concurrent instances (stable) | 50+ | Memory + latency stable at 50 idle |
| Team deploy (5 agents) | < 30 seconds | `deploy` to `team.ready` event |
| Team scale (+1 instance) | < 5 seconds | `scale` command to instance `running` |

### 6.3 Reliability

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Session event durability | 100% (zero loss) | Kill daemon mid-write; verify JSONL |
| Recovery from daemon crash | < 10 seconds | `kill -9` daemon; time to restart + `running` |
| Tool failure isolation | 100% | Inject panic in test tool; verify loop continues |
| Cron fire accuracy | Within 60 seconds of scheduled time | Log cron trigger timestamps vs schedule |

### 6.4 Security

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Credential leak to tool subprocess | Zero | Run `process env` tool; verify no `*_API_KEY` present |
| Path traversal rejection | 100% | Fuzzing `filesystem` paths with `../` variants |
| Localhost-only default | Verified | `ss -tlnp` shows `127.0.0.1:11434` by default |

---

## 7. Constraints

### 7.1 Technical Constraints

| Constraint | Rationale |
|------------|-----------|
| Core runtime in Rust | Latency, memory safety, single binary deployment |
| Session storage in JSONL | Human-readable, git-diffable, appendable without locking |
| SQLite for structured persistence | Zero dependency, embeddable, sufficient for single-node |
| TOML for agent config, TOML/YAML for team config | Human-writable, git-diffable |
| HTTP+SSE+WebSocket for API | Universally supported; no custom protocol |
| In-memory Tokio channels for default bus | Zero latency, zero dependency |

### 7.2 Design Constraints

| Constraint | Rationale |
|------------|-----------|
| Daemon is single control point | Simplifies observability, auth, and state management |
| Image is immutable once built | Reproducibility and audit integrity |
| Instance pins to exact digest | Prevents silent drift |
| A2A only via bus | Observability, decoupling, backpressure |
| No shell execution in `process` tool | Prevents shell injection |
| Credentials only in env vars | Prevents accidental commit to git |

### 7.3 Business Constraints

| Constraint | Rationale |
|------------|-----------|
| Core runtime open source | Community adoption, ecosystem growth |
| Self-hosted primary deployment | Data sovereignty, no vendor lock-in |
| Federated registries | No single registry monopoly |
| MCP compatibility | Interoperability with existing tool ecosystem |
| No first-party Discord/Slack apps | Avoid channel maintenance burden; HTTP API is sufficient |

---

## 8. Out of Scope

### 8.1 Explicitly Excluded from All Phases

- **Enterprise RBAC**: Capability grants are instance-level; no role hierarchy
- **Content moderation**: Agent speech is the operator's responsibility
- **Managed cloud hosting**: Self-hosted only; no Pekobot SaaS
- **Visual agent builder**: Configuration is code and text only
- **First-party messaging integrations**: Discord, Slack, Telegram are third-party channel adapters using the webhook API

### 8.2 Deferred to Phase 2

- Instance scheduling across multiple nodes
- CPU/memory resource enforcement (cgroups)
- Configurable restart policies
- Centralised audit log query interface

### 8.3 Deferred to Phase 3

- `pekobot install` capability package manager
- Capability registry (pekohub.com capabilities)
- Auto-install from declared dependencies
- WASM sandboxing for untrusted tools

---

## 9. Glossary

| Term | Definition |
|------|------------|
| **Agent image** | A frozen, versioned, immutable snapshot of an agent definition. Contains config, prompts, tools, skills, and knowledge. Never contains session history. |
| **Agent instance** | A living runtime derived from an image. Has its own session history, workspace, and cron registrations. Mutable. |
| **Agentic loop** | The core turn cycle: receive input → LLM completion → tool execution → respond. |
| **Capability** | An extension to the core runtime: a tool, MCP server, skill, or session plugin. |
| **Cron job** | A scheduled task registered by an agent via the `cron` tool. Persisted to `cron.json`. |
| **Daemon** | The single background process that mediates all runtime operations via HTTP API. |
| **Event bus** | The per-team message bus that mediates all agent-to-agent communication. |
| **Hook** | A trigger that activates an agent session: cron schedule, inbound webhook, bus event, or file change. |
| **Image digest** | SHA-256 hash of an image manifest. The true identity of an image, independent of its tag. |
| **MCP** | Model Context Protocol. Standard for connecting LLMs to external tool processes. |
| **Session** | A persistent conversation history stored as a JSONL file. One session = one sequence of events. |
| **Shared services fabric** | Team-level infrastructure (bus, memory, file workspace, MCPs) started once and shared across all agent instances in the team. |
| **Skill** | A packaged prompt template + optional tool bundle. Injected into system context at session startup. |
| **Team** | A declared group of agent instances with a shared event bus and shared services. |
| **Watch mode** | Development mode (`--watch`) where the runtime reloads agent configuration on source file changes. |

---

## Revision Notes

### Changes from v1.0 to v2.0

**Structural changes:**
- Requirements reorganised around the four new specs (`UNIFIED_ARCHITECTURE_SPEC.md` v4.0, `API_CONTRACT.md`, `DATA_MODEL.md`, `CAPABILITY_INTERFACE.md`)
- Phase labels added to all requirements
- Phase 2 (control plane) and Phase 3 (capability ecosystem) requirements added as stubs

**Conceptual changes:**
- **Image vs. instance distinction** (REQ-AI-002) added as a first-class requirement — absent from v1.0
- **Explicit instance upgrade** (REQ-AI-005) added — v1.0 had no requirement preventing silent upgrades
- **Daemon as single control point** (REQ-DM-001) formalised — v1.0 implied this but did not require it
- **Outbound hooks** (REQ-DM-004) added — cron, webhook, event, file_watch triggers were not in v1.0
- **Built-in tool set** fully enumerated (REQ-CAP-001) — v1.0 listed only `filesystem`, `process`, `web_search`
- **Agent tool scoping** (REQ-CAP-006) added — `agents_list` and `agent_spawn` team-scoping rules
- **Cron persistence** (REQ-CAP-007) added — v1.0 had no requirement on cron lifecycle
- **Unified runtime** (REQ-TR-004) formalised — v1.0 implied a separate "team runtime"
- **Bus-mediated A2A** (REQ-BUS-001) made a hard requirement with enforcement — v1.0 allowed direct calls
- **`sessions_send` scoping** covered under REQ-CAP-006 — not present in v1.0
- **Credential isolation** (REQ-SC-002) added — v1.0 had no requirement on env var stripping
- **Localhost-only default** (REQ-SC-004) added — not addressed in v1.0

**Removed from v1.0:**
- `team.yaml` references updated to `team.toml` (TOML is primary; YAML also accepted)
- `web_search` removed from built-in tools list — migrated to `mcp-web` MCP server
- Coordination pattern requirement (REQ-TR-004 in v1.0) removed — patterns are emergent from bus message types, not explicitly configured

---

*Version: 2.0 · Last Updated: 2026-03-16 · Status: Draft*
