# ADR-020: Daemon-Based Async Tool Execution

**Status**: Proposed  
**Date**: 2026-04-16  
**Author**: Kimi Code CLI  
**Depends On**: ADR-018a (Tool Execution Unification)

## Context

The current async tool execution implementation (Option 3: minimal file-based polling) uses `tokio::spawn` within the agent process to run background tasks. This works for long-running server processes, but breaks for the CLI (`peko send`) because:

1. The tokio runtime is dropped when the CLI process exits, aborting background tasks
2. Task files may be left in an incomplete state
3. Cross-session polling (e.g. E2E tests) fails because the task process no longer exists

A temporary workaround was added where the CLI waits up to 30 seconds for background tasks before exiting. This fixes tests but creates poor UX for real CLI usage — a user running `peko send` with a 5-minute async build would see the CLI block for 5 minutes after the agent already responded.

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
| No centralized task management | Can't list, cancel, or monitor tasks globally |
| Task files may be orphaned | No cleanup of zombie task records |

## Decision

Move the `UnifiedAsyncExecutor` and all async task lifecycle management into the **pekobot daemon**. The CLI will submit async tasks to the daemon via IPC and receive receipts immediately. The daemon owns task execution, task file updates, and cleanup.

### Target Architecture

```
peko send "run long build"
    │
    ▼
┌─────────────────────────────────────────┐
│  CLI / Agent                            │
│  ───────────                            │
│  • Build async tool call                │
│  • Submit to daemon via IPC             │
│  • Receive receipt immediately          │
│  • Return response to user              │
│  • Exit cleanly — task survives!        │
└─────────────────────────────────────────┘
    │
    ▼ IPC (HTTP / Unix socket / named pipe)
┌─────────────────────────────────────────┐
│  Pekobot Daemon                         │
│  ───────────────                        │
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

### Negative

- **Daemon dependency**: Async tools require daemon to be running
- **IPC complexity**: Need reliable communication between CLI and daemon
- **Larger scope**: Touches daemon, CLI, and async framework

## Implementation Plan

### Phase 1: Daemon Task API

Add async task management endpoints to the daemon:

```rust
// New daemon commands
pub enum DaemonCommand {
    // ... existing commands
    SpawnAsyncTask(SpawnAsyncTaskRequest),
    CancelAsyncTask(AsyncTaskId),
    GetAsyncTaskStatus(AsyncTaskId),
    ListAsyncTasks { session_key: Option<String> },
}

pub struct SpawnAsyncTaskRequest {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: serde_json::Value,
    pub session_key: String,
    pub config: AsyncToolConfig,
}
```

### Phase 2: IPC Transport

Choose and implement IPC between CLI and daemon:

| Option | Pros | Cons | Recommendation |
|--------|------|------|----------------|
| HTTP on localhost | Simple, cross-platform | Port conflicts, overhead | **Preferred** |
| Unix domain socket | Fast, secure | Windows support limited | Secondary |
| Named pipe (Windows) | Native on Windows | Not portable | Fallback |

**Decision**: Use HTTP on a configurable localhost port (default e.g. 7373) for maximum portability.

### Phase 3: AsyncExecutionRouter Integration

Modify `AsyncExecutionRouter` to detect whether it's running inside the daemon or a client:

```rust
impl AsyncExecutionRouter {
    pub async fn execute_async<F, Fut>(...) -> Result<Value> {
        if is_daemon_mode() {
            // Direct execution (current path)
            self.execute_local(...).await
        } else {
            // Client mode: submit to daemon
            self.submit_to_daemon(...).await
        }
    }
}
```

In client mode, the router:
1. Calls daemon HTTP API to spawn the task
2. Receives the same `AsyncTaskReceipt`
3. Returns it as JSON to the agent

### Phase 4: Remove CLI Wait Hack

Once daemon submission works, remove `agent.wait_for_async_tasks()` from `src/channels/cli.rs`.

### Phase 5: Task Janitor

Add a background cron job in the daemon that:
- Scans `async_tasks/` directory
- Removes task files older than a configurable TTL (default 24h)
- Updates registry entries for tasks that completed but weren't delivered

## Affected Components

| Component | Changes |
|-----------|---------|
| `src/daemon/mod.rs` | Add async task commands and executor |
| `src/daemon/server.rs` | HTTP endpoints for task spawn/cancel/list |
| `src/agent/async_tool_framework.rs` | Support client-mode submission |
| `src/extensions/services/async_router.rs` | Route to daemon when not in daemon mode |
| `src/channels/cli.rs` | Remove `wait_for_async_tasks` hack |
| `src/api/client.rs` | Add daemon client for async task API |

## Migration Path

1. Keep the current in-process fallback for when daemon is not running
2. Log a warning: "Async tools work best with pekobot daemon running"
3. After daemon-based async is stable, deprecate the fallback

## References

- Current async implementation: `src/agent/async_tool_framework.rs`
- Async router: `src/extensions/services/async_router.rs`
- CLI wait hack: `src/channels/cli.rs:439-446`
- Existing daemon module: `src/daemon/mod.rs`
