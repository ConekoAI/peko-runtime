# Removed Native Async/Timeout Parameters from Built-in Tools

## Summary

Removed native `async`, `timeout`, and `stdin` parameters from the following built-in tools to avoid confusion with framework-level `_async` and `_timeout` execution control parameters:

1. **shell** - Removed `async`, `timeout_ms`, and `stdin` parameters
2. **sessions_send** - Removed `async` and `timeout_ms` parameters
3. **agent_spawn** - Removed `mode` (async/sync) and `timeout_secs` parameters

## Changes Made

### 1. Shell Tool (`src/tools/shell.rs`)

**Removed:**
- `timeout_ms` parameter from `ShellArgs`
- `r#async` parameter from `ShellArgs`
- `stdin` parameter from `ShellArgs`
- `default_timeout()` function
- `execute_async()` method
- `with_timeout()` and `with_async()` methods on `ShellTool`
- `default_timeout_ms`, `executor`, `session_key` fields from `ShellTool`
- Stdin piping logic in `execute_command()`
- Dependencies on `UnifiedAsyncExecutor`, `AsyncToolConfig`, `AsyncResultDeliveryMode`, `AsyncTaskResult`, `Uuid`

**Simplified:**
- `execute()` now always runs synchronously
- `execute_command()` no longer takes timeout or stdin parameters
- Removed timeout and stdin handling - framework handles these concerns

**Updated:**
- `llm_description()` - Removed async examples, added note about `_async` parameter
- `parameters()` schema - Removed `async`, `timeout_ms`, and `stdin` properties
- Tests - Removed `test_shell_timeout`, `test_shell_tool_with_timeout`, and `test_shell_stdin`

### 2. Sessions Send Tool (`src/tools/sessions_send.rs`)

**Removed:**
- `r#async` parameter from `SessionsSendArgs`
- `timeout_ms` parameter from `SessionsSendArgs`
- `SendMode` enum
- `default_async()` and `default_timeout()` functions
- `execute_async()` and `execute_sync()` methods
- `with_executor()` and `with_timeout()` methods on `SessionsSendTool`
- `executor` and `default_timeout_ms` fields from `SessionsSendTool`
- Dependency on `UnifiedAsyncExecutor`

**Simplified:**
- Combined `execute_async()` and `execute_sync()` into single `execute_send()` method
- `execute()` now always sends message immediately

**Updated:**
- `llm_description()` - Removed sync/async mode examples, added note about `_async`
- `parameters()` schema - Removed `async` and `timeout_ms` properties
- Tests - Simplified to single `test_send_message`

**Exports:**
- Removed `SendMode` from `src/tools/mod.rs` exports

### 3. Agent Spawn Tool (`src/tools/agent_spawn.rs`)

**Removed:**
- `SpawnMode` enum (Async/Sync modes)
- `timeout_secs` from sync mode
- `default_sync_timeout()` function
- Custom `Deserialize` implementation for `SpawnMode`
- `execute_sync()` method
- `timeout_seconds` parameter handling

**Simplified:**
- `AgentSpawnArgs` struct with only essential parameters: `task`, `label`, `isolated`, `cleanup`, `parent_session_key`
- `execute()` now always spawns asynchronously
- Execution config uses default timeout (framework's `_timeout` overrides it)

**Updated:**
- `description()` - Removed sync mode documentation, added note about `_async`
- `parameters()` schema - Removed `mode` and `timeout_seconds` properties
- Tests - Removed `test_spawn_mode_*` tests, added `test_args_parsing`

### 4. Agent Configuration (`src/agent/agent.rs`)

**Updated:**
- Removed `.with_executor()` calls when creating `SessionsSendTool` instances
- Tool now relies on framework-level async handling

## Migration Guide for Users

### Before (Native Async)

```json
// Shell tool
{"command": "long-task.sh", "async": true, "timeout_ms": 300000}

// Sessions send
{"session_id": "sess_123", "message": "Hello", "async": false, "timeout_ms": 30000}

// Agent spawn
{"task": "Do work", "mode": "sync", "timeout_secs": 60}
```

### After (Framework Async)

```json
// Shell tool
{"command": "long-task.sh", "_async": true, "_timeout": 300}

// Sessions send
{"session_id": "sess_123", "message": "Hello", "_async": false, "_timeout": 30}

// Agent spawn
{"task": "Do work", "_async": false, "_timeout": 60}
```

## Benefits

1. **Single source of truth** - All async/timeout control through framework-level parameters
2. **Consistent behavior** - Same `_async` and `_timeout` parameters work with all tools
3. **No confusion** - Users don't need to learn which tool supports which async mechanism
4. **Simpler tool code** - Tools don't need to implement their own async handling
5. **Framework handles everything** - ToolWrapper manages async execution, timeouts, retries

## Testing

All modified tools pass their tests:
- `shell::tests` - 5 tests passed
- `sessions_send::tests` - 5 tests passed
- `agent_spawn::tests` - 6 tests passed
