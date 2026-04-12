# ADR-018c: Tool Naming Cleanup

**Status**: Completed  
**Completed Date**: 2026-04-11  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Related**: ADR-017 (Extensions 2.0), ADR-018b (Unified Tool Registry)

## Context

The `Tool` trait previously had **two description methods**:
- `description()`: Human-readable description
- `llm_description()`: LLM-optimized description

This created confusion and inconsistency - different code paths used different methods, causing tools to have different descriptions depending on how they were accessed.

## Current State (After Cleanup)

The `Tool` trait now has a **single `description()` method** that returns the LLM-optimized description. This provides:
- Consistency across all tool usage paths
- Single source of truth for tool descriptions
- Simpler implementation for new tools

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

**✅ IMPLEMENTED: Standardize on `description()` as the single source of truth for tool descriptions.**

The "human-readable" vs "LLM-optimized" distinction is artificial - LLMs are the primary consumers of tool descriptions in this system.

The `Tool` trait now defines:
```rust
fn description(&self) -> String;
```

All implementations return LLM-optimized descriptions with usage guidance.

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

All items completed - the `Tool` trait now has a single `description()` method:

Tool implementations using single `description()`:
- [x] `ShellTool`
- [x] `ReadFileTool`
- [x] `WriteFileTool`
- [x] `GlobTool`
- [x] `GrepTool`
- [x] `StrReplaceFileTool`
- [x] `CronTool`
- [x] `SessionsListTool`
- [x] `SessionsHistoryTool`
- [x] `SessionStatusTool`
- [x] `McpToolProxy` (via `McpTool` struct)
- [x] `InjectableMcpToolProxy`
- [x] `UniversalToolAdapter`
- [x] `ExtensionAsyncTool`
- [x] `AgentSpawnTool`
- [x] `AgentManagementTool`
- [x] `ToolWithContext` wrapper

Call sites using `description()`:
- [x] `src/engine/loop_v4.rs` - `build_tool_definitions()`
- [x] `src/prompt/builder.rs` - `build_tools_section()`
- [x] `src/extensions/adapters/builtin_tool_adapter.rs` - Registration handlers
- [x] `src/tools/wrapper.rs` - ToolWrapper delegation

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

**ADR-018b (Unified Tool Registry)** - ✅ Completed. Registration uses the unified `description()` method.

## Status

**Not Started** - This is a cleanup/refactoring ADR that should be done after ADR-018b is completed.

## Acceptance Criteria

- [x] `Tool` trait has only `description()` method
- [x] No `llm_description()` method in Tool trait (only in UniversalToolManifest as helper)
- [x] All tools implement single `description()` method
- [x] All call sites use `description()`
- [x] All 988 tests pass

## Implementation Details

The cleanup has been completed. The `Tool` trait defines:

```rust
/// Get LLM-optimized description with usage guidance.
///
/// This should include "Use when:" and "Don't use when:" guidance
/// to help the LLM select the right tool.
fn description(&self) -> String;
```

All tool implementations return LLM-optimized descriptions. The `UniversalToolManifest` struct maintains both `description` and `llm_description` fields for backward compatibility with existing manifest files, but its `Tool` trait implementation delegates to `llm_description()`:

```rust
// src/tools/universal/adapter.rs
impl Tool for UniversalToolAdapter {
    fn description(&self) -> String {
        self.manifest.llm_description()  // Returns llm_description field or falls back to description
    }
}
```

## References

- Tool trait: `src/tools/traits.rs`
- Loop v4 definitions: `src/engine/loop_v4.rs:858-870`
- Builtin adapter: `src/extensions/adapters/builtin_tool_adapter.rs:103-114`
- SystemPromptBuilder: `src/prompt/builder.rs:183-214`
