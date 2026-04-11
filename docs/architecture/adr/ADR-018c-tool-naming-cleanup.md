# ADR-018c: Tool Naming Cleanup

**Status**: Proposed  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Related**: ADR-017 (Extensions 2.0), ADR-018b (Unified Tool Registry)

## Context

The `Tool` trait currently has **two description methods**:
- `description()`: Human-readable description
- `llm_description()`: LLM-optimized description

This creates confusion and inconsistency - different code paths use different methods, causing tools to have different descriptions depending on how they're accessed.

## Problem Statement

### Current Inconsistency

```rust
// src/tools/traits.rs
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> String;        // Human-readable
    fn llm_description(&self) -> String;    // LLM-optimized
    fn parameters(&self) -> ParameterSchema;
}
```

### Usage Analysis

| Location | Method Used | Purpose |
|----------|-------------|---------|
| `loop_v4::build_tool_definitions()` | `llm_description()` | LLM API calls |
| `BuiltinRegisterHandler` | `description()` | Extension registration |
| `BuiltinPromptHandler` | `llm_description()` | System prompt |
| `SystemPromptBuilder` | `llm_description()` | System prompt |

**Problem**: `BuiltinRegisterHandler` uses `description()` while everyone else uses `llm_description()`. This means:
- Tools appear in ExtensionCore registry with short descriptions
- Same tools appear in prompts with detailed descriptions
- Inconsistent behavior, hard to debug

## Decision

**Remove `description()` method. Standardize on `llm_description()` as the single source of truth for tool descriptions.**

The "human-readable" vs "LLM-optimized" distinction is artificial - LLMs are the primary consumers of tool descriptions in this system.

## Implementation

### Phase 1: Update Tool Trait

```rust
// src/tools/traits.rs

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    
    /// Single description method - LLM-optimized
    /// This is the ONLY description method for tools
    fn description(&self) -> String;
    
    fn parameters(&self) -> ParameterSchema;
}

// Remove: llm_description() method
```

### Phase 2: Update All Tool Implementations

Rename `llm_description()` to `description()` in all tool implementations:

```rust
// src/tools/shell.rs
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    
    // BEFORE: fn llm_description(&self) -> String
    fn description(&self) -> String {
        format!(
            "Execute shell commands safely.\n\n{}\n\n## Examples\n{}",
            self.safety_instructions(),
            self.examples()
        )
    }
    
    fn parameters(&self) -> ParameterSchema {
        // ...
    }
}
```

### Phase 3: Update All Call Sites

Replace `llm_description()` calls with `description()`:

```rust
// src/engine/loop_v4.rs
fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
    self.tools
        .iter()
        .map(|tool| ToolDefinition {
            name: tool.name().to_string(),
            description: tool.description(), // Changed from llm_description()
            parameters: tool.parameters(),
        })
        .collect()
}
```

```rust
// src/extensions/adapters/builtin_tool_adapter.rs
impl BuiltinRegisterHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        let tool_def = ToolDefinition {
            name: self.tool.name().to_string(),
            description: self.tool.description(), // Changed from description()
            parameters: self.tool.parameters(),
        };
        // ...
    }
}
```

### Phase 4: Update ToolWrapper

```rust
// src/tools/wrapper.rs
impl Tool for ToolWrapper {
    fn name(&self) -> &str {
        self.inner.name()
    }
    
    fn description(&self) -> String {
        // Pass through to inner tool
        self.inner.description() // Changed from llm_description()
    }
    
    fn parameters(&self) -> ParameterSchema {
        self.inner.parameters()
    }
}
```

## Migration Checklist

Tool implementations to update:
- [ ] `ShellTool`
- [ ] `ReadFileTool`
- [ ] `WriteFileTool`
- [ ] `GlobTool`
- [ ] `GrepTool`
- [ ] `StrReplaceFileTool`
- [ ] `CronTool`
- [ ] `SessionsListTool`
- [ ] `SessionsHistoryTool`
- [ ] `SessionStatusTool`
- [ ] `McpToolProxy`
- [ ] `UniversalTool` (GeneralAdapter)
- [ ] `ExtensionAsyncTool`

Call sites to update:
- [ ] `src/engine/loop_v4.rs`
- [ ] `src/prompt/builder.rs`
- [ ] `src/extensions/adapters/builtin_tool_adapter.rs`
- [ ] `src/extensions/adapters/universal_tool_adapter.rs`
- [ ] `src/tools/wrapper.rs`
- [ ] Any test files

## Benefits

| Benefit | Description |
|---------|-------------|
| **Clarity** | Single method name, no confusion |
| **Consistency** | All code paths use same description |
| **Simplicity** | One less method to implement per tool |
| **DRY** | No duplicate description logic |

## Drawbacks

| Drawback | Mitigation |
|----------|------------|
| **Breaking change** | Internal refactor, no public API changes |
| **Refactoring effort** | Mechanical find/replace across codebase |

## Dependencies

**ADR-018b (Unified Tool Registry)** should be completed first to ensure registration uses the correct method.

## Acceptance Criteria

- [ ] `Tool` trait has only `description()` method
- [ ] No `llm_description()` method exists in codebase
- [ ] All tools implement single `description()` method
- [ ] All call sites use `description()`
- [ ] All 980+ tests pass

## References

- Tool trait: `src/tools/traits.rs`
- Loop v4 definitions: `src/engine/loop_v4.rs:858-870`
- Builtin adapter: `src/extensions/adapters/builtin_tool_adapter.rs:103-114`
- SystemPromptBuilder: `src/prompt/builder.rs:183-214`
