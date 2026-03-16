# Pekobot — Unified Architecture Specification

**Version:** 4.0
**Date:** 2026-03-16
**Status:** Draft

Pekobot is a filesystem-first runtime for AI agents — analogous to Docker, but for autonomous AI processes. This document is the single authoritative reference for Pekobot's architecture, covering the agent model, runtime, daemon, CLI, and the two planned upper layers: the Control Plane and the Capability Ecosystem.

---

## Table of Contents

1. [Core Concepts](#1-core-concepts)
2. [Runtime Workspace](#2-runtime-workspace)
3. [Daemon](#3-daemon)
4. [Core Runtime](#4-core-runtime)
5. [Agent Event Bus](#5-agent-event-bus)
6. [Team Composition](#6-team-composition)
7. [Interfaces](#7-interfaces)
8. [Capability Extension](#8-capability-extension)
9. [Upper Layers (Planned)](#9-upper-layers-planned--post-runtime)
10. [Architecture Layers Summary](#10-architecture-layers-summary)
11. [Implementation Notes](#11-implementation-notes)

---

## 1. Core Concepts

### 1.1 Agent Image vs. Agent Instance

The distinction between image and instance is foundational and must be held clearly throughout the system.

| Concept | Definition |
|---------|------------|
| **Agent Image** | A frozen, versioned snapshot of an agent definition. Contains config, prompts, tools, skills, and knowledge. Stored in a registry. Immutable once tagged. |
| **Agent Instance** | A living, running agent derived from an image. Has its own session history, private workspace, and runtime state. Mutable and ephemeral by default. |

The relationship mirrors Docker's image/container model. You pull or build an image once and spawn many instances from it. Editing files in an instance's workspace does not modify the source image.

> **NOTE:** An instance's `sessions/`, `memories/`, and private workspace are instance-owned. They survive restarts but are never written back to the image unless you explicitly run `pekobot commit`.

### 1.2 What Makes an Agent

An agent image is a directory (or packaged form of one) with the following structure:

```
agent-image/
├── config.toml          # REQUIRED — identity, provider, capabilities
├── AGENT.md             # Optional — behavior description
├── BOOTSTRAP.md         # Optional — initial system prompt
├── IDENTITY.md          # Optional — name, personality
├── SOUL.md              # Optional — core values
├── TOOLS.md             # Optional — tool usage guidelines
├── USER.md              # Optional — user context
├── projects/            # Optional — knowledge / workspaces
├── memories/            # Optional — seeded long-term memory
├── tools/               # Optional — custom executable tools
├── skills/              # Optional — reusable skill definitions
└── mcp.json             # Optional — MCP server configurations
```

Only `config.toml` is mandatory. Everything else is discovered and loaded dynamically if present. The `sessions/` directory is never part of an image — it belongs to the instance.

### 1.3 Minimal config.toml

```toml
[agent]
name    = "researcher"
version = "1.0.0"

[provider]
provider_type = "anthropic"
model         = "claude-sonnet-4-6"

# Optional: inherit from a base image
[base]
image = "pekohub.com/agents/base-researcher:v2"

# Optional: declare capability dependencies
[capabilities]
tools  = ["github", "browser"]
skills = ["research"]
mcps   = ["vector-store-memory"]
```

---

## 2. Runtime Workspace

### 2.1 .pekobot/ Directory

The runtime maintains a single workspace rooted at `.pekobot/`. This can live at the project level (like `.git/`) or globally at `~/.pekobot/`. When both exist, project-level takes precedence.

```
.pekobot/
├── config.toml          # Runtime configuration (daemon port, registry URLs, etc.)
├── registry/            # Local image cache
│   └── images/          # Content-addressable layer store
├── teams/               # All team workplaces
│   └── team-name/
│       ├── config.toml  # Team definition
│       ├── shared/      # Shared workplace (all agents can read/write)
│       │   ├── memory/  # Shared vector store data
│       │   └── files/   # Shared file scratch space
│       └── agents/      # Per-agent private workplaces
│           ├── coordinator/
│           │   ├── sessions/    # JSONL conversation history
│           │   ├── memories/    # Private long-term memory
│           │   └── workspace/   # Private scratch files
│           └── researcher-1/
│               ├── sessions/
│               └── workspace/
└── run/                 # Runtime state (PID files, sockets, etc.)
```

### 2.2 Standalone Agent Instances

An agent running outside a team gets its own instance directory under `.pekobot/agents/`:

```
.pekobot/
└── agents/
    └── my-assistant/
        ├── sessions/
        ├── memories/
        └── workspace/
```

### 2.3 Image Versioning

Images follow semantic versioning. The runtime resolves image references in this order:

1. Local image cache (`.pekobot/registry/images/`)
2. Configured registries in order (`pekohub.com` by default, then any private registries)
3. Filesystem path if the reference starts with `./` or `/`

> **RULE:** An instance always pins to the exact image digest it was created from. Pulling a new tag does not silently update running instances. Upgrade is explicit: `pekobot instance upgrade <name> --to <image:tag>`.

---

## 3. Daemon

### 3.1 Overview

Pekobot runs as a local daemon (`pekobot daemon start`) that exposes an HTTP API on localhost. This is the single point of control for all runtime operations — the CLI, TUI, Web UI, and external integrations all talk to the daemon, never to agents directly.

> **NOTE:** Default port: `11434` (matches Ollama convention for familiarity). Configurable in `.pekobot/config.toml`.

### 3.2 Inbound HTTP API

| Method + Path | Description | Notes |
|---------------|-------------|-------|
| `POST /agents/{id}/chat` | Send a message to an agent instance | Streams response via SSE |
| `GET  /agents` | List all instances | |
| `POST /agents` | Create a new instance from image | |
| `DELETE /agents/{id}` | Stop and remove instance | |
| `GET  /agents/{id}/sessions` | List sessions for instance | |
| `GET  /teams` | List all teams | |
| `POST /teams` | Deploy a team from config | |
| `DELETE /teams/{id}` | Stop and teardown team | |
| `GET  /images` | List local images | |
| `POST /images/pull` | Pull image from registry | |
| `POST /images/build` | Build image from directory | |
| `GET  /events` | Subscribe to system event stream | WebSocket |
| `GET  /health` | Daemon health check | |

### 3.3 Outbound Traffic and Hooks

The daemon supports outbound traffic through a hook system for proactive agent activation. This is what enables cron-style agents and webhook receivers.

| Hook Type | Trigger | Use Case |
|-----------|---------|----------|
| `cron` | Time-based schedule (crontab syntax) | Daily digest agent, periodic data sync |
| `webhook` | Inbound HTTP POST to daemon endpoint | GitHub event → code review agent |
| `event` | Internal event bus message | New team message → notification agent |
| `file_watch` | Filesystem change in watched path | New file in `inbox/` → processing agent |

Hooks are declared in the agent's `config.toml`:

```toml
[[hooks]]
type     = "cron"
schedule = "0 8 * * *"   # Every day at 8am
action   = "run"         # Start a session with the trigger as input

[[hooks]]
type   = "webhook"
path   = "/hooks/github"
action = "run"
```

> **NOTE:** When a hook triggers a session, the trigger payload becomes the first user message. Session ownership and history work identically to interactive sessions. There is no "background session" special case.

---

## 4. Core Runtime

### 4.1 Philosophy: Minimal and Fast

The core runtime is a minimal Rust process optimized for latency and correctness. It handles exactly five things:

- **Agentic loop** — the turn-based cycle of receive → think → act → respond
- **Tool calling** — synchronous (blocking) and asynchronous (parallel) tool execution
- **Subagent spawning** — create child agent instances within a session
- **Session management** — durable JSONL session storage with atomic writes
- **Agent-to-Agent (A2A) communication** — via the team event bus

Everything else — custom tools, MCP integrations, skills, session plugins — is handled through capability extensions. The core runtime does not import or know about these. It only knows how to invoke them through a stable interface.

### 4.2 Agentic Loop

```rust
loop {
    let input  = recv_next_input().await;    // from user, hook, or A2A bus
    let output = llm_complete(input).await;  // blocking LLM call

    for tool_call in output.tool_calls {
        if tool_call.is_async {
            spawn_tool(tool_call);            // fire and forget, result arrives later
        } else {
            let result = run_tool(tool_call).await;
            feed_result_to_context(result);
        }
    }

    if output.is_final { break; }
}
```

### 4.3 Tool Calling: Sync and Async

| Mode | Behavior |
|------|----------|
| **Synchronous** | The agent waits for the tool result before continuing. Default for most tools. Suitable for short-lived operations (file read, search query). |
| **Asynchronous** | The agent continues processing and the tool result is injected into context when ready. Declared with `async = true` in the tool spec. Suitable for long-running operations (browser fetch, code execution). |

### 4.4 Subagent Spawning

An agent can spawn child instances within its own session scope. The parent waits for the child to complete and receives its output as a tool result. This enables parallelism without requiring team-level orchestration.

```json
{
  "tool": "spawn_agent",
  "args": {
    "image": "researcher:v2",
    "task":  "Summarize the Q4 earnings report at <url>",
    "async": false
  }
}
```

### 4.5 Session Management

Sessions are stored as JSONL files in the instance's `sessions/` directory. Each line is one event (user turn, assistant turn, tool call, tool result, metadata).

- Writes are atomic: append to a `.tmp` file, then rename
- Multiple sessions per instance are supported
- Sessions can be branched: `pekobot session branch <session-id>`
- Session plugins (e.g. lossless compression, summarization) hook into the write path via the capability extension interface

---

## 5. Agent Event Bus

### 5.1 All A2A Communication Goes Through the Bus

Agents within a team never call each other directly. All inter-agent communication is mediated by the team's event bus. This provides:

- **Decoupling** — agents have no knowledge of each other's address or state
- **Observability** — every message is logged and replayable
- **Backpressure** — the bus can buffer messages if a consumer is busy
- **Extensibility** — filters, routers, and monitors attach to the bus without modifying agents

### 5.2 Message Types

| Type | Direction | Description |
|------|-----------|-------------|
| `Direct` | One agent → one agent | Private message to a named instance. Not broadcast. |
| `Task` | Coordinator → worker | Request to perform a specific task. Worker replies with `TaskResult`. |
| `TaskResult` | Worker → coordinator | Completion (or failure) of a `Task`. Contains output payload. |
| `Broadcast` | One → all | Publish to all subscribers. Used for events and announcements. |
| `Subscribe` | Agent → bus | Register interest in a topic pattern. Agent receives matching messages. |

### 5.3 Bus Backends

The bus backend is pluggable. The runtime selects based on deployment context.

| Backend | When to Use |
|---------|-------------|
| **In-memory** (default) | Single-process teams. Zero latency. No persistence. Suitable for most development and production single-node deployments. |
| **Redis Streams** | Multi-process or multi-host teams. Persistent. Supports consumer groups for load balancing across worker instances. |
| **NATS** | High-throughput or geographically distributed teams. Supports JetStream for durable delivery. |

---

## 6. Team Composition

### 6.1 team.toml

A team is declared in a single TOML file. The runtime creates the `.pekobot/teams/<name>/` workspace on deploy.

```toml
[team]
name = "research-team"

[[agents]]
name      = "coordinator"
image     = "./agents/coordinator"   # local filesystem image
instances = 1
role      = "coordinator"

[[agents]]
name      = "researcher"
image     = "pekohub.com/agents/researcher:v2.5"
instances = 3
role      = "worker"

[[agents]]
name      = "writer"
image     = "pekohub.com/agents/writer:v1.0"
instances = 1

[shared.memory]
type    = "chroma"
persist = true

[shared.bus]
backend = "in-memory"   # or redis / nats

[shared.files]
path = ".pekobot/teams/research-team/shared/files"
```

### 6.2 Shared Services Fabric

Heavy infrastructure is instantiated once per team and shared across all agent instances via reference counting. When the last consumer disconnects, the shared service is torn down.

| Shared Service | Description |
|----------------|-------------|
| **Vector memory** | A single vector store (Chroma, Qdrant, etc.) accessible by all agents in the team. Each agent namespaces its writes to avoid collisions. |
| **File workspace** | A shared directory all agents can read from and write to. Useful for passing documents and artifacts between agents. |
| **Browser MCP** | A single browser instance shared by all agents needing web access. Eliminates the cost of spinning up multiple browser processes. |
| **Event bus** | One bus per team. All A2A messages route through it. |
| **External MCPs** | Any MCP server declared in `[shared]` is started once and proxied to all agents. |

#### Vector Memory Namespacing

When using shared vector memory, each agent instance writes to an isolated namespace to prevent collisions:

| Namespace Pattern | Example | Access |
|-------------------|---------|--------|
| Private instance namespace | `{instance_id}` (e.g., `inst_7k2mxp3q`) | Read/write by owning instance only |
| Agent-type namespace | `{agent_name}` (e.g., `researcher`) | Read/write by all instances of that agent type |
| Team shared namespace | `_team_shared` | Read/write by all team members |

**Default behavior:**
- `memory_store` without explicit namespace → writes to private instance namespace
- Cross-namespace reads allowed with explicit `namespace` parameter

**Configuration in `config.toml`:**
```toml
[capabilities.grants]
memory_namespaces = ["_team_shared", "researcher"]  # Allow access to these namespaces
```

---

## 7. Interfaces

### 7.1 CLI

The CLI talks to the daemon via HTTP. All commands are non-interactive — suitable for scripting and CI pipelines.

```bash
# Agent lifecycle
pekobot run ./my-agent/                    # Create instance from local image, attach
pekobot run researcher:v2 --detach         # Run detached instance
pekobot stop <instance-id>
pekobot rm <instance-id>
pekobot ps                                 # List running instances
pekobot logs <instance-id> --follow

# Image management
pekobot build ./my-agent/ -t my-agent:v1.0
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0
pekobot pull pekohub.com/agents/researcher:v2
pekobot images

# Team management
pekobot team deploy -f team.toml
pekobot team scale research-team researcher 5
pekobot team stop research-team
pekobot team ps

# Session management
pekobot session list <instance-id>
pekobot session show <session-id>
pekobot session branch <session-id>

# Daemon
pekobot daemon start
pekobot daemon stop
pekobot daemon status
```

### 7.2 TUI

The TUI is a terminal user interface built on top of the HTTP API. It is a separate binary (`pekobot-tui`) and has no privileged access to agent internals. It provides:

- Live view of running instances and teams
- Interactive chat with any running instance
- Event bus monitor (tap into the bus stream for a team)
- Log tail with filtering

### 7.3 Web UI

A lightweight web UI is served by the daemon at `http://localhost:11434/ui`. It provides the same capabilities as the TUI via a browser interface. No build step — single static HTML file served from the daemon binary.

### 7.4 External Channels

External channels (Discord, Slack, email, etc.) are not first-party features. They are third-party integrations that connect to the daemon's HTTP API. The daemon exposes a generic webhook endpoint (`/webhooks/{agent-id}/{token}`) that channels POST messages to. Channel adapters handle formatting and authentication externally.

> **RULE:** Pekobot does not ship Discord bots or Slack apps. It ships a stable HTTP API that makes them trivial to build.

### 7.5 Game and Service Integration

Games and other services connect to the daemon's WebSocket endpoint for bidirectional streaming. The agent appears as a local service at `ws://localhost:11434/agents/{id}/ws`. The wire format is JSON with a simple envelope schema.

---

## 8. Capability Extension

### 8.1 Extension Points

The core runtime exposes four stable extension interfaces. Capabilities plug in through these interfaces; the runtime core does not change.

| Extension Point | Description |
|-----------------|-------------|
| **Tool** | Executable script or binary. Any language. Invoked by the runtime via stdin/stdout with a JSON protocol. Lives in the agent's `tools/` directory or installed globally. |
| **MCP Server** | External process implementing the Model Context Protocol. The runtime acts as MCP client and proxies tool calls through. |
| **Skill** | A packaged prompt template + optional tool bundle. Composable and shareable. Declared in `skills/` and referenced in config. |
| **Session Plugin** | Hooks into the session write/read path. Used for compression, summarization, encryption. Implements the `SessionPlugin` trait. |

### 8.2 Capability Declaration

Agents declare their capability dependencies in `config.toml`. The runtime validates and, if auto-install is enabled, installs missing capabilities before starting the instance.

```toml
[capabilities]
tools  = ["github", "browser"]
skills = ["research", "citation-formatter"]
mcps   = ["vector-store-memory", "code-executor"]

[capabilities.session]
plugin = "lossless-compression"
```

---

## 9. Upper Layers (Planned — Post-Runtime)

These layers will be built after the runtime is stable. They are architecturally separate from the runtime and communicate with it exclusively through the daemon HTTP API.

### 9.1 Agent Control Plane

The control plane manages the runtime. Where the runtime *executes* agents, the control plane *governs* them.

| Responsibility | Description |
|----------------|-------------|
| **Lifecycle management** | Start, stop, restart, upgrade instances on schedule or on demand. Handles crash recovery with configurable restart policies. |
| **Instance scheduling** | Decide which host/process an instance runs on. In single-node deployments this is trivial; in multi-node it is the core value. |
| **Health monitoring** | Periodic health checks against running instances. Restarts unhealthy instances. Emits health events to the bus. |
| **Resource allocation** | CPU and memory budgets per instance. Enforced via OS mechanisms (cgroups on Linux). |
| **Policy enforcement** | Capability grants, rate limits, and access control at the instance level. |
| **Audit logs** | Structured log of all instance lifecycle events, tool calls, and A2A messages. Queryable. |

### 9.2 Capability Ecosystem

A package manager and registry for agent capabilities — the npm for agents. Capabilities are installable, versioned, and shareable.

```bash
# Install capabilities
pekobot install tool:web-browser
pekobot install skill:researcher
pekobot install mcp:vector-store-memory
pekobot install session:lossless-compression

# Publish a capability
pekobot capability publish ./my-tool/ -t pekohub.com/tools/my-tool:v1.0

# List installed capabilities
pekobot capability list
```

| Capability Type | Description |
|-----------------|-------------|
| `tool:` | An executable tool installable into any agent. Versioned binary or script bundle. |
| `skill:` | A prompt + tool bundle. Gives an agent a reusable, composable behaviour pattern. |
| `mcp:` | A packaged MCP server. Installed as a managed process, proxied to agents that declare it. |
| `session:` | A session plugin. Installed as a shared library loaded by the runtime session manager. |

When an agent declares capabilities in `config.toml`, the runtime checks if they are installed. If auto-install is enabled (default in development), missing capabilities are fetched from the configured registry automatically before the instance starts.

---

## 10. Architecture Layers Summary

| Layer | Status | Responsibility |
|-------|--------|----------------|
| **Capability Ecosystem** | Planned (Phase 3) | Package manager and registry for tools, skills, MCPs, session plugins |
| **Control Plane** | Planned (Phase 2) | Lifecycle management, scheduling, health, policy, audit |
| **Daemon + HTTP API** | Phase 1 | Single control point; inbound API, outbound hooks, event routing |
| **Team Runtime** | Phase 1 | Team composition, shared services fabric, event bus |
| **Core Runtime** | Phase 1 | Agentic loop, tool calling, subagent spawn, session management |
| **Agent Image / Instance** | Phase 1 | Definition format, versioning, workspace isolation |

---

## 11. Implementation Notes

### 11.1 Technology Choices

| Component | Technology |
|-----------|------------|
| Core runtime | Rust — for latency, safety, and memory predictability |
| Session storage | JSONL files — human-readable, git-diffable, appendable |
| Structured persistence | SQLite — zero-dependency, embedded, sufficient for single-node |
| Config format | TOML for agents and runtime; YAML also accepted for team definitions |
| HTTP API | `axum` (Rust) — async, type-safe, minimal |
| Event bus (default) | In-memory Tokio channels — zero latency, no dependencies |
| Package format | OCI-inspired content-addressable layers, SHA-256 digests |
| TUI | `ratatui` (Rust) |

### 11.2 Key Design Constraints

- The core runtime must have zero knowledge of specific capability implementations (no browser imports, no Chroma imports). Only the extension interfaces are visible.
- Session writes must be atomic. A crash mid-write must never corrupt an existing session.
- The daemon must be startable with no configuration. Sensible defaults for everything.
- CLI commands must be composable and scriptable. No interactive prompts, ever.
- An agent instance must pin to an exact image digest. Implicit upgrades are not permitted.

### 11.3 What Is Explicitly Out of Scope

- Enterprise RBAC — capability grants are instance-level only in the runtime
- Content moderation — the operator's responsibility
- Managed cloud hosting — self-hosted first; cloud is a future consideration
- Visual agent builder — code and text only
- First-party Discord/Slack bots — use the webhook API

---

*Version: 4.0 · Last Updated: 2026-03-16 · Status: Draft*
