# ToolWrapper Design: Architectural Consistency Analysis

## Executive Summary

**Verdict: ✅ CONSISTENT but has ALTERNATIVE that may be more elegant**

The ToolWrapper design is architecturally sound and doesn't violate any principles of the Extension Architecture (ADR-017). However, there's an alternative approach using hooks that may align more closely with the existing extension philosophy.

## Extension Architecture Review

### Core Principles (from ADR-017)

1. **Hook-Based Extensibility**
   - Extension points are defined as `HookPoint` enums
   - Handlers implement `HookHandler` trait
   - Execution flows through `ExtensionCore::invoke_hook()`

2. **Adapter Pattern**
   - `ExtensionTypeAdapter` maps external formats to hook points
   - Examples: `McpAdapter`, `UniversalToolAdapter`, `SkillAdapter`

3. **Layer Separation**
   ```
   ┌─────────────────────────────────────┐
   │  Layer 3: Extension Type Adapters   │  ← MCP, Universal, Skills
   │  (McpAdapter, UniversalToolAdapter) │
   ├─────────────────────────────────────┤
   │  Layer 2: Extension Core            │  ← Hook registration/invocation
   │  (ExtensionCore, HookPoint)         │
   ├─────────────────────────────────────┤
   │  Layer 1: Tool Abstraction          │  ← Tool trait, ToolExecutor
   │  (Tool, ToolExecutor, ToolContext)  │
   └─────────────────────────────────────┘
   ```

### Current Async Extension Flow

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────────┐
│   LLM Call   │────▶│ AsyncAgenticLoop │────▶│   ToolExecutor   │
└──────────────┘     └──────────────────┘     └──────────────────┘
                              │                          │
                              ▼                          ▼
                    ┌──────────────────┐     ┌──────────────────┐
                    │  should_use_async│     │ ExtensionCore   │
                    │  detect_capabilities│   │ (ToolExecute hook)│
                    └──────────────────┘     └──────────────────┘
                                                      │
                              ┌───────────────────────┼──────────┐
                              ▼                       ▼          ▼
                    ┌──────────────────┐  ┌──────────────────┐  ┌──────────┐
                    │ Native Tool      │  │ MCP Tool         │  │ Universal│
                    │ (Tool trait)     │  │ (via hooks)      │  │ (hooks)  │
                    └──────────────────┘  └──────────────────┘  └──────────┘
```

## Option A: ToolWrapper (Proposed Design)

### Where It Fits

```
┌──────────────────────────────────────────────────────────────────┐
│  AsyncAgenticLoop                                                │
│  └─▶ decides sync vs async based on config + capabilities       │
├──────────────────────────────────────────────────────────────────┤
│  ToolExecutor                                                    │
│  └─▶ executes Tool trait implementations                        │
├──────────────────────────────────────────────────────────────────┤
│  ╔══════════════════════════════════════════════════════════════╗│
│  ║  TOOL WRAPPER (NEW)                                          ║│
│  ║  ├─▶ Extracts _async, _timeout params                        ║│
│  ║  ├─▶ Strips reserved params                                  ║│
│  ║  └─▶ Routes to sync/async executor                           ║│
│  ╚══════════════════════════════════════════════════════════════╝│
├──────────────────────────────────────────────────────────────────┤
│  Tool Implementations                                            │
│  ├─▶ Native: impl Tool                                           │
│  ├─▶ MCP: via hooks → Tool adapter                               │
│  └─▶ Universal: via hooks → Tool adapter                         │
└──────────────────────────────────────────────────────────────────┘
```

### Architectural Consistency

| Aspect | Status | Notes |
|--------|--------|-------|
| **Hook Compatibility** | ✅ Compatible | Wrapper operates below hook layer |
| **Adapter Pattern** | ✅ Consistent | Wrapper is effectively an adapter |
| **Layer Separation** | ⚠️ Adds layer | New layer between Executor and Tools |
| **Backwards Compatible** | ✅ Yes | Transparent to existing code |
| **Extensible** | ✅ Yes | Can add more reserved params |

### Pros

1. **Simple Implementation**
   - Single `ToolWrapper` struct wraps any `dyn Tool`
   - No changes to Extension Core needed
   - Works uniformly across all tool types

2. **Type Safety**
   - Leverages Rust's type system
   - Compile-time guarantees

3. **Performance**
   - Minimal overhead (one dynamic dispatch)
   - No hook invocation overhead

### Cons

1. **Not Hook-Based**
   - Doesn't use the Extension Architecture's hook system
   - Could be seen as bypassing the intended extension mechanism

2. **Two Paths for Async**
   - Native async: `ToolWrapper` → `AsyncToolExecutor`
   - Hook-based async: `ToolWrapper` → `Hook` → `AsyncToolExecutor`
   - Potential confusion

3. **Parameter Extraction Timing**
   - Happens at Tool trait level
   - Extension hooks receive already-filtered params
   - Hooks can't see original `_async` flag

## Option B: Hook-Based Preprocessor (More Architectural)

### Design

Create a new hook point for parameter preprocessing:

```rust
pub enum HookPoint {
    // ... existing hooks ...
    
    /// Preprocess tool parameters before execution
    /// Allows modifying params, extracting metadata, routing decisions
    ToolPreExecute {
        tool_name: String,
    },
}

pub struct ToolPreExecuteOutput {
    /// Modified parameters (reserved params stripped)
    pub params: Value,
    /// Execution mode override
    pub execution_mode: Option<ExecutionMode>,
    /// Timeout override
    pub timeout_secs: Option<u64>,
    /// Other metadata...
}

pub enum ExecutionMode {
    Sync,
    Async,
    Deferred,
}
```

### Flow

```
┌──────────────────────────────────────────────────────────────────┐
│  AsyncAgenticLoop                                                │
│  └─▶ Calls ToolExecutor with raw params (including _async)      │
├──────────────────────────────────────────────────────────────────┤
│  ToolExecutor                                                    │
│  └─▶ BEFORE execution, invokes ToolPreExecute hook              │
├──────────────────────────────────────────────────────────────────┤
│  ╔══════════════════════════════════════════════════════════════╗│
│  ║  TOOL PREPROCESSOR HOOK (NEW)                                ║│
│  ║  ├─▶ Registered as extension                                 ║│
│  ║  ├─▶ Extracts _async, _timeout                               ║│
│  ║  ├─▶ Returns modified params + execution_mode               ║│
│  ║  └─▶ Core handles routing based on execution_mode           ║│
│  ╚══════════════════════════════════════════════════════════════╝│
├──────────────────────────────────────────────────────────────────┤
│  ExtensionCore                                                   │
│  └─▶ Routes to appropriate executor based on hook output        │
├──────────────────────────────────────────────────────────────────┤
│  Tool Execution (sync or async)                                  │
└──────────────────────────────────────────────────────────────────┘
```

### Implementation

```rust
// In ExtensionCore or AsyncAgenticLoop
pub async fn execute_tool_with_preprocessing(
    &self,
    tool_name: &str,
    params: Value,
) -> Result<Value> {
    // 1. Invoke preprocessing hook
    let pre_result = self.core.invoke_hook(
        HookPoint::ToolPreExecute { tool_name: tool_name.to_string() },
        HookInput::Json(params.clone()),
    ).await?;
    
    // 2. Parse preprocessing output
    let (filtered_params, execution_mode, timeout) = match pre_result {
        HookOutput::ToolPreprocess(output) => {
            (output.params, output.execution_mode, output.timeout_secs)
        }
        _ => (params, None, None), // No preprocessing, use defaults
    };
    
    // 3. Route to appropriate executor
    match execution_mode {
        Some(ExecutionMode::Async) => {
            self.execute_async(tool_name, filtered_params, timeout).await
        }
        Some(ExecutionMode::Sync) | None => {
            self.execute_sync(tool_name, filtered_params, timeout).await
        }
    }
}
```

### Registration

```rust
// Register the reserved parameter preprocessor
extension_core.register_hook(
    HookPoint::ToolPreExecute { tool_name: "*".to_string() },
    Arc::new(ReservedParamPreprocessor::new()),
    & ExtensionId::new("core.reserved-params"),
).await?;
```

### Pros

1. **Hook-Based**
   - Uses Extension Architecture as intended
   - Consistent with existing patterns

2. **Extensible**
   - Other extensions can register their own preprocessors
   - Chain of responsibility pattern

3. **Observable**
   - Preprocessing visible in hook registry
   - Debuggable via hook inspection

4. **Flexible**
   - Can be disabled by not registering the hook
   - Custom preprocessors can override behavior

### Cons

1. **More Complex**
   - New hook point to define
   - New HookOutput variant
   - Handler registration required

2. **Performance**
   - Additional hook invocation overhead
   - More dynamic dispatch

3. **Timing Issues**
   - When should preprocessing happen?
   - Before or after tool lookup?

## Recommendation

### Hybrid Approach: ToolWrapper + Optional Hook Integration

Combine the simplicity of ToolWrapper with architectural alignment:

```rust
/// Wraps tools and provides reserved parameter handling
/// Can work standalone OR integrate with ExtensionCore
pub struct ToolWrapper {
    inner: Arc<dyn Tool>,
    config: WrapperConfig,
    /// Optional: ExtensionCore for hook-based preprocessing
    extension_core: Option<Arc<ExtensionCore>>,
}

impl ToolWrapper {
    pub async fn execute(&self, params: Value) -> Result<Value> {
        // 1. Try hook-based preprocessing if ExtensionCore available
        let (params, execution_mode) = if let Some(core) = &self.extension_core {
            self.preprocess_with_hooks(core, params).await?
        } else {
            // 2. Fallback to local preprocessing
            self.preprocess_locally(params)
        };
        
        // 3. Execute based on mode
        match execution_mode {
            ExecutionMode::Async => self.execute_async(params).await,
            ExecutionMode::Sync => self.execute_sync(params).await,
        }
    }
}
```

### Migration Path

**Phase 1: ToolWrapper (Immediate)**
- Implement ToolWrapper as standalone component
- No Extension Core changes needed
- Immediate value delivery

**Phase 2: Hook Integration (Future)**
- Add `ToolPreExecute` hook point
- Migrate ToolWrapper to use hooks internally
- Deprecate standalone mode

## Decision Matrix

| Criteria | ToolWrapper | Hook-Based | Hybrid |
|----------|-------------|------------|--------|
| **Implementation Speed** | ⭐⭐⭐ Fast | ⭐⭐ Medium | ⭐⭐⭐ Fast |
| **Architectural Purity** | ⭐⭐ Good | ⭐⭐⭐ Excellent | ⭐⭐⭐ Excellent |
| **Performance** | ⭐⭐⭐ Best | ⭐⭐ Good | ⭐⭐ Good |
| **Extensibility** | ⭐⭐ Good | ⭐⭐⭐ Excellent | ⭐⭐⭐ Excellent |
| **Backwards Compatible** | ⭐⭐⭐ Yes | ⭐⭐⭐ Yes | ⭐⭐⭐ Yes |
| **Debuggability** | ⭐⭐ Good | ⭐⭐⭐ Excellent | ⭐⭐⭐ Excellent |

## Final Recommendation

**Go with Option A (ToolWrapper) for now, with architectural path to Option B.**

### Rationale:

1. **Pragmatism**
   - ToolWrapper delivers value immediately
   - No blocking dependencies on Extension Core changes

2. **Future-Proof**
   - Can be refactored to use hooks later
   - API remains stable for callers

3. **Separation of Concerns**
   - ToolWrapper handles parameter extraction
   - Extension Core handles execution routing
   - Clear responsibilities

4. **Proven Pattern**
   - Similar to how `SyncToAsyncAdapter` works
   - Consistent with existing codebase patterns

### Implementation Notes:

```rust
// Location: src/tools/wrapper.rs
// Pattern: Adapter wrapping Tool trait
// Integration: Used by ToolExecutor or AsyncAgenticLoop

pub struct ToolWrapper {
    inner: Arc<dyn Tool>,
    config: WrapperConfig,
}

impl Tool for ToolWrapper {
    fn parameters(&self) -> Value {
        // Return ORIGINAL tool parameters
        // Reserved params are NOT in the schema
        self.inner.parameters()
    }
    
    async fn execute(&self, params: Value) -> Result<Value> {
        // Extract reserved params here
        // Route to appropriate executor
    }
}
```

## System Prompt Integration

Regardless of approach, add to `{{tools}}` section:

```markdown
### Execution Control (All Tools)

Optional reserved parameters (prefixed with `_`) control tool execution:

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `_async` | bool | false | Execute asynchronously |
| `_timeout` | int | 120/300 | Timeout in seconds (sync/async) |
| `_progress` | bool | true | Request progress updates |

**Note:** These parameters are automatically handled by the runtime 
and are NOT passed to the tool itself. If a tool defines a parameter 
with the same name, the tool's parameter takes precedence.
```

## Conclusion

The ToolWrapper design is **architecturally consistent** and **implementable now**. While a hook-based approach would be more "pure" to the Extension Architecture philosophy, the wrapper approach is pragmatic, performant, and doesn't preclude future hook integration.

**Recommended Action:** Proceed with ToolWrapper implementation.
