# ADR-019: Dynamic Tool and System Prompt Updates

**Status**: Completed  
**Completed Date**: 2026-04-11  
**Updated**: 2026-04-11  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Depends On**: ADR-018a (Tool Execution Unification), ADR-018b (Unified Tool Registry)

## Context

Currently, the agent's tool set and system prompt are determined at session start and remain static throughout the session. This creates several UX issues:

1. **Tool changes require session restart**: If a user enables/disables tools mid-session, the changes don't take effect until a new session is started
2. **System prompt is immutable**: Changes to SYSTEM.md or tool descriptions require session restart
3. **Security gap**: Disabled tools may still be callable by the LLM if they were enabled at session start

## Problem Statement

### Current Architecture (Static)

```
Session Start
     │
     ▼
┌─────────────────────────────────────────┐
│  AgenticLoopV4::new()                   │
│  ────────────────────                   │
│  • system_prompt = build_prompt(tools)  │  ◄── Built ONCE
│  • tools stored in self.tools           │
│  • tool_defs built once                 │
└─────────────────────────────────────────┘
     │
     ▼
┌─────────────────────────────────────────┐
│  Main Loop (each iteration)             │
│  ─────────────────────────              │
│  • Same system prompt used              │
│  • Same tool_defs passed to provider    │
│  • Tool lookup from static list         │
└─────────────────────────────────────────┘
```

### Edge Cases

| Scenario | Current Behavior | Desired Behavior |
|----------|------------------|------------------|
| Tool disabled → enabled | Tool unknown to LLM | LLM learns about tool immediately |
| Tool enabled → disabled | LLM can still call tool | Clear error: "Tool disabled" |
| SYSTEM.md modified | No effect | New content in next iteration |
| Skills updated | No effect | New skills injected |

## Proposed Solution

### Prerequisites

**THIS ADR DEPENDS ON ADR-018a AND ADR-018b BEING COMPLETED FIRST.**

Once complete:
- ALL tools route through ExtensionCore (ADR-018a)
- Single unified registry for tool metadata (ADR-018b)
- Consistent descriptions via `Tool::description()` (ADR-018c)

This enables:
- Single permission check point
- Unified tool registration lifecycle
- Consistent dynamic updates

### Architecture (Dynamic)

```
Session Start
     │
     ▼
┌─────────────────────────────────────────┐
│  AgenticLoopV4::new()                   │
│  (Minimal initialization)               │
└─────────────────────────────────────────┘
     │
     ▼
┌─────────────────────────────────────────┐
│  Per-Iteration (every message)          │
│  ─────────────────────────────          │
│  • Rebuild system prompt from fresh     │
│  • Rebuild tool definitions from config │
│  • Check permissions at execution       │
└─────────────────────────────────────────┘
```

### Implementation Components

#### 1. Dynamic System Prompt Rebuild

**Location**: `src/engine/loop_v4.rs` - Inside loop iteration

```rust
// Current (line ~166-189): Static system prompt
let mut messages = vec![ChatMessage {
    role: MessageRole::System,
    content: vec![ContentBlock::Text {
        text: self.system_prompt.clone(), // Static
    }],
}];

// Proposed: Dynamic rebuild
let fresh_system = self.build_system_prompt_fresh().await;
messages[0] = ChatMessage::system(fresh_system);
```

**Fresh Build Method**:
```rust
impl AgenticLoopV4 {
    async fn build_system_prompt_fresh(&self) -> String {
        let current_tools = self.get_current_tools().await;
        
        SystemPromptBuilder::new(&self.agent.name())
            .with_tools(current_tools)
            .with_extension_core(Arc::clone(&self.extension_core))
            .with_workspace(&self.workspace)
            // ... other dynamic params
            .build()
    }
    
    async fn get_current_tools(&self) -> Vec<Arc<dyn Tool>> {
        // Filter tools based on current ToolConfig whitelist
        let config = load_current_tool_config().await;
        self.tools.iter()
            .filter(|t| config.is_tool_enabled(t.name()))
            .cloned()
            .collect()
    }
}
```

**Benefits**:
- SYSTEM.md changes reflected immediately
- Tool list updates in prompt
- Skill/extension changes picked up
- Future: Dynamic persona switching

**Performance**: ~1-5ms per iteration (acceptable)

#### 2. Dynamic Tool Definitions

**Location**: `src/engine/loop_v4.rs` - Move inside loop

```rust
// Current (line ~381): Built once outside loop
let tool_defs = self.build_tool_definitions();

// Proposed: Rebuild each iteration
loop {
    let tool_defs = self.build_tool_definitions_fresh().await;
    let response = self.provider
        .chat_with_tools(&messages, &tool_defs, &options)
        .await?;
    // ...
}
```

**Provider Compatibility**:
- ✅ OpenAI/Anthropic: Accept dynamic tool list per call
- ✅ Legacy providers: Tool list embedded in prompt (rebuilt anyway)

#### 3. Tool Permission Check at ExtensionCore Layer

**Location**: `src/extensions/core.rs` - Hook dispatch

After ADR-018, ALL tools go through ExtensionCore, enabling centralized permission checking:

```rust
impl ExtensionCore {
    pub async fn invoke_hook(&self, hook_point: HookPoint) -> HookResult {
        // ADD: Permission check for ALL tool executions
        if let HookPoint::ToolExecute { tool_name, context, .. } = &hook_point {
            if !self.is_tool_enabled(tool_name, context).await {
                return HookResult::Error(format!(
                    "Tool '{}' is currently disabled. \
                     Enable it in agent config to use it.",
                    tool_name
                ));
            }
        }
        
        // ... existing hook dispatch
    }
}
```

**Why ExtensionCore layer (not ToolExecutor)**:
- After ADR-018a, ALL tools route through ExtensionCore
- Single point of enforcement for all tool types
- Hook-based permissions can be extended (rate limiting, audit, etc.)

## Implementation Phases

### Phase 1: Tool Permission Check (Safety Net)
**Priority**: High  
**Risk**: Low  
**Effort**: ~2 hours  
**Depends On**: ADR-018

- Add `is_tool_enabled()` check in `ExtensionCore::invoke_hook()` for `HookPoint::ToolExecute`
- Load current `ToolConfig` at execution time
- Return clear error message if disabled

**Acceptance Criteria**:
- [ ] Disabled tools return clear error when called
- [ ] Test: Enable tool mid-session, LLM can call it
- [ ] Test: Disable tool mid-session, LLM gets error when calling

### Phase 2: Dynamic Tool Definitions
**Priority**: Medium  
**Risk**: Low  
**Effort**: ~4 hours  
**Depends On**: Phase 1

- Move `build_tool_definitions()` inside loop iteration
- Ensure fresh tool list passed to provider each call
- Update streaming version too (`stream_with_tools`)

**Acceptance Criteria**:
- [ ] Tool schema updates without session restart
- [ ] Provider receives updated tool list each iteration
- [ ] No performance regression (>10ms added latency)

### Phase 3: Dynamic System Prompt
**Priority**: Medium  
**Risk**: Medium  
**Effort**: ~8 hours  
**Depends On**: Phase 2

- Create `build_system_prompt_fresh()` async method
- Rebuild prompt each iteration from current state
- Handle compaction summary consistency

**Acceptance Criteria**:
- [ ] SYSTEM.md changes reflected in next iteration
- [ ] Tool list in prompt matches current config
- [ ] Compaction works correctly with dynamic prompts
- [ ] No performance regression (>10ms added latency)

### Phase 4: Optimization (Future)
**Priority**: Low  
**Risk**: Low  
**Effort**: ~4 hours  
**Depends On**: Phase 3

- Cache prompt sections that rarely change
- Invalidate cache on file/config changes
- Only rebuild changed sections

## Edge Cases and Considerations

### 1. Compaction Consistency

**Problem**: Compaction summaries include system prompt. If it changes mid-session, summaries become inconsistent.

**Solutions**:
- Option A: Include "system prompt hash" in compaction metadata, rebuild if changed
- Option B: Freeze system prompt in summaries (use original)
- Option C: Track prompt versions, compact per-version

**Recommendation**: Option A (rebuild summary if prompt changed)

### 2. Message History with Changing Tools

**Problem**: LLM may reference a tool that was available in earlier messages but is now disabled.

**Solution**: 
- Keep full message history (including tool calls)
- Execution layer will reject disabled tools with clear error
- LLM can adapt based on error message

### 3. Performance

**Current Loop Latency**: ~100-500ms (depending on provider)  
**Target Added Latency**: <10ms (<2% overhead)

**Optimization Strategies**:
- Cache tool definitions if config unchanged
- Use inotify/file watcher for SYSTEM.md changes
- Parallel rebuild (prompt + tool defs simultaneously)

### 4. Provider-Specific Behavior

| Provider | Dynamic Tools | Dynamic Prompt | Notes |
|----------|---------------|----------------|-------|
| OpenAI | ✅ Yes | ✅ Yes | Full support |
| Anthropic | ✅ Yes | ✅ Yes | Full support |
| Ollama | ✅ Yes | ✅ Yes | Full support |
| Legacy | ✅ Yes (in prompt) | ✅ Yes | Tool list in text |

## Alternatives Considered

### Alternative 1: Session Restart Required (Current)
- **Pros**: Simple, no complexity
- **Cons**: Poor UX, security gap
- **Verdict**: Rejected - doesn't meet user needs

### Alternative 2: Session Recovery/Snapshot
- **Pros**: Clean slate, no inconsistency issues
- **Cons**: Disruptive, loses context
- **Verdict**: Rejected - too disruptive

### Alternative 3: Event-Driven Updates
- **Pros**: Efficient, only update when needed
- **Cons**: Complex, race conditions
- **Verdict**: Future optimization (Phase 4)

## Decision

**ALL phases completed.** ADR-019 is fully implemented with:
- Phase 1: Permission checks at ExtensionCore layer
- Phase 2: Dynamic tool definitions rebuilt each iteration
- Phase 3: Dynamic system prompt rebuilt each iteration

Phase 4 (Optimization) is future work and can be tackled when performance needs require it.

## Updated Implementation Plan

### Phase 0: Foundation ✅ COMPLETED
ADR-018a is complete. All tools now route through ExtensionCore, enabling centralized permission checking.

### Phase 1: Permission Check ✅ COMPLETED

Tool permission checking has been implemented in `ExtensionCore::invoke_hook()`:

```rust
// src/extensions/core/registry.rs
pub async fn invoke_hook(&self, point: HookPoint, input: HookInput) -> HookResult {
    // Check global enable
    if !self.is_globally_enabled().await {
        return HookResult::PassThrough;
    }
    
    // ADR-019 Phase 1: Tool permission check at ExtensionCore layer
    if let HookPoint::ToolExecute { ref tool_name } = point {
        if !self.is_tool_enabled(tool_name).await {
            warn!(tool_name = %tool_name, "Tool execution blocked: tool is not enabled");
            return HookResult::Error(
                anyhow::anyhow!(
                    "Tool '{}' is currently disabled. Enable it in agent config to use it.",
                    tool_name
                )
            );
        }
    }
    // ... rest of dispatch
}
```

**Implementation Details:**
- Permission check happens at the ExtensionCore layer for ALL tool types
- Tool configuration is passed from AgentRunner before each run via `init_tool_config()`
- Clear error message when disabled tools are called
- Non-tool hooks (ToolRegister, etc.) are not affected

**Tests Added:**
- `test_tool_execute_blocked_when_disabled` - Verifies disabled tools are blocked
- `test_tool_execute_permitted_when_enabled` - Verifies enabled tools work
- `test_non_tool_hooks_not_affected_by_tool_config` - Verifies other hooks unaffected

### Phase 2: Dynamic Tool Definitions ✅ COMPLETED

Tool definitions are now rebuilt dynamically each iteration:

```rust
// src/engine/loop_v4.rs
loop {
    iteration += 1;
    
    // ADR-019 Phase 2: Build tool definitions dynamically each iteration
    let tool_defs = self.build_tool_definitions_fresh().await;
    
    // Provider receives updated tool list
    let response = self.provider
        .chat_with_tools(&messages, &tool_defs, &options)
        .await?;
}
```

**Implementation Details:**
- `build_tool_definitions_fresh()` queries `ExtensionCore::list_tool_definitions()`
- Provider receives current tool list each iteration
- Tool enable/disable changes take effect immediately

### Phase 3: Dynamic System Prompt ✅ COMPLETED

System prompt is now rebuilt dynamically each iteration:

```rust
// src/engine/loop_v4.rs
// ADR-019 Phase 3: Rebuild system prompt dynamically
if !messages.is_empty() && matches!(messages[0].role, MessageRole::System) {
    let fresh_prompt = self.build_system_prompt_fresh().await;
    messages[0] = ChatMessage {
        role: MessageRole::System,
        content: vec![ContentBlock::Text { text: fresh_prompt }],
        tool_calls: None,
        tool_call_id: None,
    };
}
```

**Implementation Details:**
- `SystemPromptBuilder` now supports `with_tool_definitions()` for dynamic updates
- `build_system_prompt_fresh()` queries ExtensionCore for current tools
- SYSTEM.md changes reflected immediately (via BootstrapConfig)
- Tool list in prompt matches current config

### Phase 4: Optimization (Future)

Future performance improvements:
- Cache prompt sections that rarely change
- Invalidate cache on file/config changes
- Only rebuild changed sections

**Expected Impact:** <5ms latency reduction per iteration

## Progress Tracking

| Phase | Status | PR | Notes |
|-------|--------|-----|-------|
| ADR-018a: Tool Execution Unification | ✅ Completed | - | All tools now route through ExtensionCore |
| ADR-018b: Unified Tool Registry | ✅ Completed | - | `register_tool()`, `list_tools()`, `get_tool_metadata()` |
| ADR-018c: Tool Naming Cleanup | 🔲 Not Started | - | Can proceed independently |
| Phase 1: Permission Check | ✅ Completed | - | Centralized enforcement in ExtensionCore |
| Phase 2: Dynamic Tool Definitions | ✅ Completed | - | Rebuilt each iteration from ExtensionCore |
| Phase 3: Dynamic System Prompt | ✅ Completed | - | Rebuilt each iteration with current tools |
| Phase 4: Optimization | 🔲 Future | - | Low priority - caching for performance |

## References

- ADR-018a (Prerequisite): `docs/architecture/adr/ADR-018a-tool-execution-unification.md`
- ADR-018b (Prerequisite): `docs/architecture/adr/ADR-018b-unified-tool-registry.md`
- ADR-018c (Recommended): `docs/architecture/adr/ADR-018c-tool-naming-cleanup.md`
- Current static implementation: `src/engine/loop_v4.rs:71-88`
- Tool execution: `src/engine/tool_executor.rs:117`
- System prompt builder: `src/prompt/builder.rs`
- Tool config: `src/types/agent.rs:202-273`
