# ADR-020: Daemon-Based Async Tool Execution

**Status**: Completed  
**Date**: 2026-04-16  
**Last Updated**: 2026-04-17  
**Author**: Kimi Code CLI  
**Depends On**: ADR-018a (Tool Execution Unification)  
**Superseded By**: ADR-021 (Daemon as Central Runtime)

## Context

The current async tool execution implementation (Option 3: minimal file-based polling) uses `tokio::spawn` within the agent process to run background tasks. This works for long-running server processes, but breaks for the CLI (`peko send`) because:

1. The tokio runtime is dropped when the CLI process exits, aborting background tasks
2. Task files may be left in an incomplete state
3. Cross-session polling (e.g. E2E tests) fails because the task process no longer exists

A temporary workaround was added where the CLI waits up to 30 seconds for background tasks before exiting. This fixes tests but creates poor UX for real CLI usage — a user running `peko send` with a 5-minute async build would see the CLI block for 5 minutes after the agent already responded.

Additionally, `src/commands/send.rs` uses `StatelessAgentService::execute_message_streaming()` directly and does **not** call `wait_for_async_tasks()`, meaning async tasks are already at risk of being dropped in the primary CLI path.

## Problem Statement

### Current Architecture (In-Process Background Tasks)

```
peko send "run long build"
    │
    ▼
┌─────────────────────────────────────────┐
│  AgenticLoopV4                          │
│  ───────────────                        │
│  • LLM calls shell with _async=true     │
│  • AsyncExecutionRouter spawns task     │
│  • Receipt returned immediately         │
│  • Agent may poll task_file             │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  tokio::spawn(async {                   │
│    shell_tool.execute()                 │
│    write_task_file()                    │
│  })                                     │
│                                         │
│  ⚠️ DROPPED when CLI exits              │
└─────────────────────────────────────────┘
```

### Key Issues

| Issue | Impact |
|-------|--------|
| Background tasks die with process | Async work lost between CLI invocations |
| CLI blocks on exit (workaround) | Poor UX for genuine long-running tasks |
| `send.rs` has no wait protection | Async tasks already broken in main CLI path |
| No centralized task management | Can't list, cancel, or monitor tasks globally |
| Task files may be orphaned | No cleanup of zombie task records |

## Decision

Move the `UnifiedAsyncExecutor` and all async task lifecycle management into the **peko daemon**. The CLI will submit async tasks to the daemon via HTTP IPC and receive receipts immediately. The daemon owns task execution, task file updates, and cleanup.

To keep the architecture clean, we introduce:

1. An **`AsyncTaskTransport` trait** that abstracts whether tasks run locally (daemon mode) or remotely (CLI client mode). The `AsyncExecutionRouter` routes based on parameters; it does **not** contain transport logic.
2. A **`ToolRuntime`** component that is extracted from `Agent` so the daemon can resolve and execute tools without instantiating a full agent. This is the long-term, future-proof architecture (Option B).

### Target Architecture

```
peko send "run long build"
    │
    ▼
┌─────────────────────────────────────────┐
│  CLI / Agent                            │
│  ───────────                            │
│  • Build async tool call                │
│  • Submit to daemon via HTTP API        │
│  • Receive receipt immediately          │
│  • Return response to user              │
│  • Exit cleanly — task survives!        │
└─────────────────────────────────────────┘
    │
    ▼ HTTP IPC (localhost:11435)
┌─────────────────────────────────────────┐
│  Peko Daemon                         │
│  ───────────────                        │
│  ┌─────────────────────────────────┐    │
│  │  ToolRuntime                    │    │
│  │  ───────────                    │    │
│  │  • Tool registry (ExtensionCore)│    │
│  │  • Built-in tool initialization │    │
│  │  • Tool resolution by name      │    │
│  │  • Workspace context per agent  │    │
│  └─────────────────────────────────┘    │
│  ┌─────────────────────────────────┐    │
│  │  UnifiedAsyncExecutor           │    │
│  │  ─────────────────────          │    │
│  │  • Spawns tokio tasks           │    │
│  │  • Writes task files            │    │
│  │  • Enforces timeouts            │    │
│  │  • Handles cancellation         │    │
│  └─────────────────────────────────┘    │
│  ┌─────────────────────────────────┐    │
│  │  Task File Janitor              │    │
│  │  ────────────────               │    │
│  │  • Cleans old task files        │    │
│  │  • Removes orphaned records     │    │
│  └─────────────────────────────────┘    │
└─────────────────────────────────────────┘
```

## Consequences

### Positive

- **True background execution**: CLI can exit immediately; tasks survive
- **Centralized management**: Single source of truth for all async tasks
- **Observability**: Enables `peko tasks` command to list/monitor/cancel tasks
- **Resource control**: Daemon can enforce global limits on concurrent async tasks
- **Clean CLI UX**: No blocking wait hack needed
- **SRP preserved**: Transport logic is extracted from the router; tool runtime is separate from agent lifecycle
- **Future-proof**: `ToolRuntime` can be reused by other daemon services (cron jobs, webhooks, future job queues)

### Negative

- **Daemon dependency for async tools**: If the daemon is not running, async tools cannot execute (sync tools remain unaffected)
- **IPC overhead**: One additional HTTP round-trip per async tool call
- **Refactoring cost**: `Agent::init_builtins_async()` must be extracted into `ToolRuntime`
- **Larger initial scope**: Touches daemon, API routes, CLI transport, async framework, and agent initialization

## Implementation Plan

### Phase 0: Extract `ToolRuntime` from `Agent`

Currently, `Agent::init_builtins_async()` (`src/agent/agent.rs:48`) initializes built-in tools and registers them with an `ExtensionCore` that is owned by the `Agent`. The daemon has no `Agent`, so it cannot execute tools.

We extract a reusable **`ToolRuntime`** component:

```rust
pub struct ToolRuntime {
    extension_core: Arc<ExtensionCore>,
    path_resolver: PathResolver,
}

impl ToolRuntime {
    pub async fn new(path_resolver: PathResolver) -> Result<Self> {
        let extension_core = Arc::new(ExtensionCore::new());
        // Register built-in tools (extracted from Agent::init_builtins_async)
        Self::register_builtins(&extension_core, &path_resolver).await?;
        Ok(Self { extension_core, path_resolver })
    }

    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        workspace: &std::path::Path,
    ) -> Result<serde_json::Value> {
        // Resolve tool via ExtensionCore and execute
        // ...
    }
}
```

**Refactoring steps:**
1. Move the built-in tool registration logic from `Agent::init_builtins_async()` into `ToolRuntime::register_builtins()`.
2. `Agent::init_builtins_async()` delegates to `ToolRuntime::register_builtins()`.
3. `Agent` holds a `ToolRuntime` instead of directly owning `ExtensionCore` (or both hold `Arc<ExtensionCore>`).
4. The daemon's `AppState` holds a shared `Arc<ToolRuntime>`.

This ensures both `Agent` and daemon initialize tools the same way, without duplication.

### Phase 1: Add `ToolRuntime` and `UnifiedAsyncExecutor` to Daemon State

The daemon's `AppState` (`src/api/state.rs`) must hold both components:

```rust
pub struct AppState {
    // ... existing fields ...
    pub tool_runtime: Arc<ToolRuntime>,
    pub async_task_executor: Arc<UnifiedAsyncExecutor>,
}
```

Both are constructed during `AppState::build()` and live for the lifetime of the daemon.

### Phase 2: Daemon HTTP API Routes

Add new routes under `/async/tasks` in `src/api/routes/async_tasks.rs`:

| Method | Path | Description |
|--------|------|-------------|
| POST | `/async/tasks` | Spawn a new async task |
| GET | `/async/tasks/{id}` | Get task status and result |
| DELETE | `/async/tasks/{id}` | Cancel a task |
| GET | `/async/tasks` | List tasks (optionally filter by `session_key`) |

**Request body (`SpawnAsyncTaskRequest`):**
```rust
pub struct SpawnAsyncTaskRequest {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: serde_json::Value,
    pub session_key: String,
    pub workspace: std::path::PathBuf,  // Needed for tool context
    pub config: AsyncToolConfig,
}
```

**Route handler logic:**
1. Look up the tool in `state.tool_runtime`
2. Build the `sync_executor` closure using `tool_runtime.execute_tool()`
3. Submit the closure to `state.async_task_executor.execute()`
4. Return `AsyncTaskReceipt` to the caller

Request/response types mirror `AsyncTaskReceipt` and `TaskFileRecord` exactly so that the agent sees the same contract regardless of transport.

### Phase 3: `AsyncTaskTransport` Abstraction

Introduce a transport trait in `src/extensions/services/async_transport.rs`:

```rust
#[async_trait::async_trait]
pub trait AsyncTaskTransport: Send + Sync {
    async fn spawn_task(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        tool_context: &ToolExecutionContext,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt>;

    async fn get_status(&self, task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>>;
    async fn cancel_task(&self, task_id: &AsyncTaskId) -> Result<bool>;
}
```

Two implementations:

- **`LocalAsyncTransport`** — wraps `UnifiedAsyncExecutor::execute()` directly (used in daemon)
- **`DaemonHttpTransport`** — submits via `ApiClient` to `/async/tasks` (used in CLI)

The `AsyncExecutionRouter` is constructed with a `Box<dyn AsyncTaskTransport>`:

```rust
pub struct AsyncExecutionRouter {
    transport: Box<dyn AsyncTaskTransport>,
    default_sync_timeout: Duration,
    default_async_timeout: Duration,
}
```

This eliminates the need for `is_daemon_mode()` branching inside the router.

### Phase 4: CLI Transport Integration

In CLI mode, construct the `AsyncExecutionRouter` with `DaemonHttpTransport`.

The transport uses the existing `ApiClient` (`src/api/client.rs`), extended with async-task methods:

```rust
impl ApiClient {
    pub async fn spawn_async_task(&self, req: &SpawnAsyncTaskRequest) -> Result<AsyncTaskReceipt, ClientError>;
    pub async fn get_async_task(&self, task_id: &str) -> Result<AsyncTaskStatusResponse, ClientError>;
    pub async fn cancel_async_task(&self, task_id: &str) -> Result<(), ClientError>;
}
```

If the daemon is unreachable, the transport returns an error. The CLI may optionally auto-start the daemon (see *Out of Scope* below).

### Phase 5: Remove CLI Wait Hacks

Once daemon submission is active:

1. Remove `agent.wait_for_async_tasks()` from `src/channels/cli.rs:439-446`
2. No change needed in `src/commands/send.rs` because the async task now lives in the daemon, not the CLI process

### Phase 6: Task File Janitor

Register a daemon-internal cron job (reusing the existing `CronScheduler`) that:

- Scans `async_tasks/` directory
- Removes task files older than a configurable TTL (default 24h)
- Calls `registry.cleanup_completed()` to purge stale in-memory entries

## Affected Components

| Component | Changes |
|-----------|---------|
| `src/agent/agent.rs` | Delegate built-in tool init to `ToolRuntime` |
| `src/runtime/tool_runtime.rs` | **New** — extracted tool registry and execution |
| `src/api/state.rs` | Add `ToolRuntime` and `UnifiedAsyncExecutor` to `AppState` |
| `src/api/routes/mod.rs` | Merge `/async/tasks` router |
| `src/api/routes/async_tasks.rs` | **New** — HTTP endpoints for task lifecycle |
| `src/api/client.rs` | Add async task methods to `ApiClient` |
| `src/agent/async_tool_framework.rs` | Ensure `AsyncTaskReceipt` is serializable for HTTP |
| `src/extensions/services/async_router.rs` | Inject `AsyncTaskTransport`, remove local-only assumptions |
| `src/extensions/services/async_transport.rs` | **New** — `LocalAsyncTransport` and `DaemonHttpTransport` |
| `src/channels/cli.rs` | Remove `wait_for_async_tasks` hack |
| `src/daemon/mod.rs` | Register janitor cron job (optional) |

## Migration Path

1. **Daemon-first**: When daemon is running, all async tasks are submitted via HTTP
2. **Graceful degradation**: If daemon is unreachable, return a clear error:  
   `"Async tool execution requires the peko daemon. Start it with: peko daemon start"`
3. **No in-process fallback**: The old `tokio::spawn` path is removed from the CLI entirely. The daemon always uses `LocalAsyncTransport`.

## Out of Scope (Separate ADRs)

### ADR-020a: CLI Auto-Start Daemon
The question of whether *all* CLI commands should automatically start the daemon if it is not running is a product-level decision that affects every command (`peko send`, `peko agent create`, `peko ext enable`, etc.). This ADR focuses **only** on async tool execution transport. If auto-start is desired, it should be specified in a follow-up ADR so that:
- The daemon-start logic can be centralized (`ensure_daemon_running()`)
- Commands that do not need the daemon (e.g. pure config operations) can opt out
- UX and offline-usage implications are considered explicitly

### ADR-020b: Daemon-Mode Detection
Instead of an `is_daemon_mode()` global function, the daemon sets an environment variable (`PEKO_DAEMON_MODE=1`) during startup. Components that need to know their runtime context read this variable at initialization time. This is documented here but does not require a separate ADR unless it grows into a broader runtime-profile system.

## Implementation Status

**Completed: 2026-04-17**

All phases were implemented:

| Phase | Component | Status | Files |
|-------|-----------|--------|-------|
| Phase 0 | `ToolRuntime` extraction | ✅ | `src/runtime/mod.rs`, `src/runtime/tool_runtime.rs` |
| Phase 1 | Add to `AppState` | ✅ | `src/api/state.rs` |
| Phase 2 | HTTP API routes | ✅ | `src/api/routes/async_tasks.rs`, `src/api/routes/mod.rs` |
| Phase 3 | `AsyncTaskTransport` trait | ✅ | `src/extensions/services/async_transport.rs` |
| Phase 4 | `ApiClient` extension | ✅ | `src/api/client.rs` |
| Phase 5 | Remove CLI wait hacks | ✅ | `src/channels/cli.rs`, `src/agent/stateless_service.rs` |
| Phase 6 | Task file janitor | ✅ | `src/daemon/mod.rs`, `UnifiedAsyncExecutor::run_janitor()` |

**Key implementation details:**
- `Agent::init_builtins_async()` delegates to `ToolRuntime::register_builtins()` (Phase 0)
- `create_transport()` in CLI attempts `DaemonHttpTransport`, falls back to `LocalAsyncTransport`
- `main.rs` initializes global `ExtensionCore` with the appropriate transport before command dispatch
- Workspace propagation through the full async execution chain (HookInput::ToolCall.workspace)
- `BuiltinToolAdapter` updated to extract and forward workspace from `HookInput`

**Known limitation (addressed by ADR-021):**
The daemon's `ToolRuntime` only registers built-in tools (`ShellTool`, `ReadFileTool`, etc.). MCP tools and universal tools are not registered in the daemon's `ExtensionCore` — they are registered per-agent in the CLI process. This means async tool execution is currently limited to built-in tools only. ADR-021 addresses this by making the daemon the central runtime with the full extension registry.

## References

- Current async implementation: `src/agent/async_tool_framework.rs`
- Async router: `src/extensions/services/async_router.rs`
- CLI wait hack: `src/channels/cli.rs:439-446`
- Existing daemon module: `src/daemon/mod.rs`
- Existing API client: `src/api/client.rs`
- E2E test contract: `e2e_tests/extensions/tools/tool_async.ps1`
- Agent tool initialization: `src/agent/agent.rs:48`
- ADR-040: Tool Timeout and Async Refactor (2026-06-23) — supersedes the `_async`/`_timeout` parameter-injection layer.
