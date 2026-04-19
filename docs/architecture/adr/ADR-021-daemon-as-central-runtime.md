# ADR-021: Daemon as Central Runtime

**Status**: Proposed  
**Date**: 2026-04-17  
**Last Updated**: 2026-04-18  
**Author**: Kimi Code CLI  
**Depends On**: ADR-020 (Daemon-Based Async Execution)  

## Context

ADR-020 established the foundation for daemon-based async tool execution: a transport trait, HTTP routes, and a `ToolRuntime` component in the daemon. However, as implemented, the daemon only has access to **built-in tools** (ShellTool, ReadFileTool, etc.). MCP tools and universal tools are registered per-agent in the CLI process — not in the daemon.

This creates a critical limitation: async tool execution only works for built-in tools. An agent that calls an MCP tool with `_async=true` would have the daemon receive the request but fail to execute it, because `McpToolExecuteHandler` is not registered in the daemon's `ExtensionCore`.

Meanwhile, **sync tool execution** still runs entirely in the CLI process, meaning:
- The CLI must remain alive for the duration of tool execution
- Long-running sync tools block the CLI
- Resource usage is duplicated (CLI + daemon both running)

### The Root Issue

The current architecture splits execution between two runtimes:

```
CLI Process:
  └─ ExtensionCore (full registry: built-in + MCP + universal)
      └─ All sync tool calls
      └─ All async tool calls (via DaemonHttpTransport → daemon)

Daemon Process:
  └─ ToolRuntime (minimal registry: built-in only)
      └─ Async task execution for built-in tools only
```

This split is unsustainable. It limits async to a subset of tools, requires the CLI to stay alive for sync calls, and creates two divergent tool registries that must be kept in sync.

### DRY Violations in Current Code (Post-ADR-020)

The codebase currently has **three independent tool-registry initialisation paths**:

1. **Daemon** (`src/api/state.rs:229`): `ToolRuntime::new()` → registers built-ins only.
2. **Agent** (`src/agent/agent.rs:58-148`): `ToolRuntime::register_builtins()` + `ExtensionManager::load_from_directory()` → full registry (built-ins + universal + MCP).
3. **CLI** (`src/main.rs:69-93`): Global `ExtensionCore` with `AsyncExecutionRouter` → no direct tool registration; relies on Agent init later.

None of these paths share a single "initialise full registry" function. ADR-021 eliminates this duplication by centralising all registry initialisation in the daemon.

## Problem Statement

### Current Architecture (Post-ADR-020)

```
CLI (pekobot send)
    │
    ├─ Sync tool call → Agent → ExtensionCore (full registry) → executes in CLI
    │
    └─ Async tool call → DaemonHttpTransport → daemon
                              │
                              └─ ToolRuntime (built-in only) → executes in daemon
```

### Issues

| Issue | Impact |
|-------|--------|
| Async limited to built-in tools | MCP/universal tools with `_async=true` fail silently or incorrectly |
| Sync tools run in CLI | CLI must stay alive; long-running tools block the process |
| Dual tool registries | MCP server configs loaded in both CLI and daemon separately |
| Daemon underutilised | Daemon has full infrastructure but only handles async built-in tools |
| Fallback is broken | If daemon unreachable, `LocalAsyncTransport` silently loses tasks |

## Decision

Make the **daemon the central runtime** for all tool execution — sync and async. The CLI becomes a thin client that forwards requests to the daemon over HTTP. The daemon owns:

- **All tool registries** (built-in, MCP, universal)
- **All tool execution** (sync and async)
- **Session state and persistence**
- **MCP server lifecycle**

```
CLI (pekobot send)
    │
    └─ HTTP → Daemon
                 │
                 ├─ ExtensionCore (full registry)
                 ├─ MCP servers (managed lifecycle)
                 ├─ UnifiedAsyncExecutor
                 └─ SessionManager
```

This is a gradual migration. ADR-020 provides the transport infrastructure. This ADR extends it to cover all tool types and sync execution.

## Target Architecture

### Daemon Components

```
Pekobot Daemon
    │
    ├─ AppState
    │    ├─ runtime: Arc<RuntimeFacade>            ← NEW: consolidates shared tools
    │    │                                            (built-in + MCP + universal)
    │    ├─ agent_service: Arc<StatelessAgentService>
    │    ├─ session_service: Arc<SessionService>
    │    └─ lifecycle: Arc<LifecycleManager>
    │
    ├─ HTTP API
    │    ├─ POST /async/tasks      ← existing (async tool spawn)
    │    ├─ GET  /async/tasks/{id} ← existing (async status)
    │    ├─ DELETE /async/tasks/{id}
    │    ├─ GET  /async/tasks
    │    ├─ POST /tools/execute    ← NEW: synchronous tool execution
    │    └─ POST /session/message  ← NEW: stateless agentic loop in daemon
    │
    └─ MCP Server Lifecycle Manager
         └─ Health monitoring, restart on failure, config persistence
```

**AppState Refactoring (SRP Compliance)**

`AppState` currently holds 14 fields and is trending toward a God Object. To prevent this, we introduce a `RuntimeFacade` that encapsulates all **shared** tool-runtime concerns:

```rust
// src/runtime/facade.rs
pub struct RuntimeFacade {
    extension_core: Arc<ExtensionCore>,
    extension_manager: Arc<RwLock<ExtensionManager>>,
    async_task_executor: Arc<UnifiedAsyncExecutor>,
}

impl RuntimeFacade {
    pub async fn execute_tool(&self, name: &str, params: Value, workspace: &Path) -> Result<Value>;
    pub async fn execute_tool_async(&self, ...) -> Result<AsyncTaskReceipt>;
    pub async fn list_tools(&self) -> Vec<ToolMetadata>;
    pub async fn has_tool(&self, name: &str) -> bool;
}
```

`AppState` then holds `Arc<RuntimeFacade>` instead of the individual components. This keeps `AppState` as a thin composition root while `RuntimeFacade` owns the "how do I run shared tools?" responsibility.

**Important:** `RuntimeFacade` does NOT own agent-specific tools (`AgentSpawnTool`, `SessionStatusTool`, `SessionsSendTool`, etc.). Those remain per-agent because they hold agent-specific state (`SubagentExecutor`, session key provider, etc.). See Phase 1 below for the split.

### CLI Components (Thin Client)

```
pekobot send "prompt"
    │
    ├─ Parse request
    ├─ HTTP POST /session/message
    │    └─ Daemon runs AgenticLoopV4, returns event stream
    └─ Stream events back to stdout
```

The CLI no longer creates an `Agent` or `ExtensionCore`. It only:
1. Parses the command-line request
2. Forwards it over HTTP to the daemon
3. Streams the response back to the user

### Tool Execution Flow (Sync and Async)

```
CLI: peko send "use mcp:github:create_issue with _async=true"
    │
    └─ HTTP POST /async/tasks { tool_name: "mcp:github:create_issue", ... }
         │
         └─ Daemon: RuntimeFacade.has_tool("mcp:github:create_issue") → true
              └─ McpToolExecuteHandler → executes MCP tool
              └─ UnifiedAsyncExecutor.spawn() → task file written
         │
         └─ AsyncTaskReceipt { task_id, task_file }
              │
              └─ CLI: returns receipt to agent, agent polls task_file
```

## Consequences

### Positive

- **All tools work async**: MCP and universal tools can use `_async=true` with full daemon lifetime
- **CLI is thin**: No agent instantiation in CLI; no tool registry; fast startup
- **Single tool registry**: MCP servers started once in daemon, shared across all sessions
- **Sync without CLI wait**: Long-running sync tools execute in daemon; CLI only needs to stay alive for the HTTP request lifetime (much shorter than full agent session)
- **Consistent state**: Sessions, tool configs, MCP servers all in one place
- **Enables `pekobot shell`**: Interactive daemon shell becomes feasible
- **DRY restored**: One `RuntimeFacade::initialise_full_registry()` path instead of three divergent initialisation paths

### Negative

| Risk | Mitigation |
|------|------------|
| **Daemon is always required** | CLI cannot operate standalone. Document this clearly; remove the `LocalAsyncTransport` fallback in `main.rs`. |
| **Session affinity** | Sessions are persisted to JSONL (`src/session/` file-based storage), so data survives daemon restart. Runtime state (active agent turns, pending tool calls) is lost. Acceptable for v1. |
| **Network latency** | localhost HTTP round-trip (~1-2 ms). Negligible for tool execution. |
| **Migration cost** | Phased approach (see below) spreads cost over multiple releases. |
| **Security** | Daemon binds to localhost only. Future ADR can add API key auth. |

### Failure Modes

| Scenario | Behaviour |
|----------|-----------|
| Daemon crashes during agentic loop | In-flight SSE stream terminates. Client retries. Session history is persisted; the loop can resume from the last checkpoint on retry. |
| CLI disconnects mid-SSE-stream | The SSE forwarder task detects the disconnect (`sender.send()` fails) and breaks. The agentic loop continues until it tries to send the next event, at which point the `EventStream` receiver is dropped and the loop terminates. Future work may add explicit `CancellationToken` propagation. |
| MCP server dies mid-tool-call | Tool call fails with connection error. Phase 4 adds health monitoring + automatic restart before the next call. |

## Migration Phases

### Phase 1: Expand Daemon Registry (MCP + Universal Tools)

**Goal**: Register MCP tools and universal tools in the daemon's `ExtensionCore`, not just built-in tools.

**Changes:**
1. Introduce `RuntimeFacade` in `src/runtime/facade.rs` to consolidate `ToolRuntime`, `ExtensionManager`, and `UnifiedAsyncExecutor`.
2. Create a single `RuntimeFacade::initialise_full_registry()` function that:
   - Registers built-in tools via `ToolRuntime::register_builtins()`
   - Loads universal tools via `ExtensionManager::load_from_directory()`
   - Loads MCP servers via `ExtensionManager` with `McpAdapter::with_default_manager()`
3. Replace `AppState.tool_runtime` and `AppState.async_task_executor` with `AppState.runtime: Arc<RuntimeFacade>`.
4. Add `POST /tools/execute` endpoint for sync tool calls (basic tool execution without agent loop).

**Tool allowlist strategy:**

`ExtensionManager::load_from_directory()` does **not** filter tools — it registers all discovered tools with `ExtensionCore`. Filtering is performed by `ExtensionCore::register_tool()`, which checks the `tool_config.enabled` whitelist. In the current agent path (`agent.rs:100-101`), the agent sets its own `ToolConfig` on the `ExtensionCore` before loading extensions:

```rust
if let Some(ref tool_config) = self.config.tools {
    self.extension_core.set_tool_config(tool_config.clone()).await;
}
```

In the daemon, there is no single agent's config. The daemon will either:
- **Option A (default)**: Not call `set_tool_config()`, relying on `ExtensionCore`'s default behaviour (all registered tools are enabled).
- **Option B (future)**: Load a system-wide `ToolConfig` from `~/.pekobot/config.toml`.

For Phase 1, **Option A** is sufficient. The daemon is the trusted runtime; all discovered tools are available to all sessions. Per-agent filtering can be added later at the `StatelessAgentService` layer if needed.

**Registry loading in daemon:**
```rust
// src/api/state.rs — AppState::build()
let runtime = RuntimeFacade::new(path_resolver_clone.clone()).await?;
runtime.initialise_full_registry().await?;
// RuntimeFacade::new() creates ExtensionCore, ToolRuntime, ExtensionManager,
// and UnifiedAsyncExecutor internally.
```

**Agent-specific tools remain per-agent:**

The following tools are **not** moved to `RuntimeFacade` because they hold agent-specific state:

| Tool | Agent-Specific State |
|------|---------------------|
| `AgentSpawnTool` | `SubagentExecutor`, `DynamicSessionKeyProvider` |
| `AgentSpawnStatusTool` | `SubagentExecutor` |
| `AgentSpawnListTool` | `SubagentExecutor.registry()` |
| `SessionStatusTool` | `SessionManager`, `current_session_id` |
| `SessionsSendTool` | Agent name for A2A messaging context |

These tools continue to be registered by `Agent::init_builtins_async()` (or by `StatelessAgentService` when it creates the ephemeral `Agent`). The shared `ExtensionCore` from `RuntimeFacade` is passed to the agent, so the agent registers its specific tools on top of the shared registry.

**Cleanup in `main.rs` (moved from Phase 3):**

Remove the `LocalAsyncTransport` fallback and global `ExtensionCore` initialisation:

```rust
// REMOVE:
// if let Err(e) = init_extension_core(&cli.command).await { ... }
// if let Err(e) = run_extension_migration(&paths).await { ... }
//
// REPLACE WITH:
// CLI commands validate daemon reachability via ApiClient::health_check()
// and fail fast with a clear message if the daemon is not running.
```

**Feasibility: HIGH** — All components exist; this is wiring and consolidation.

---

### Phase 2: Agentic Loop in Daemon (Stateless Session API)

**Goal**: Move the full agentic loop into the daemon. CLI sends a prompt, daemon runs the loop, returns events.

**Context:** `StatelessAgentService` already exists in `AppState` (`src/api/state.rs:72`) and is used by existing API routes (`POST /agents/:name/chat`). This phase is **not** creating a new service from scratch — it is:
1. Moving the CLI `send` command from local execution to the **existing** HTTP endpoint (`POST /agents/:name/chat`).
2. Ensuring clean SSE cancellation when the CLI disconnects mid-stream.

**Endpoint:** The CLI will use the **existing** `POST /agents/:name/chat` endpoint (`src/api/routes/chat.rs`). This endpoint already:
1. Accepts `{ message, session_id, role }`
2. Calls `StatelessAgentService::execute_message_streaming()`
3. Returns SSE via `event_stream_to_sse()`

No new endpoint is needed. The CLI `send` command changes from local execution to an HTTP client of this existing endpoint.

**CLI changes:**
```rust
// src/commands/send.rs
// BEFORE: creates Agent, runs loop locally via agent_service.execute_message_streaming()
// AFTER:
let client = ApiClient::new()?;
let stream = client.chat_stream(agent_name, message, session_id).await?;
// Stream events to stdout
```

**SSE cancellation on client disconnect:**

The current `event_stream_to_sse()` (`src/api/streaming.rs:183`) forwards events from `EventStream` to SSE. If the client disconnects, `sender.send()` fails and the forwarder breaks — but the agentic loop task (spawned by `StatelessAgentService` at `stateless_service.rs:706`) may continue running because the `JoinHandle` is discarded.

To handle this cleanly, `StatelessAgentService` needs to expose the loop task handle. The internal method `execute_streaming_with_session` spawns the loop via `tokio::spawn` but drops the handle. Two implementation options:

**Option A (recommended — non-breaking):** Add a new public method:
```rust
pub async fn execute_message_streaming_cancellable(
    &self,
    request: MessageRequest,
) -> Result<(EventStream, tokio::task::JoinHandle<()>)>;
```

This delegates to a modified internal method that captures and returns the `JoinHandle`. The existing `execute_message_streaming()` can call it and discard the handle, preserving backward compatibility.

**Option B (breaking):** Change `execute_streaming_with_session` to return `(EventStream, JoinHandle<()>)` and update all callers.

With the handle exposed, the SSE handler can race them:

```rust
// In the POST /agents/:name/chat handler:
let (event_stream, loop_handle) = state.agent_service()
    .execute_message_streaming_cancellable(msg_request).await?;
let (sse_stream, forwarder_handle) = event_stream_to_sse(event_stream);

// Race between client disconnect and loop completion
tokio::select! {
    _ = forwarder_handle => {
        // Client disconnected — abort the agentic loop
        loop_handle.abort();
    }
    _ = loop_handle => {
        // Loop finished naturally — forwarder will drain remaining events
    }
}
```

**Feasibility: MEDIUM** — Requires careful SSE streaming over HTTP and clean cancellation, but `StatelessAgentService` already handles the hard part.

---

### Phase 3: Deprecate CLI Agent Instantiation

After Phase 2, these CLI code paths become dead:
- `Agent::new()` in CLI context (agents are created by daemon's `StatelessAgentService`)
- `StatelessAgentService` CLI initialisation (moved to daemon)
- Local `ExtensionCore` initialisation in CLI (already removed in Phase 1)

**Remove or deprecate:**
- `src/commands/send.rs` local execution path
- Any other CLI commands that still instantiate `Agent` directly

**Feasibility: MEDIUM** — Touching command handlers; requires testing every CLI path.

---

### Phase 4: MCP Server Lifecycle in Daemon

**Goal**: MCP servers are monitored, restarted on failure, and managed centrally by the daemon.

**Current state of MCP sharing:** `McpAdapter::with_default_manager()` uses a **global `OnceLock<Arc<RwLock<McpManager>>>`** (`src/extensions/adapters/mcp_adapter.rs:64`). This ensures that all `McpAdapter` instances share the same `McpManager`. However:

- The global is lazily initialised with `McpConfig::default()` (empty configuration).
- It is only populated when `ensure_server_config()` is called during `ExtensionManager::load_from_directory()`.
- The daemon's current path (`ToolRuntime::new()`) never creates an `McpAdapter`, so the global is **never initialised in the daemon**.

Therefore, the **singleton Arc pattern exists** but the daemon **does not currently use it**. Phase 4's real work is:

1. **Daemon-side initialisation**: Ensure `RuntimeFacade::initialise_full_registry()` triggers `McpAdapter::with_default_manager()` and loads user configs into the global manager.
2. **Health monitoring**: Periodic ping/heartbeat to each MCP server process.
3. **Restart on failure**: If a server process exits or becomes unresponsive, terminate the old process and restart it.
4. **Tool re-registration**: After restart, re-register the server's tools with `ExtensionCore`.
5. **Failure backoff**: Exponential backoff for restart loops to avoid hammering broken servers.

**Health check design (to be specified in implementation):**
```rust
// src/mcp/health.rs (new module)
pub struct McpHealthMonitor {
    check_interval: Duration,
    backoff: ExponentialBackoff,
}

impl McpHealthMonitor {
    pub async fn monitor(&self, server_name: &str, manager: &McpManager) { ... }
}
```

**Feasibility: LOW** — Health monitoring and graceful restart are genuinely new, complex work. Recommend deferring until Phase 1-3 are stable.

---

### Phase 5: CLI as Pure Relay

**Goal**: CLI has zero tool execution logic. Every operation is an RPC to the daemon.

**Commands converted one at a time:**

| Command | Phase | Daemon API Endpoint |
|---------|-------|---------------------|
| `pekobot send` | 2 | `POST /agents/:name/chat` (existing) |
| `pekobot agent create` | 5 | `POST /agents` |
| `pekobot ext enable` | 5 | `POST /extensions/{id}/enable` |
| `pekobot ext install` | 5 | `POST /extensions` |
| `pekobot session` | 5 | `GET/DELETE /session/{key}` |

**After Phase 5, the CLI package contains only:**
- CLI argument parsing (`clap`)
- HTTP client (`ApiClient`)
- Output formatting / streaming
- Daemon lifecycle commands (`daemon start`, `daemon stop`, `daemon status`)

**Feasibility: MEDIUM** — Low technical risk, but high touch-point count. Converting one command per release reduces risk.

## API Surface

### Existing (from ADR-020)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/async/tasks` | Spawn async task |
| GET | `/async/tasks/{id}` | Get task status |
| DELETE | `/async/tasks/{id}` | Cancel task |
| GET | `/async/tasks` | List tasks |

### New Endpoints

| Method | Path | Description | Phase |
|--------|------|-------------|-------|
| GET | `/session/{key}` | Get session state | 5 |
| DELETE | `/session/{key}` | Delete session | 5 |
| POST | `/tools/execute` | Execute a tool synchronously (no agent loop) | 1 |
| GET | `/tools` | List all available tools | 1 |
| GET | `/mcp/servers` | List MCP servers | 4 |
| POST | `/mcp/servers` | Add MCP server config | 4 |
| DELETE | `/mcp/servers/{name}` | Remove MCP server | 4 |
| POST | `/mcp/servers/{name}/restart` | Restart MCP server | 4 |

## Out of Scope

### CLI Auto-Start Daemon (ADR-020a, deferred)
Whether CLI commands should auto-start the daemon if it is not running is a product decision. This ADR assumes the daemon is started explicitly (`pekobot daemon start`). Auto-start can be addressed separately.

### Authentication Between CLI and Daemon
The daemon binds to localhost. Future work may add API key authentication or mutual TLS for multi-user scenarios.

### Session Persistence Across Daemon Restarts
Sessions are persisted to JSONL files (`src/session/`). A future ADR may address full session recovery (restoring active agent turns, pending tool calls) after daemon restart.

### Distributed Daemon (Multiple Hosts)
This ADR assumes a single local daemon. Multi-host deployment is out of scope.

## References

- ADR-020: Daemon-Based Async Execution (infrastructure this builds on)
- ADR-018a: Tool Execution Unification
- Daemon module: `src/daemon/mod.rs`
- Extension manager: `src/extensions/manager/mod.rs`
- API client: `src/api/client.rs`
- Stateless agent service: `src/agent/stateless_service.rs`
- Tool runtime: `src/runtime/tool_runtime.rs`
- Async transport: `src/extensions/services/async_transport.rs`
- Async router: `src/extensions/services/async_router.rs`
- MCP adapter: `src/extensions/adapters/mcp_adapter.rs`
- API streaming: `src/api/streaming.rs`
- Chat routes: `src/api/routes/chat.rs`
