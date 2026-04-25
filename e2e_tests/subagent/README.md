# Subagent Spawn E2E Tests

This directory contains end-to-end tests for the `agent_spawn` tool and related subagent functionality.

## Prerequisites

- Daemon must be running: `peko daemon start`
- API key configured for the chosen provider (default: `minimax`)
- `peko` CLI built and available on PATH

## Test Files

| Test | File | What it tests |
|---|---|---|
| **Blocking Mode** | `subagent_blocking.ps1` | Default blocking spawn — parent waits for subagent to complete its agentic loop and returns inline result |
| **Async Mode** | `subagent_async.ps1` | `_async: true` spawn — returns receipt immediately, agent polls `task_file` for progress and result |
| **Nesting & Depth** | `subagent_nesting.ps1` | Subagents spawning subagents, depth limit enforcement, result bubbling |
| **Session Isolation** | `subagent_isolation.ps1` | `isolated=true` vs `isolated=false`, workspace inheritance, cleanup policies |
| **Status & List** | `subagent_status_list.ps1` | `agent_spawn_status` and `agent_spawn_list` tools for monitoring runs |
| **All Tests** | `subagent_all.ps1` | Runs the complete suite in sequence |

## Running Tests

### Individual test
```powershell
.\e2e_tests\subagent\subagent_blocking.ps1 -Provider minimax
```

### Full suite
```powershell
.\e2e_tests\subagent\subagent_all.ps1 -Provider minimax
```

## Ideal UX Design

### Blocking Mode (Default)

```json
// Agent calls agent_spawn with task
{
  "task": "Use write_file to create hello.txt with 'Hello World'",
  "isolated": false
}

// Tool returns inline result after subagent completes its agentic loop
{
  "status": "completed",
  "output": "File hello.txt created successfully with 11 bytes.",
  "child_session_key": "agent:default:test:peer:user:alice:overlay:spawn:..."
}
```

The parent agent **waits** for the subagent to finish. The subagent runs a full agentic loop (can use tools, reason, etc.) and its final assistant response becomes the `output`.

### Async Mode (`_async: true`)

```json
// Agent calls agent_spawn with _async
{
  "task": "Use shell to run: Start-Sleep 30; echo done",
  "_async": true,
  "_timeout": 60
}

// Tool returns receipt immediately (parent does NOT wait)
{
  "status": "accepted",
  "task_id": "task_abc123",
  "task_file": "C:\\Users\\...\\pekobot\\async_tasks\\task_abc123.json"
}
```

The agent can then **poll the task_file** directly using `read_file` or `shell` to check progress:

```json
// task_file contents while running
{
  "task_id": "task_abc123",
  "status": "running",
  "started_at": "2026-04-25T10:00:00Z"
}

// task_file contents when complete
{
  "task_id": "task_abc123",
  "status": "completed",
  "result": { "output": "done" },
  "completed_at": "2026-04-25T10:00:15Z"
}
```

No automatic injection into the agentic loop. The agent is in full control.

## Test Verification Strategy

All tests use **deterministic verification** via the filesystem:

1. Parent agent spawns a subagent with a task to write a file
2. Test script checks if the file exists and has correct content
3. Agent response is checked for success/failure keywords

This avoids relying on LLM output variability for pass/fail decisions.
