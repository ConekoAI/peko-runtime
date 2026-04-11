# Shell Tool Async/Timeout Conflict Analysis

## Executive Summary

The `shell` tool has **native** `async` and `timeout_ms` parameters that overlap conceptually with the system's **execution control** parameters (`_async`, `_timeout`). While there's no direct naming conflict (different parameter names), the dual implementations create:

1. **Conceptual confusion** - Two ways to do async execution
2. **Inconsistent behavior** - Tool-level async vs framework-level async
3. **Potential interference** - Double async wrapping when both are used

## Parameter Comparison

### Shell Tool Native Parameters
```rust
pub struct ShellArgs {
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_ms: u64,       // Native: milliseconds, max 300000
    pub r#async: Option<bool>, // Native: tool-level async
    pub stdin: Option<String>,
}
```

### System Execution Control Parameters (ToolWrapper)
```rust
pub struct ReservedParams {
    #[serde(rename = "_async")]
    pub async_mode: bool,      // Framework-level async
    #[serde(rename = "_timeout")]
    pub timeout_secs: Option<u64>,  // Framework-level: seconds
    // ... _callback, _progress, _priority, _retry
}
```

## Execution Paths Analysis

### Path 1: Extension Framework (No ToolWrapper)
```
LLM Call -> ExtensionCore -> BuiltinToolAdapter -> ShellTool::execute()
                                        ↓
                              Receives raw params
                              (async, timeout_ms work natively)
```

**Status**: Shell tool's native `async` and `timeout_ms` work as designed.

### Path 2: Tool Factory with AsyncToolExecutor (No ToolWrapper)
```
LLM Call -> ToolFactory -> ShellTool (wrapped in Arc)
                              ↓
                    execute() receives raw params
                    (async, timeout_ms work natively)
```

**Status**: Shell tool's native parameters work.

### Path 3: Agentic Loop with ToolWrapper (Hypothetical)
```
LLM Call -> ToolWrapper::execute() -> ReservedParams::extract()
                                         ↓
                              Removes _async, _timeout
                              Then calls ShellTool::execute()
                                         ↓
                              Shell sees stripped params
```

**Status**: If `_async` is used instead of `async`, shell won't see it.

## Key Findings

### Finding 1: No Direct Naming Collision
- Shell tool uses `async` and `timeout_ms`
- System uses `_async` and `_timeout`
- Different names = no collision in parameter extraction

### Finding 2: Conceptual Overlap
Both systems provide:
| Feature | Shell Native | System Framework |
|---------|-------------|------------------|
| Async execution | `async: true` | `_async: true` |
| Timeout | `timeout_ms: 60000` | `_timeout: 60` |
| Returns | Receipt object | Task ID via receipt |

### Finding 3: Double Async Risk
If both parameters are used simultaneously:
```json
{
    "command": "long-task.sh",
    "async": true,        // Shell's native async
    "timeout_ms": 300000,
    "_async": true,       // Framework async
    "_timeout": 300
}
```

**What happens:**
1. `ToolWrapper` extracts `_async: true`, `_timeout: 300`
2. `ToolWrapper` calls `AsyncToolExecutor::execute_async()`
3. Inside async execution, `ShellTool::execute()` receives:
   ```json
   {"command": "long-task.sh", "async": true, "timeout_ms": 300000}
   ```
4. Shell tool sees `async: true`, calls its own `execute_async()`
5. **Result**: Double async wrapping - potential for confusion

### Finding 4: Documentation Inconsistency
Current LLM description for shell tool:
```rust
// From shell.rs llm_description()
r#"{"command": "./long-build-script.sh", "async": true, "timeout_ms": 300000}"#
```

But system prompt also includes:
```rust
// From wrapper.rs get_reserved_params_prompt_section()
"_async": true,
"_timeout": 300
```

**LLM sees both and can get confused!**

## Evidence from User Testing

```bash
# User asked about shell tool params
pekobot send test "what are the params for shell tool?"
# Response showed NATIVE params: async, timeout_ms

pekobot send test "are there extra params"
# Response showed SYSTEM params: _async, _timeout, etc.
```

**The LLM correctly distinguishes them but users (and potentially LLMs) may confuse which to use.**

## Recommended Solutions

### Option 1: Deprecate Shell's Native Async/Timeout (Recommended)

Remove `async` and `timeout_ms` from `ShellArgs`, rely entirely on framework-level controls.

**Pros:**
- Single source of truth for async/timeout
- Consistent across all tools
- No confusion

**Cons:**
- Breaking change for existing code using shell tool's native async
- Shell tool becomes "dumber" - relies on wrapper

**Implementation:**
```rust
// Remove from ShellArgs:
// - timeout_ms
// - r#async

// ShellTool::execute() always executes synchronously
// Framework's ToolWrapper handles all async/timeout
```

### Option 2: Rename Shell's Parameters (Breaking Change)

Change shell's parameters to avoid any confusion:
```rust
pub struct ShellArgs {
    pub command: String,
    pub cwd: Option<String>,
    pub shell_timeout_ms: u64,  // Renamed from timeout_ms
    pub run_in_background: bool, // Renamed from async
    pub stdin: Option<String>,
}
```

**Pros:**
- Clear distinction between tool and framework
- Shell can still have custom async behavior

**Cons:**
- Breaking change
- Still has two async systems

### Option 3: Keep Both but Document Clearly

Keep current implementation, improve documentation to clarify:

- `async`/`timeout_ms`: Tool-level, returns shell-specific receipt
- `_async`/`_timeout`: Framework-level, returns task ID, works with all tools

**Pros:**
- No code changes
- Backward compatible

**Cons:**
- Ongoing confusion risk
- Double async still possible

### Option 4: ToolWrapper Intercepts Shell Tool

Make shell tool go through ToolWrapper even in extension framework:

```rust
// In BuiltinToolAdapter::register_tool()
let wrapped_tool = ToolWrapper::new(tool, config);
// Register wrapped_tool instead
```

**Pros:**
- Consistent behavior
- Framework controls all async

**Cons:**
- Significant architectural change
- May break shell tool's special async behavior

## Immediate Actions (Short Term)

1. **Add conflict detection**: ToolWrapper already has `check_param_conflicts()` that warns about `_async`, `_timeout` - extend this to also warn about `async`, `timeout_ms` in tool schemas.

2. **Update shell tool documentation**: Clarify the difference between:
   - Shell's `async`/`timeout_ms` for command-level control
   - System's `_async`/`_timeout` for framework-level control

3. **Add telemetry**: Track when both systems are used together to measure confusion.

## Recommended Long-Term Solution

**Option 1 (Deprecate native async)**: Eventually migrate shell tool to use only framework-level async/timeout.

**Migration path:**
1. Mark `async` and `timeout_ms` as deprecated in shell tool schema
2. Add warnings when they're used
3. In next major version, remove them
4. Shell tool becomes purely synchronous; framework handles all async

## Other Affected Tools

The same pattern exists in other tools that were designed before the unified async framework:

### 1. Sessions Send Tool (`src/tools/sessions_send.rs`)
```rust
pub struct SessionsSendArgs {
    pub session_id: String,
    pub message: String,
    #[serde(default = "default_async")]
    pub r#async: bool,           // Native async
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,         // Native timeout (milliseconds)
}
```
**Impact**: High - Same pattern as shell tool

### 2. Agent Spawn Tool (`src/tools/agent_spawn.rs`)
```rust
pub enum SpawnMode {
    Sync { timeout_secs: u64 },  // Native sync timeout
    Async,
}
```
**Impact**: Medium - Uses structured mode instead of boolean

### Summary of Affected Tools

| Tool | Native Async Param | Native Timeout Param | Conflict Level |
|------|-------------------|---------------------|----------------|
| `shell` | `async` (bool) | `timeout_ms` | **High** |
| `sessions_send` | `async` (bool) | `timeout_ms` | **High** |
| `agent_spawn` | `mode: "async"` | `timeout_secs` (in sync mode) | **Medium** |

## Root Cause: LLM Sees Both Parameter Sets

The prompt builder (`src/prompt/builder.rs` lines 206-208) **always adds** the reserved parameters section to the system prompt:

```rust
// Add reserved parameters documentation
lines.push(String::new());
lines.push(crate::tools::get_reserved_params_prompt_section());
```

This means the LLM sees:

1. **Tool-specific description** (from `shell.llm_description()`):
   ```
   Async execution:
   {"command": "./long-build-script.sh", "async": true, "timeout_ms": 300000}
   ```

2. **Reserved params section** (added by prompt builder):
   ```
   ## Execution Control Parameters (All Tools)
   | `_async` | boolean | `false` | Execute asynchronously... |
   | `_timeout` | integer | 120 (sync)<br>300 (async) | Maximum execution time... |
   ```

**Result**: The LLM has both documented and can choose either!

## Architectural Recommendation

The root cause is that these tools were designed with their own async/timeout before `ToolWrapper` existed. The long-term solution is to:

1. **Phase 1**: Deprecate native async/timeout in shell and sessions_send
2. **Phase 2**: Migrate to framework-level async exclusively
3. **Phase 3**: Remove native async/timeout parameters

This aligns with the ADR-017 (Unified Extension Architecture) principle that tool behavior should be controlled consistently at the framework level.

## Code References

- Shell tool: `src/tools/shell.rs` (lines 49-66 for ShellArgs, 451-471 for execute)
- Sessions Send: `src/tools/sessions_send.rs` (lines 47-60 for SessionsSendArgs)
- Agent Spawn: `src/tools/agent_spawn.rs` (lines 22-50 for SpawnMode)
- ToolWrapper: `src/tools/wrapper.rs` (lines 45-71 for ReservedParams, 98-141 for extract)
- Builtin adapter: `src/extensions/adapters/builtin_tool_adapter.rs`
