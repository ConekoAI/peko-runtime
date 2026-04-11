# ADR-021: Dynamic Tool and System Prompt Updates

**Status**: Proposed  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  

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

#### 3. Tool Permission Check at Execution Layer

**Location**: `src/engine/tool_executor.rs:117`

```rust
pub async fn execute_with_context(
    &self,
    tool: Arc<dyn Tool>,
    params: serde_json::Value,
    context: &ToolExecutionContext,
) -> Result<serde_json::Value> {
    // ADD: Permission check against current config
    if !self.is_tool_enabled(&tool.name(), context).await {
        return Err(anyhow::anyhow!(
            "Tool '{}' is currently disabled. \
             Enable it in agent config to use it.",
            tool.name()
        ));
    }
    
    // ... existing execution logic
}

async fn is_tool_enabled(&self, tool_name: &str, context: &ToolExecutionContext) -> bool {
    // Load current ToolConfig and check whitelist
    let config = load_tool_config_for_session(&context.session_id).await;
    config.is_tool_enabled(tool_name)
}
```

**Alternative**: Use Extension Framework hook
```rust
// HookPoint::ToolExecute with wildcard pattern
// Could enable complex permission rules (time-based, rate-limited, etc.)
```

**Recommendation**: Direct check in executor (simpler, no hook overhead for security)

## Implementation Phases

### Phase 1: Tool Permission Check (Safety Net)
**Priority**: High  
**Risk**: Low  
**Effort**: ~2 hours

- Add `is_tool_enabled()` check in `ToolExecutor::execute_with_context()`
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

**Proceed with implementation** using the three-phase approach outlined above.

**Key Principles**:
1. Safety first: Permission check at execution layer (Phase 1)
2. Gradual rollout: Each phase independently valuable
3. Backward compatible: Changes are internal, no API changes
4. Measurable: Each phase has clear acceptance criteria

## Progress Tracking

| Phase | Status | PR | Notes |
|-------|--------|-----|-------|
| Phase 1: Permission Check | 🔲 Not Started | - | - |
| Phase 2: Dynamic Tool Definitions | 🔲 Not Started | - | - |
| Phase 3: Dynamic System Prompt | 🔲 Not Started | - | - |
| Phase 4: Optimization | 🔲 Not Started | - | Future work |

## References

- Current static implementation: `src/engine/loop_v4.rs:71-88`
- Tool execution: `src/engine/tool_executor.rs:117`
- System prompt builder: `src/prompt/builder.rs`
- Tool config: `src/types/agent.rs:202-273`
