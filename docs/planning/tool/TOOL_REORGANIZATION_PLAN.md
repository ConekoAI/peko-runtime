# Tool Reorganization & Cleanup Plan

## Executive Summary

Current built-in tools: **18 tools** across **13 source files**
Target after cleanup: **15 tools** across **10 source files**
- Remove 2 deprecated/legacy tools
- Consolidate 1 duplicate
- Move 1 demo tool to examples

---

## 1. Issues Identified

### Issue A: Duplicate Agent Spawn Tools

| Tool | Location | Status | API |
|------|----------|--------|-----|
| `agent_spawn` (v1) | `agent_management.rs:214` | Legacy | Custom format |
| `agent_spawn_v2` | `agent_spawn_v2.rs:111` | Active | OpenClaw-compatible |

**Problem**: Two different spawn tools with incompatible APIs. The v1 tool is still exposed to LLMs alongside v2.

**Impact**: Confusion for both users and LLMs about which tool to use.

### Issue B: Deprecated Session Messaging

| Tool | Location | Status | Replacement |
|------|----------|--------|-------------|
| `session_messaging` | `session_messaging.rs:88` | Deprecated | `agent_invoke` |

**Problem**: File marked as deprecated but still exported and usable. The header says "will be removed in future version" but it's still active.

**Impact**: Dead code adding maintenance burden.

### Issue C: Overlapping Responsibilities

Three different "messaging" tools with different purposes but potentially confusing names:

```
agent_invoke        → Agent-to-agent (sync/async) - INTERNAL
agent_broadcast     → Broadcast to ALL agents - INTERNAL  
message             → External channels (Discord/Slack) - EXTERNAL
session_messaging   → Agent-to-agent (deprecated) - INTERNAL
```

---

## 2. Cleanup Actions

### Action 1: Remove Deprecated `session_messaging`

**File**: `src/tools/session_messaging.rs`

**Steps**:
1. Remove `pub mod session_messaging` from `src/tools/mod.rs`
2. Remove `SessionMessagingTool` from `src/tools/mod.rs` exports
3. Delete `src/tools/session_messaging.rs`
4. Remove `AgentInbox` references from codebase

**Risk**: Low (already deprecated, no dependencies)

---

### Action 2: Consolidate Agent Spawn (Remove v1)

**File**: `src/tools/agent_management.rs`

**Steps**:
1. Remove `AgentSpawnTool` struct (lines 214-278)
2. Remove `SpawnSession` variant from `ManagerCommand` (if not used elsewhere)
3. Keep `AgentSpawnToolV2` as the only spawn tool
4. Consider renaming `agent_spawn_v2` → `agent_spawn` for cleaner API

**Risk**: Medium (need to verify no code still uses v1)

**Migration Path**:
```rust
// Before (confusing)
"agent_spawn"    // v1 - legacy
"agent_spawn_v2" // v2 - current

// After (clean)
"agent_spawn"    // v2 - only option
```

---

### Action 3: Move Demo Tool to Examples

**File**: `src/tools/progress_demo.rs`

**Steps**:
1. Move to `examples/progress_demo_tool.rs`
2. Remove from `src/tools/mod.rs`
3. Keep as reference implementation for progress bars

**Risk**: None (demo only)

---

### Action 4: Reorganize File Structure

**Current Structure**:
```
src/tools/
├── mod.rs              (exports everything)
├── agent_management.rs (4 tools - mixed concerns)
├── agent_spawn_v2.rs   (3 tools)
├── agent_invoke.rs     (1 tool)
├── session_messaging.rs (1 tool - DEPRECATED)
├── session_introspection.rs (3 tools)
├── message_tool.rs     (1 tool)
├── filesystem.rs       (1 tool)
├── process.rs          (1 tool)
├── apply_patch.rs      (1 tool)
├── cron_tool.rs        (1 tool)
├── progress_demo.rs    (1 tool - demo)
└── ...
```

**Proposed Structure**:
```
src/tools/
├── mod.rs
├── core/
│   ├── mod.rs
│   ├── filesystem.rs
│   ├── process.rs
│   └── apply_patch.rs
├── agent/
│   ├── mod.rs
│   ├── spawn.rs          (was agent_spawn_v2.rs)
│   ├── invoke.rs         (was agent_invoke.rs)
│   ├── management.rs     (was agent_management.rs minus spawn)
│   └── broadcast.rs      (optional split)
├── session/
│   ├── mod.rs
│   └── introspection.rs  (was session_introspection.rs)
├── external/
│   ├── mod.rs
│   └── messaging.rs      (was message_tool.rs)
├── scheduling/
│   ├── mod.rs
│   └── cron.rs           (was cron_tool.rs)
└── internal/             (not exposed to LLM)
    ├── traits.rs
    ├── context.rs
    └── factory.rs
```

---

## 3. Consolidated Tool API

After cleanup, the 15 built-in tools would be:

| Tool | Category | Purpose | Input |
|------|----------|---------|-------|
| `filesystem` | Core | File operations | `{"action": "read\|write\|list", "path": "..."}` |
| `process` | Core | Shell execution | `{"command": "...", "args": []}` |
| `apply_patch` | Core | Search/replace | `{"path": "...", "search": "...", "replace": "..."}` |
| `agent_spawn` | Agent | Spawn subagent | `{"task": "...", "isolated": bool}` |
| `agent_spawn_status` | Agent | Check spawn status | `{"spawn_id": "..."}` |
| `agent_spawn_list` | Agent | List spawns | `{}` |
| `agent_invoke` | Agent | Message agent (A2A) | `{"target_did": "...", "content": "..."}` |
| `agent_broadcast` | Agent | Broadcast to all | `{"message": "..."}` |
| `agents_list` | Agent | List all agents | `{}` |
| `agent_info` | Agent | Get agent info | `{"agent_id": "..."}` |
| `sessions_list` | Session | List sessions | `{}` |
| `sessions_history` | Session | Session history | `{"session_key": "..."}` |
| `session_status` | Session | Session status | `{"session_key": "..."}` |
| `message` | External | Send to channels | `{"channel": "discord", "content": "..."}` |
| `cron` | Scheduling | Schedule tasks | `{"schedule": "...", "command": "..."}` |

---

## 4. Implementation Phases

### Phase 1: Remove Deprecated (Low Risk)
- [ ] Remove `session_messaging.rs`
- [ ] Remove `SessionMessagingTool` exports
- [ ] Update `mod.rs`

### Phase 2: Consolidate Spawn (Medium Risk)
- [ ] Audit codebase for `AgentSpawnTool` v1 usage
- [ ] Remove v1 from `agent_management.rs`
- [ ] Optionally rename `agent_spawn_v2` → `agent_spawn`
- [ ] Update tests

### Phase 3: Reorganize Files (Low Risk)
- [ ] Move `progress_demo.rs` to examples
- [ ] Restructure directories (core/, agent/, session/, external/)
- [ ] Update all imports

### Phase 4: Update Factory (Medium Risk)
- [ ] Update `ToolFactory` to use new structure
- [ ] Ensure MCP fallback still works
- [ ] Update tool discovery

### Phase 5: Documentation
- [ ] Update tool documentation
- [ ] Update E2E tests
- [ ] Update migration guide

---

## 5. Migration Impact

### For Users
- `agent_spawn` v1: Will use v2 automatically (if we rename)
- `session_messaging`: Must use `agent_invoke` instead
- All other tools: No changes

### For Developers
- File paths change (e.g., `tools::agent_spawn_v2::AgentSpawnToolV2` → `tools::agent::spawn::AgentSpawnTool`)
- Cleaner module hierarchy
- Easier to find tools

---

## 6. Benefits

1. **Reduced Complexity**: 18 → 15 tools
2. **Clearer API**: No duplicate spawn tools
3. **Better Organization**: Logical grouping by category
4. **Less Maintenance**: Remove deprecated code
5. **Easier Onboarding**: New devs can find tools faster

---

## 7. Decision Needed

Should I proceed with implementing this reorganization plan? The key decisions are:

1. **Remove `session_messaging`** - Straightforward, already deprecated
2. **Remove `AgentSpawnTool` v1** - Need to verify no active usage
3. **Rename `agent_spawn_v2` → `agent_spawn`** - Cleaner API but breaking change
4. **File restructure** - Cosmetic but improves maintainability
