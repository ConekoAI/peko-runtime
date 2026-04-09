# Tool Wrapper Design: Reserved Parameters for Agent Control

## Overview
A unified wrapper that adds reserved parameters to ALL tools (built-in, MCP, Universal) allowing agents to control execution mode without modifying tool schemas.

## Design Goals

1. **Backward Compatibility** - Existing tools work unchanged
2. **Agent Control** - Agents can opt-in to async when beneficial
3. **Clean Schemas** - Tool definitions don't show reserved params
4. **Conflict Resolution** - Tool params take precedence over reserved ones
5. **Sync Default** - Safe, predictable default behavior

## Reserved Parameters

```json
{
  "_async": false,           // Request async execution (default: false)
  "_timeout": 120,           // Timeout in seconds (sync: default 120, async: default 300)
  "_callback": "queue",      // Result delivery: "queue" | "stream" | "blocking"
  "_progress": true,         // Request progress updates (async only, default: true)
  "_priority": "normal",     // Task priority: "low" | "normal" | "high"
  "_retry": 0                // Number of retries on failure (default: 0)
}
```

**Naming Convention:** Underscore prefix `_` to avoid conflicts with user-defined params.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Agent (LLM)                                      │
│                                                                      │
│  Tool Call: {                                                        │
│    "name": "mcp:files:search",                                      │
│    "arguments": {                                                    │
│      "query": "*.rs",           ← Tool's original param              │
│      "_async": true,            ← Reserved param (agent control)     │
│      "_timeout": 300                                                 │
│    }                                                                 │
│  }                                                                   │
└─────────────────────────────────┬────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    ToolWrapper (Universal)                           │
│                                                                      │
│  1. Extract reserved params                                          │
│  2. Filter out reserved params from tool_params                      │
│  3. Determine execution mode                                         │
│  4. Route to appropriate executor                                    │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  Conflict Resolution Logic                                      │ │
│  │                                                                 │ │
│  │  if tool_schema.has_param("_async"):                           │ │
│  │      # Tool defines _async itself                               │ │
│  │      use tool_param (not reserved)                              │ │
│  │      log_warning("Tool shadows reserved param: _async")        │ │
│  │  else:                                                          │ │
│  │      use reserved_param                                         │ │
│  └────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────┬────────────────────────────────────┘
                                  │
              ┌───────────────────┴───────────────────┐
              │                                       │
              ▼                                       ▼
┌──────────────────────────────┐      ┌──────────────────────────────┐
│   Sync Tool Executor         │      │   Async Tool Executor        │
│   (default, blocking)        │      │   (non-blocking, receipt)    │
│                              │      │                              │
│  - Timeout: _timeout secs    │      │  - Returns receipt           │
│  - Blocking call             │      │  - Poll for status           │
│  - Immediate result          │      │  - Progress callbacks        │
└──────────────────────────────┘      └──────────────────────────────┘
```

## System Prompt Injection

The `{{tools}}` section in system prompt includes:

```markdown
## Available Tools

### Tool: mcp:files:search
Search for files matching a pattern.

**Parameters:**
- `query` (string, required): Search pattern (e.g., "*.rs")
- `path` (string, optional): Directory to search

---

### Reserved Parameters (All Tools)

These optional parameters control tool execution behavior. They are 
automatically stripped from the tool call before execution.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `_async` | boolean | `false` | Execute asynchronously. Returns a task ID for status checking. |
| `_timeout` | integer | 120 (sync) / 300 (async) | Maximum execution time in seconds. |
| `_callback` | string | "queue" | Result delivery: `queue`, `stream`, or `blocking`. |
| `_progress` | boolean | `true` | Request progress updates (async only). |
| `_priority` | string | "normal" | Task priority: `low`, `normal`, `high`. |
| `_retry` | integer | 0 | Number of retries on failure. |

**Example with reserved params:**
```json
{
  "name": "mcp:files:search",
  "arguments": {
    "query": "*.rs",
    "_async": true,
    "_timeout": 300,
    "_progress": true
  }
}
```

**Note:** If a tool defines a parameter with the same name as a reserved 
parameter, the tool's parameter takes precedence and a warning is logged.
```

## Implementation

### ToolWrapper Struct

```rust
/// Universal wrapper for all tools with reserved parameter support
pub struct ToolWrapper {
    inner: Arc<dyn Tool>,
    config: WrapperConfig,
}

pub struct WrapperConfig {
    /// Default timeout for sync execution
    pub default_sync_timeout_secs: u64,
    /// Default timeout for async execution
    pub default_async_timeout_secs: u64,
    /// Whether to allow async by default
    pub allow_async: bool,
    /// Async executor reference
    pub async_executor: Option<Arc<AsyncToolExecutor>>,
}

impl ToolWrapper {
    /// Wrap any tool
    pub fn new(inner: Arc<dyn Tool>, config: WrapperConfig) -> Self {
        Self { inner, config }
    }
    
    /// Extract reserved params, return (filtered_params, reserved_params)
    fn extract_reserved_params(&self, params: Value) -> (Value, ReservedParams) {
        let mut filtered = params.clone();
        let mut reserved = ReservedParams::default();
        
        if let Some(obj) = filtered.as_object_mut() {
            // Extract each reserved param
            if let Some(v) = obj.remove("_async") {
                reserved.async_mode = v.as_bool().unwrap_or(false);
            }
            if let Some(v) = obj.remove("_timeout") {
                reserved.timeout_secs = v.as_u64();
            }
            if let Some(v) = obj.remove("_callback") {
                reserved.callback = v.as_str().map(|s| s.to_string());
            }
            if let Some(v) = obj.remove("_progress") {
                reserved.progress = v.as_bool().unwrap_or(true);
            }
            if let Some(v) = obj.remove("_priority") {
                reserved.priority = v.as_str().map(|s| s.to_string());
            }
            if let Some(v) = obj.remove("_retry") {
                reserved.retry_count = v.as_u64().unwrap_or(0) as u32;
            }
        }
        
        (filtered, reserved)
    }
    
    /// Check if tool shadows reserved params
    fn check_param_conflicts(&self, reserved: &ReservedParams) -> Vec<String> {
        let schema = self.inner.parameters();
        let mut conflicts = Vec::new();
        
        // Check if tool schema has any reserved param names
        if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
            if props.contains_key("_async") && reserved.async_mode {
                conflicts.push("_async".to_string());
            }
            if props.contains_key("_timeout") && reserved.timeout_secs.is_some() {
                conflicts.push("_timeout".to_string());
            }
            // ... etc
        }
        
        conflicts
    }
}

#[async_trait]
impl Tool for ToolWrapper {
    fn name(&self) -> &str {
        self.inner.name()
    }
    
    fn description(&self) -> &str {
        self.inner.description()
    }
    
    fn parameters(&self) -> Value {
        // Return ORIGINAL tool parameters (no reserved params)
        self.inner.parameters()
    }
    
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let (filtered_params, reserved) = self.extract_reserved_params(params);
        
        // Check for conflicts
        let conflicts = self.check_param_conflicts(&reserved);
        if !conflicts.is_empty() {
            tracing::warn!(
                tool = %self.name(),
                conflicts = ?conflicts,
                "Tool shadows reserved parameters, using tool's definition"
            );
        }
        
        // Route to appropriate executor
        if reserved.async_mode && self.config.allow_async {
            self.execute_async(filtered_params, reserved).await
        } else {
            self.execute_sync(filtered_params, reserved).await
        }
    }
}
```

### ReservedParams Struct

```rust
#[derive(Debug, Clone, Default)]
pub struct ReservedParams {
    pub async_mode: bool,
    pub timeout_secs: Option<u64>,
    pub callback: Option<String>,
    pub progress: bool,
    pub priority: Option<String>,
    pub retry_count: u32,
}

impl ReservedParams {
    /// Get effective timeout (use reserved or default)
    pub fn effective_timeout(&self, is_async: bool, defaults: &WrapperConfig) -> u64 {
        self.timeout_secs.unwrap_or(if is_async {
            defaults.default_async_timeout_secs
        } else {
            defaults.default_sync_timeout_secs
        })
    }
}
```

## Conflict Resolution

### Case 1: Tool Uses Reserved Name Accidentally
```rust
// Tool schema defines "_async" as its own parameter
{
  "name": "custom_tool",
  "parameters": {
    "_async": { "type": "boolean" }  // Oh no!
  }
}

// Agent calls with reserved intent
{
  "_async": true  // Ambiguous!
}

// Resolution: Tool param takes precedence
// Result: "_async" is passed to tool, not used for wrapper
// Warning logged: "Tool 'custom_tool' shadows reserved param '_async'"
```

### Case 2: Agent Uses Reserved Params Correctly
```rust
// Tool: mcp:files:search
// No "_async" in schema ✓

// Agent call
{
  "query": "*.rs",
  "_async": true
}

// Resolution: Reserved params extracted by wrapper
// "_async": Used by wrapper for async execution
// "query": Passed to mcp:files:search
```

## Benefits

1. **Zero Tool Changes**
   - No modifications to existing tools
   - Works with MCP, Universal, and built-in tools

2. **Agent Empowerment**
   - Agents can choose async for long operations
   - Can tune timeouts per call
   - Can request progress updates

3. **Backward Compatible**
   - Existing agents work unchanged (sync default)
   - Tool schemas unchanged
   - Reserved params are opt-in

4. **Conflict Safe**
   - Tool params take precedence
   - Clear warning logs
   - No silent failures

5. **Extensible**
   - Easy to add new reserved params
   - Versioned reserved param schema

## Edge Cases

### Edge Case 1: Nested Reserved Params
```json
{
  "config": {
    "_async": true  // Inside nested object
  }
}
```
**Resolution:** Only top-level reserved params are extracted. Nested ones are passed to tool.

### Edge Case 2: Array with Reserved Names
```json
{
  "items": ["_async", "_timeout"]
}
```
**Resolution:** Only object keys are checked, array values are passed through.

### Edge Case 3: Tool Named "_async"
```rust
// Tool literally named "_async"
```
**Resolution:** Tool name is separate from params, no conflict.

## Future Extensions

### Batch Execution
```json
{
  "_batch": true,           // Execute multiple calls as batch
  "_batch_id": "batch_123"  // Batch identifier
}
```

### Conditional Execution
```json
{
  "_if": "{{previous_result.success}}",  // Conditional execution
  "_unless": "{{previous_result.error}}"
}
```

### Resource Limits
```json
{
  "_max_memory_mb": 512,
  "_max_cpu_percent": 50
}
```

## Migration Path

### Phase 1: Wrapper Implementation
- Implement `ToolWrapper`
- Add to tool loading pipeline
- Log reserved param usage

### Phase 2: System Prompt Update
- Add reserved params section to `{{tools}}`
- Include examples

### Phase 3: Agent Training
- Agents learn to use `_async` for long operations
- Monitor async adoption

### Phase 4: Optimization
- Auto-suggest async based on tool history
- Smart defaults per tool type

## Questions to Consider

1. **Should we namespace reserved params?**
   - `_peko_async` instead of `_async`?
   - More unique but verbose

2. **Should reserved params be in tool schema?**
   - Pro: Agents see them in function calling
   - Con: Pollutes tool definitions
   - **Current decision:** No, keep schemas clean

3. **How to handle very old agents?**
   - They won't use reserved params
   - Everything stays sync (safe default)
   - No breaking change

4. **Should we allow tool to opt-out of async?**
   - Some tools may not support async well
   - Add `_supports_async: false` to tool metadata?
