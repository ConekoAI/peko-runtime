# Extension Architecture Refactoring Plan

**Goal:** Make Extensions 2.0 the single source of truth for all extensibility.

**Status:** In Progress  
**Started:** 2026-04-11

---

## Current Problems

```
┌─────────────────────────────────────────────────────────────┐
│                         ISSUES                              │
├─────────────────────────────────────────────────────────────┤
│  1. src/tool_management/    → DEAD CODE                     │
│  2. src/cap/                → DUPLICATES extensions         │
│  3. src/tool_registry/      → DUPLICATES extensions         │
│  4. src/tools/factory.rs    → Does its own discovery        │
│  5. Parallel discovery      → DRY violation                 │
│  6. Built-in registry       → Separate from extensions      │
└─────────────────────────────────────────────────────────────┘
```

---

## Target Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    UNIFIED ARCHITECTURE                     │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  pekobot ext <command>                              │   │
│  │  (install/enable/disable/uninstall/list/bundle)    │   │
│  └──────────────────────┬──────────────────────────────┘   │
│                         │                                   │
│  ┌──────────────────────▼──────────────────────────────┐   │
│  │  ExtensionManager                                   │   │
│  │  - Routes to appropriate adapter                    │   │
│  │  - Manages enable/disable state                     │   │
│  │  - Handles bundling                                 │   │
│  └──────────────────────┬──────────────────────────────┘   │
│                         │                                   │
│    ┌────────────────────┼────────────────────┐             │
│    ▼                    ▼                    ▼             │
│ ┌─────────┐      ┌──────────┐      ┌──────────────┐       │
│ │  Skill  │      │   MCP    │      │   Universal  │       │
│ │ Adapter │      │  Adapter │      │    Adapter   │       │
│ └────┬────┘      └────┬─────┘      └──────┬───────┘       │
│      │                │                   │                │
│      └────────────────┼───────────────────┘                │
│                       │                                     │
│  ┌────────────────────▼────────────────────┐               │
│  │        ExtensionCore (22 hooks)         │               │
│  │                                         │               │
│  │  • ToolRegister                         │               │
│  │  • ToolExecute / ToolExecuteAsync      │               │
│  │  • PromptSystemSection                 │               │
│  │  • AgentInit / AgentShutdown           │               │
│  │  • etc.                                 │               │
│  └────────────────────┬────────────────────┘               │
│                       │                                     │
│  ┌────────────────────▼────────────────────┐               │
│  │         Tool Execution Layer            │               │
│  │                                         │               │
│  │  ┌─────────────┐  ┌─────────────────┐  │               │
│  │  │ Built-in    │  │ External Tools  │  │               │
│  │  │ Handlers    │  │ (MCP/Universal) │  │               │
│  │  └─────────────┘  └─────────────────┘  │               │
│  └─────────────────────────────────────────┘               │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

---

## Guiding Principles

| Principle | Enforcement |
|-----------|-------------|
| **SRP** | Extensions handle discovery/registration; Tool layer handles execution |
| **DRY** | Single discovery path through ExtensionManager |
| **KISS** | No parallel hierarchies |
| **Single Source of Truth** | ExtensionCore registry is the ONLY registry |

---

## Phase 1: Remove Dead Code

### 1.1 Delete `src/tool_management/`
- [x] **DONE** - 5 files, ~800 lines
- Not compiled, not used
- Functionality superseded by `ExtensionManager`

### 1.2 Delete `src/tool_registry/`
- [ ] **PENDING** - 3 files, ~435 lines
- Local registry functionality: use ExtensionManager
- Remote/Pekohub: Doesn't exist, add later if needed
- Used only by `cap/catalog.rs` (which we're also removing)

---

## Phase 2: Remove Duplicate Abstractions

### 2.1 Delete `src/cap/`
- [ ] **PENDING** - 6 files, ~900 lines
- Replaced by Extensions 2.0

**Migration:**
| `cap/` Function | Replacement |
|-----------------|-------------|
| `CapabilityCatalog::list_installed()` | `ExtensionManager::list()` |
| `CapabilityCatalog::get()` | `ExtensionManager::get()` |
| `BuiltInCapabilityRegistry` | Built-in tools register via `ExtensionCore` at startup |
| `CapabilityLifecycle` | Extension adapter `initialize()`/`shutdown()` |

### 2.2 Move Built-in Tool Registration

**Current:** `BuiltInCapabilityRegistry` in `cap/builtin.rs`

**New Approach:** Built-in tools self-register via ExtensionCore at startup

```rust
// src/tools/builtin_registry.rs (NEW - minimal)
pub struct BuiltInToolRegistry;

impl BuiltInToolRegistry {
    pub async fn register_with_core(core: &ExtensionCore) {
        // Register each built-in tool via ToolRegister hook
        core.invoke_hook(HookPoint::ToolRegister, 
            tool_definitions_vec.into()
        ).await;
    }
}
```

---

## Phase 3: Refactor Tool Factory

### 3.1 Current Problem
`src/tools/factory.rs` does its OWN discovery:
```rust
// WRONG - parallel discovery
fn create_tools() {
    // Built-in tools
    // MCP servers from mcp.toml  ← DUPLICATES McpAdapter
    // Universal tools from dir   ← DUPLICATES UniversalToolAdapter
}
```

### 3.2 New Approach
ToolFactory queries ExtensionCore ONLY:

```rust
// src/tools/factory.rs (REFACTORED)
pub struct ToolFactory;

impl ToolFactory {
    pub async fn create_tools_from_extensions(core: &ExtensionCore) -> Vec<Arc<dyn Tool>> {
        // Query ExtensionCore for registered tools
        let hooks = core.get_hooks(HookPoint::ToolRegister).await;
        
        // Convert hook registrations to tool instances
        hooks.into_iter()
            .map(|hook| self.create_tool_from_hook(hook))
            .collect()
    }
}
```

---

## Phase 4: Unified Extension Types

### 4.1 Current State (5 types, but messy)
- `skill` → `SkillAdapter`
- `mcp` → `McpAdapter`
- `universal-tool` → `UniversalToolAdapter`
- `hook` → `HookAdapter`
- `gateway` → `GatewayAdapter`

### 4.2 Clean State (4 types)
| Type | Purpose | Hook Points |
|------|---------|-------------|
| `skill` | Documentation | `PromptSystemSection` |
| `mcp` | MCP servers | `ToolRegister`, `ToolExecute`, `AgentInit`, `AgentShutdown` |
| `universal-tool` | Local executables | `ToolRegister`, `ToolExecute` |
| `gateway` | I/O channels | `ChannelInput`, `ChannelOutput`, `MessagePreSend`, etc. |

**Remove:** `hook` type - fold into `gateway` or remove entirely (was never functional)

---

## Phase 5: CLI Consolidation

### 5.1 Current State
- `pekobot ext` → Uses ExtensionManager ✅
- `pekobot tools` → DEAD (was in tool_management)
- Built-in enable/disable → Uses cap/TeamCapabilityManager

### 5.2 Target State
**Single command surface:** `pekobot ext`

| Command | Behavior |
|---------|----------|
| `pekobot ext list` | Lists ALL extensions (skills, MCP, tools, gateways) |
| `pekobot ext list --type mcp` | Filter by type |
| `pekobot ext enable shell` | Enable built-in tool (via ExtensionConfig) |
| `pekobot ext disable shell` | Disable built-in tool |
| `pekobot ext install ./my-mcp` | Install MCP server |
| `pekobot ext install ./my-skill` | Install skill |

---

## Implementation Steps

### Step 1: Remove Dead Code
- [x] Delete `src/tool_management/`
- [ ] Delete `src/tool_registry/`

### Step 2: Remove cap/
- [ ] Delete `src/cap/`

### Step 3: Create Minimal Built-in Registry
- [ ] Create `src/tools/builtin_registry.rs`

### Step 4: Refactor ToolFactory
- [ ] Remove all discovery logic
- [ ] Query ExtensionCore for `ToolRegister` hooks
- [ ] Create tool instances from hook registrations

### Step 5: Update ExtensionManager
- [ ] Remove `HookAdapter` (was never functional)
- [ ] Ensure built-in tools appear in `ext list`

### Step 6: Update CLI
- [ ] `pekobot ext` commands handle ALL extension types
- [ ] Remove any remaining `cap` references

---

## File Changes Summary

| Action | Files | Lines (approx) |
|--------|-------|----------------|
| **DELETE** | `src/tool_management/*` | -800 |
| **DELETE** | `src/tool_registry/*` | -435 |
| **DELETE** | `src/cap/*` | -900 |
| **CREATE** | `src/tools/builtin_registry.rs` | +150 |
| **REFACTOR** | `src/tools/factory.rs` | -200, +100 |
| **UPDATE** | `src/extensions/manager/mod.rs` | +50 |
| **UPDATE** | `src/commands/ext.rs` | -100, +50 |
| **UPDATE** | `src/lib.rs` | -3 |
| **Net** | | **-2,100 lines** |

---

## Validation Checklist

- [ ] `cargo build` passes
- [ ] `cargo test` passes
- [ ] `pekobot ext list` shows built-in tools
- [ ] `pekobot ext list` shows MCP servers
- [ ] `pekobot ext list` shows skills
- [ ] `pekobot ext disable shell` disables shell tool
- [ ] `pekobot ext enable shell` re-enables shell tool
- [ ] Agent can still use tools
- [ ] MCP servers still work
- [ ] Universal tools still work

---

## Final Architecture Principles

1. **Extensions 2.0 is the ONLY discovery mechanism**
2. **ExtensionCore is the ONLY registry**
3. **ToolFactory only EXECUTES, doesn't DISCOVER**
4. **Single CLI surface: `pekobot ext`**
5. **Built-in tools are first-class extensions**

This eliminates:
- Parallel discovery paths
- Duplicate registries
- Dead code
- Confusing multiple command surfaces
