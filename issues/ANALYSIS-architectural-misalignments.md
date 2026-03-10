# ANALYSIS-001: Architectural Misalignments

**Purpose:** Document specific code locations that violate architectural principles

---

## 1. Violation: Global Dead Code Suppression

**Location:** `src/lib.rs:5-14`

```rust
#![allow(
    dead_code,
    unused_async,
    clippy::missing_errors_doc,
    // ... 8 more
)]
```

**Architectural Principle:** Auditability - "Complete execution trail"

**Violation:** Suppressing warnings hides potentially unused code that should be removed or used.

**Impact:** Technical debt accumulation, larger binary size, confusion about what's actually used.

**Fix:** Remove global allow, use targeted allows with comments explaining why.

---

## 2. Violation: Duplicate Channel Systems

**Locations:** 
- `src/channels/mod.rs` (Line 71: Channel trait)
- `src/agent/channels/mod.rs` (Line 71: Channel trait)

**Architectural Principle:** Minimal core - "~2MB runtime, everything else is user-installed"

**Violation:** Two separate channel implementations increase core size and maintenance burden.

**Comparison:**

| Feature | `src/channels/` | `src/agent/channels/` |
|---------|-----------------|----------------------|
| Trait name | `Channel` | `Channel` (same name!) |
| Gateway integration | ✅ Via GatewayPlugin | ❌ None |
| Streaming support | ✅ Full | ❌ Basic |
| Discord implementation | 200+ lines | 150+ lines (different) |
| CLI implementation | Interactive | Interactive (different) |

**Impact:** 
- Binary bloat
- Confusion about which to use
- Divergent implementations
- Double maintenance

**Fix:** Delete `src/agent/channels/`, migrate any unique functionality to `src/channels/`.

---

## 3. Violation: Conflicting Type Names

**Location:** Multiple files

**Architectural Principle:** Clear abstractions

**Violations:**

### 3.1 SessionRegistry
```rust
// src/tools/session_messaging.rs:24
pub struct SessionRegistry { /* messaging */ }

// src/tools/session_introspection.rs:156  
pub trait SessionRegistry { /* introspection */ }
```

**Problem:** Same name, completely different purposes. Import collisions.

### 3.2 AgentContext
```rust
// src/agent/context.rs:11
pub struct AgentContext { registry_view, agent_states }

// src/types/message.rs:398
pub struct AgentContext { steered_provider, transformers }
```

**Problem:** Two contexts with same name for different purposes.

### 3.3 AgentConfig
```rust
// src/types/agent.rs:9
pub struct AgentConfig { name, capabilities, ... }

// src/config/mod.rs:14
pub struct AgentConfig { name, model, system_prompt, ... }
```

**Problem:** Two configs for the same entity! Likely one is obsolete.

**Impact:**
- Import confusion
- Wrong type errors
- Maintenance confusion

**Fix:** Rename to disambiguate:
- `SessionRegistry` → `AgentInbox`
- `SessionRegistry` (trait) → `SessionIntrospection`
- `AgentContext` (message) → `SteeringContext`
- Consolidate `AgentConfig`

---

## 4. Violation: Tool Trait Complexity

**Location:** `src/tools/traits.rs`

**Architectural Principle:** "Tools are simple synchronous functions"

**Current Trait:**
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn llm_description(&self) -> String;  // Duplicate of description?
    fn parameters(&self) -> serde_json::Value;
    
    // Method 1: Basic execution
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;
    
    // Method 2: Context-aware execution (with progress, abort)
    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value>;
    
    fn supports_progress(&self) -> bool;  // Not used by engine
    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64;  // Not used
}
```

**Problems:**
1. Two execution methods - confusing
2. `supports_progress` - engine doesn't check this
3. `estimated_duration_ms` - engine doesn't use this
4. `llm_description` vs `description` - unclear distinction

**Architectural Intent:** Per GRAND_ARCHITECTURE: "Tools don't know about sync/async. They just execute and return."

**Fix:** Simplify to single method with context:
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;
    
    async fn execute(
        &self, 
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value>;
}
```

---

## 5. Violation: Module Structure Confusion

**Location:** `src/agent/`

**Current Structure:**
```
src/agent/
├── mod.rs              # Public re-exports
├── agent.rs            # Single Agent struct
├── manager.rs          # AgentManager (494 lines)
├── manager_mod.rs      # Re-exports with #![allow(dead_code)]
├── pool.rs             # AgentPool
├── lifecycle.rs        # LifecycleManager  
├── registry.rs         # LocalRegistry
├── commands.rs         # ManagerCommand
├── context.rs          # AgentContext
├── types.rs            # ManagerEvent, AgentInfo
└── channels/           # DUPLICATE
```

**Problems:**
1. `manager.rs` vs `manager_mod.rs` - what's the difference?
2. `manager_mod.rs` is just re-exports (32 lines) with dead_code allow
3. Too many tiny modules
4. `commands.rs` contains ManagerCommand but tools also have commands

**Architectural Intent:** Clear separation of concerns

**Fix:**
```
src/agent/
├── mod.rs              # Clean public API
├── agent.rs            # Single Agent
├── manager/            # Everything manager-related
│   ├── mod.rs          # AgentManager
│   ├── pool.rs         # AgentPool (internal)
│   ├── lifecycle.rs    # LifecycleManager (internal)
│   └── registry.rs     # LocalRegistry (internal)
├── messaging.rs        # Agent-to-agent (from tools/session_messaging.rs)
├── context.rs          # AgentContext
└── types.rs            # Public types only
```

---

## 6. Violation: Cron in Wrong Module

**Location:** `src/cron/mod.rs`

**Architectural Principle:** Three orthogonal layers

**Current:** Cron scheduler in top-level `src/cron/`

**Architectural Intent:** Per GRAND_ARCHITECTURE, scheduling is part of Orchestration layer:
```
src/orchestration/
├── scheduler/          # Time-based (cron, interval, idle)
├── event_router/       # Event-based
└── lifecycle/          # Already exists as agent/lifecycle.rs
```

**Fix:** Move `src/cron/` to `src/orchestration/scheduler/`

---

## 7. Violation: In-Memory Registry Per Agent

**Location:** `src/agent/manager.rs:405`

```rust
// In AgentManager::create_communication_tools()
let registry = Arc::new(SessionRegistry::new()); // NEW INSTANCE PER AGENT!
tools.push(Arc::new(SessionMessagingTool::new(
    registry,
    agent_did.to_string(),
)));
```

**Architectural Principle:** Agents can message each other

**Violation:** Each agent gets its own `SessionRegistry` - they're not shared!

**Impact:** Agent A sends message to Agent B's registry instance ≠ Agent B's registry instance

**Fix:** Single shared registry at manager level:
```rust
pub struct AgentManager {
    // ...
    shared_inbox: Arc<AgentInbox>, // Shared across all agents
}
```

---

## 8. Violation: Heavy Dependencies in Core

**Location:** `Cargo.toml:73-78`

```toml
# HTML parsing for web_search
scraper = "0.19"
readability = "0.3"
```

**Architectural Principle:** Minimal core ~2MB, externalize heavy tools

**Violation:** Web scraping dependencies in core binary

**Impact:** 
- Core size bloat
- Violates "web tools moved to MCP" principle

**Fix:** These should be in MCP, not core.

---

## 9. Violation: Unimplemented TODOs

**Critical TODOs indicating architectural gaps:**

| Location | TODO | Gap |
|----------|------|-----|
| `daemon/mod.rs:293` | "Implement system event queue" | GAP-004 |
| `daemon/mod.rs:320` | "Implement isolated agent spawning" | GAP-002 |
| `daemon/mod.rs:348` | "Implement actual delivery to channels" | GAP-005 |
| `commands/agent.rs:509` | "Implement actual export via Packager" | Feature incomplete |
| `commands/agent.rs:535` | "Implement actual import via Unpackager" | Feature incomplete |

**Fix:** Either implement or remove and track in issues.

---

## 10. Violation: No Feature Flags

**Location:** `Cargo.toml`

**Architectural Principle:** Minimal core, opt-in features

**Current:** No feature flags - everything compiled in

**Impact:** Cannot build minimal core without browser, web search, etc.

**Fix:** Add feature flags as documented in REFACTOR-001.

---

## Summary Table

| # | Violation | Severity | File(s) | Fix Complexity |
|---|-----------|----------|---------|----------------|
| 1 | Global dead_code allow | Medium | lib.rs | Low |
| 2 | Duplicate channels | High | channels/, agent/channels/ | Medium |
| 3 | Conflicting type names | High | tools/, types/ | Medium |
| 4 | Tool trait complexity | Medium | tools/traits.rs | Low |
| 5 | Module structure | Medium | agent/ | Medium |
| 6 | Cron location | Low | cron/ | Low |
| 7 | Per-agent registry | High | agent/manager.rs | Medium |
| 8 | Heavy dependencies | Medium | Cargo.toml | Medium |
| 9 | TODOs | Varies | multiple | Varies |
| 10 | No feature flags | Medium | Cargo.toml | Low |

---

## Recommended Priority Order

1. **#2** - Duplicate channels (blocks GAP-010)
2. **#3** - Conflicting names (blocks clarity)
3. **#7** - Per-agent registry (blocks GAP-005)
4. **#5** - Module structure (foundational)
5. **#4** - Tool trait (affects all tools)
6. **#1, #6, #8, #10** - Cleanup
7. **#9** - Address TODOs
