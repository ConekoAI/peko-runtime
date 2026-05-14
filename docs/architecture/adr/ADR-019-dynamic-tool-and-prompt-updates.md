# ADR-019: Dynamic Tool and System Prompt Updates

**Status**: Phase 1 Complete, Phases 2-3 Partial  
**Completed Date**: 2026-04-11 (Phase 1), 2026-04-12 (Phases 2-3 Infrastructure)  
**Updated**: 2026-04-12  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Depends On**: ADR-018a (Tool Execution Unification), ADR-018b (Unified Tool Registry)

## Context

Currently, the agent's tool set and system prompt are determined at session start and remain static throughout the session. This creates several UX issues:

1. **Tool changes require session restart**: If a user enables/disables tools mid-session, the changes don't take effect until a new session is started
2. **System prompt is immutable**: Changes to SYSTEM.md or tool descriptions require session restart
3. **Security gap**: Disabled tools may still be callable by the LLM if they were enabled at session start

## Problem Statement

### Current Architecture (Static Registry)

```
Session Start
     │
     ▼
┌─────────────────────────────────────────┐
│  Agent::create_tools_async()            │
│  ───────────────────────────            │
│  • Filter tools by whitelist            │
│  • Register with ExtensionCore          │ ◄── Registry is STATIC after this
│  • System prompt built                  │
└─────────────────────────────────────────┘
     │
     ▼
┌─────────────────────────────────────────┐
│  Main Loop (each iteration)             │
│  ─────────────────────────              │
│  • Rebuild tool_defs from registry      │ ◄── Same tools every iteration
│  • Rebuild system prompt                │ ◄── Same tools mentioned
│  • Provider gets static tool list       │
└─────────────────────────────────────────┘
```

### Key Insight: Two Levels of "Dynamic"

| Level | What's Dynamic | What's Static | Current State |
|-------|---------------|---------------|---------------|
| **Loop Level** | Tool definitions rebuilt each iteration | Registry content | ✅ Works |
| **Registry Level** | Can enable/disable existing tools | Tools registered at startup | ❌ Not dynamic |

### The Real Gap

The loop rebuilds tool definitions and system prompt **from the ExtensionCore registry** each iteration. But the registry itself is populated **once at startup** in `Agent::create_tools_async()`. 

**To enable true mid-session tool changes, we need:**
1. A way to register new tools after startup (currently only at `create_tools_async()`)
2. A way to unregister tools mid-session (`unregister_tool()` exists but isn't used)
3. A trigger mechanism when config changes (`peko ext enable/disable` → update registry)

## Implementation Status

### Phase 1: Tool Permission Check ✅ COMPLETED

**Purpose**: Safety net - reject disabled tool calls at execution time

**Implementation**: `ExtensionCore::invoke_hook()` checks `is_tool_enabled()` before executing any tool:

```rust
// src/extensions/core/registry.rs
pub async fn invoke_hook(&self, point: HookPoint, input: HookInput) -> HookResult {
    // ADR-019 Phase 1: Tool permission check
    if let HookPoint::ToolExecute { ref tool_name } = point {
        if !self.is_tool_enabled(tool_name).await {
            return HookResult::Error(anyhow::anyhow!(
                "Tool '{}' is currently disabled. Enable it in agent config to use it.",
                tool_name
            ));
        }
    }
    // ... dispatch to handler
}
```

**Why this matters**: Even if a tool was registered at startup (because it was in the whitelist then), if it's later disabled, calls to it will be rejected. This is defense in depth.

**Status**: ✅ Working. All tool calls go through this check.

---

### Phase 2: Dynamic Tool Definitions ⚠️ PARTIAL

**Claimed**: Tool definitions rebuilt dynamically each iteration

**Reality**: Infrastructure in place, but registry is static

```rust
// src/engine/loop_v4.rs - INSIDE the loop
loop {
    // ADR-019 Phase 2: Build tool definitions dynamically each iteration
    let tool_defs = self.build_tool_definitions_fresh().await;
    // ...
}

async fn build_tool_definitions_fresh(&self) -> Vec<ToolDefinition> {
    // Queries ExtensionCore - but registry hasn't changed since startup!
    self.extension_core.list_tool_definitions().await
}
```

**What's Working**:
- ✅ Tool definitions are rebuilt each iteration
- ✅ Provider gets fresh list each time
- ✅ If registry were dynamic, changes would be picked up

**What's Missing**:
- ❌ Registry is static (populated once at startup)
- ❌ No mechanism to add/remove tools mid-session

---

### Phase 3: Dynamic System Prompt ⚠️ PARTIAL

**Claimed**: System prompt rebuilt dynamically each iteration

**Reality**: Infrastructure in place, but uses static registry

```rust
// src/engine/loop_v4.rs - INSIDE the loop
if !messages.is_empty() && matches!(messages[0].role, MessageRole::System) {
    let fresh_prompt = self.build_system_prompt_fresh().await;
    messages[0] = ChatMessage::system(fresh_prompt);
}

async fn build_system_prompt_fresh(&self) -> String {
    let tool_defs = self.extension_core.list_tool_definitions().await; // Static!
    SystemPromptBuilder::new(self.agent.name())
        .with_tool_definitions(tool_defs)
        // ...
        .build()
}
```

**What's Working**:
- ✅ System prompt is rebuilt each iteration
- ✅ SYSTEM.md changes are picked up (via file read)
- ✅ If registry were dynamic, tool list would update

**What's Missing**:
- ❌ Tool list comes from static registry

---

### Phase 4: True Dynamic Registry 🔲 NOT STARTED

**The Missing Piece**: Making `ExtensionCore` registry truly dynamic

**Required Changes**:

1. **API for mid-session registration**:
```rust
// New: Register a tool after startup
impl ExtensionCore {
    pub async fn register_tool_dynamic(&self, metadata: ToolMetadata, handler: Arc<dyn HookHandler>) -> Result<()> {
        // Same as register_tool, but allows adding new tools
        // (remove duplicate check or make it idempotent)
    }
}
```

2. **Trigger mechanism**:
```rust
// When user runs: peko ext enable <tool>
// Currently: Only updates config file
// Needed: Also register with ExtensionCore if session active
```

3. **Tool lifecycle management**:
```rust
// Track which tools are "built-in" vs "session-added"
// Enable/disable should toggle visibility, not just permission
```

---

## Why "ext enable/disable" Doesn't Need to Notify Sessions

**Your question**: Why should ext enable/disable notify sessions if both system prompt and tool definitions are rebuilt anew for each message?

**Answer**: It shouldn't! The session loop already rebuilds everything each iteration. The issue is that the **registry** is static.

**Correct Architecture**:

```
┌─────────────────────────────────────────────────────────────────┐
│  User runs: peko ext enable shell                             │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  1. Update agent.toml (whitelist)                                │
│  2. Call ExtensionCore::register_tool("shell", ...)              │ ◄── ADD THIS
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│  Running Session (AgenticLoopV4)                                 │
│  ───────────────────────────────                                 │
│  Loop iteration N:                                               │
│    • build_tool_definitions_fresh() → sees "shell" in registry   │
│    • build_system_prompt_fresh() → includes shell description    │
│    • provider gets shell schema                                  │
└─────────────────────────────────────────────────────────────────┘
```

**No notification needed!** The next iteration picks up the change automatically **IF** the registry is updated.

---

## Current Behavior vs Desired

### Scenario A: Tool disabled at startup → enabled mid-session

| Aspect | Current | Desired |
|--------|---------|---------|
| Registry | Shell never registered | Shell registered when enabled |
| System Prompt | No shell description | Shell description appears |
| Provider Schema | No shell | Shell appears |
| LLM Can Use | No | Yes |

**Current workaround**: Restart session after enabling tool

### Scenario B: Tool enabled at startup → disabled mid-session

| Aspect | Current | Desired |
|--------|---------|---------|
| Registry | Shell stays registered | Shell unregistered when disabled |
| System Prompt | Shell description stays | Shell description removed |
| Provider Schema | Shell stays | Shell removed |
| LLM Can Use | Blocked at execution (Phase 1) | Tool invisible to LLM |

**Current behavior**: Phase 1 check blocks execution, but LLM still sees tool (confusing)

---

## Updated Implementation Plan

### Phase 1: Permission Check ✅ COMPLETED
Tool permission checking at ExtensionCore layer. All tool calls validated against whitelist.

### Phase 2/3: Dynamic Rebuild Infrastructure ✅ COMPLETED  
Loop rebuilds tool definitions and system prompt each iteration from registry.

### Phase 4: Dynamic Registry 🔲 NOT STARTED
Make ExtensionCore registry mutable mid-session:

**Tasks**:
- [ ] Allow `register_tool` to be called after startup (idempotent, allow updates)
- [ ] Connect `peko ext enable` to `register_tool` for active sessions
- [ ] Connect `peko ext disable` to `unregister_tool` for active sessions
- [ ] Handle edge cases (tool in use, pending calls, etc.)

**Effort**: ~8 hours
**Priority**: Medium - current workaround (session restart) works

---

## Progress Tracking

| Component | Status | Notes |
|-----------|--------|-------|
| Phase 1: Permission Check | ✅ Complete | `is_tool_enabled()` in `invoke_hook()` |
| Phase 2/3: Dynamic Rebuild | ✅ Complete | Rebuilt each iteration from registry |
| Phase 4: Dynamic Registry | 🔲 Not Started | Registry still populated once at startup |
| E2E Tests | ✅ Complete | `adr019_phase1_test.ps1` verifies whitelist enforcement |

---

## References

- ADR-018a (Prerequisite): `docs/architecture/adr/ADR-018a-tool-execution-unification.md`
- ADR-018b (Prerequisite): `docs/architecture/adr/ADR-018b-unified-tool-registry.md`
- Current implementation: `src/engine/loop_v4.rs`
- Tool registration: `src/agent/agent.rs:237-245`
- Permission check: `src/extensions/core/registry.rs:331-344`
