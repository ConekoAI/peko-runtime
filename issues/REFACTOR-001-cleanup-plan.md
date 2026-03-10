# REFACTOR-001: Pre-Gap Cleanup & Foundation Plan

**Status:** Planning  
**Priority:** 🔴 Critical (Do Before Gaps)  
**Est. Effort:** 1-2 weeks  

---

## Overview

Before implementing the [architecture gaps](./index.md), we must clean up technical debt and align the codebase with architectural principles. This document identifies:

1. **Cleanup items** - Remove dead code, duplicates, fix warnings
2. **Refactoring items** - Restructure for alignment with architecture
3. **Foundation items** - Establish patterns needed for gap implementation

---

## 1. CRITICAL CLEANUP (Do First)

### 1.1 Remove Global `#[allow(dead_code)]`

**File:** `src/lib.rs` lines 5-14

```rust
#![allow(
    dead_code,
    unused_async,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::unused_self,
    clippy::format_push_string,
    clippy::unnecessary_debug_formatting,
    clippy::pass_by_ref_mut
)]
```

**Problem:** This global suppression hides ~13 files with actual dead code.

**Action:**
1. Remove global allow
2. Add targeted allows only where needed
3. Delete or use actually-dead code

**Affected Files:**
- `src/commands/agent.rs` (1 dead_code allow)
- `src/agent/pool.rs` (1 dead_code allow)
- `src/agent/manager_mod.rs` (entire file marked)
- `src/gateway/loader.rs` (1 dead_code allow)
- `src/memory/markdown.rs` (1 dead_code allow)
- `src/cron/mod.rs` (1 dead_code allow)
- `src/portable/mod.rs` (1 dead_code allow)
- `src/portable/unpackager.rs` (1 dead_code allow)
- `src/tool_registry/remote.rs` (1 dead_code allow)
- `src/tool_registry/mod.rs` (1 dead_code allow)

---

### 1.2 Merge Duplicate Channel Systems

**Problem:** Two separate channel implementations:
- `src/channels/` - Original channel trait (CLI, Discord, HTTP, Matrix, Slack, Telegram, WhatsApp)
- `src/agent/channels/` - Duplicate implementations with same names

**Evidence:**
```bash
$ ls src/channels/
cli.rs discord.rs http.rs matrix.rs mod.rs slack.rs telegram.rs whatsapp.rs

$ ls src/agent/channels/
cli.rs discord.rs http.rs matrix.rs mod.rs slack.rs telegram.rs whatsapp.rs
```

**Action:**
1. Compare implementations
2. Keep the one aligned with `GatewayPlugin` trait (likely `src/channels/`)
3. Delete duplicate in `src/agent/channels/`
4. Update all imports

**Decision Matrix:**
| Aspect | `src/channels/` | `src/agent/channels/` | Keep |
|--------|-----------------|----------------------|------|
| GatewayPlugin integration | ✅ Yes | ❌ No | src/channels |
| Streaming config | ✅ Yes | ❌ No | src/channels |
| handle_stream method | ✅ Yes | ❌ No | src/channels |

**Verdict:** Delete `src/agent/channels/`, keep `src/channels/`

---

### 1.3 Consolidate SessionRegistry Types

**Problem:** Three different SessionRegistry implementations:

```rust
// 1. src/tools/session_messaging.rs:24
pub struct SessionRegistry {
    sessions: Mutex<HashMap<String, Vec<SessionMessage>>>,
    subscribers: Mutex<HashMap<String, mpsc::Sender<SessionMessage>>>,
}

// 2. src/tools/session_introspection.rs:156
pub trait SessionRegistry: Send + Sync {
    fn list_sessions(&self) -> Vec<SessionInfo>;
    // ...
}

// 3. src/tools/session_introspection.rs:367
pub struct InMemorySessionRegistry {
    current_session_id: String,
    sessions: Vec<SessionInfo>,
}
```

**Problems:**
- Name collision causing confusion
- Different purposes but similar names
- No clear separation of concerns

**Action:**
```rust
// Rename for clarity:
// SessionRegistry (session_messaging) -> AgentInbox
// SessionRegistry (trait) -> SessionIntrospection
// InMemorySessionRegistry -> InMemorySessionIntrospection
```

---

### 1.4 Deduplicate Message/Agent Types

**Problem:** Multiple similar types across modules:

| Type | Location 1 | Location 2 | Action |
|------|-----------|-----------|--------|
| `AgentContext` | `src/agent/context.rs:11` | `src/types/message.rs:398` | Consolidate |
| `AgentInfo` | `src/agent/types.rs:32` | `src/tools/agent_management.rs:35` (AgentListEntry) | Consolidate |
| `ChatMessage` | `src/types/provider.rs:121` | `src/providers/traits.rs:130` | Consolidate |
| `AgentConfig` | `src/types/agent.rs:9` | `src/config/mod.rs:14` | Consolidate |

**Action:**
1. Move all type definitions to `src/types/`
2. Re-export from original locations for backward compat
3. Deprecate duplicates with `#[deprecated]`

---

## 2. ARCHITECTURAL REFACTORING

### 2.1 Separate Orchestration from Communication

**Current:** Daemon handles both cron (orchestration) and some channel logic

**Target per Architecture:**
```
src/orchestration/     # NEW
├── mod.rs
├── scheduler.rs       # Move from src/cron/
├── event_router.rs    # NEW for GAP-004
├── idle_detector.rs   # NEW for GAP-006
└── webhook_server.rs  # NEW for GAP-004

src/channels/          # Communication only
├── mod.rs
├── cli.rs
├── discord.rs
└── ...
```

**Action:**
1. Create `src/orchestration/` module
2. Move cron/scheduler into it
3. Clear separation: orchestration = system→agent, channels = user→agent

---

### 2.2 Tool Trait Alignment

**Current:** Tool trait has grown with ad-hoc additions

**File:** `src/tools/traits.rs`

**Issues:**
1. `execute` and `execute_with_context` - two methods for same thing
2. `estimated_duration_ms` - not used by engine
3. `supports_progress` - unclear semantics

**Proposed Clean Trait:**
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn llm_description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;
    
    /// Single execution method with context
    async fn execute(
        &self, 
        params: serde_json::Value,
        ctx: &ToolContext,  // Always provide context
    ) -> anyhow::Result<serde_json::Value>;
}
```

---

### 2.3 Agent Manager Module Cleanup

**Problem:** `src/agent/` has confusing module structure:

```
src/agent/
├── mod.rs          # Re-exports everything
├── agent.rs        # Single agent struct
├── manager.rs      # AgentManager (main implementation)
├── manager_mod.rs  # Re-exports manager components (dead_code!)
├── pool.rs         # AgentPool
├── lifecycle.rs    # LifecycleManager
├── registry.rs     # LocalRegistry
├── commands.rs     # ManagerCommand handling
├── context.rs      # AgentContext
├── types.rs        # ManagerEvent, AgentInfo
└── channels/       # DUPLICATE (to be deleted)
```

**Issues:**
1. `manager.rs` and `manager_mod.rs` - confusing naming
2. `manager_mod.rs` is just re-exports with `#![allow(dead_code)]`
3. Too many small modules

**Proposed Structure:**
```
src/agent/
├── mod.rs              # Public API
├── agent.rs            # Single agent
├── manager/
│   ├── mod.rs          # AgentManager + re-exports
│   ├── pool.rs         # AgentPool (internal)
│   ├── lifecycle.rs    # LifecycleManager (internal)
│   └── registry.rs     # LocalRegistry (internal)
├── messaging.rs        # Agent-to-agent (moved from tools/)
├── context.rs          # AgentContext
└── types.rs            # Public types
```

**Delete:** `manager_mod.rs` (merge into `manager/mod.rs`)

---

## 3. FOUNDATION FOR GAPS

### 3.1 Async Execution Primitives

**For:** GAP-002 (System-Managed Async)

**Create:** `src/engine/execution.rs`

```rust
/// Core execution primitives needed for async support
pub enum ExecutionMode {
    Sync { timeout: Duration },
    Async,
}

pub struct TaskHandle {
    pub id: TaskId,
    // ...
}

pub enum TaskStatus {
    // ...
}
```

**Why now:** Need these types before refactoring tool execution.

---

### 3.2 Session Overlay Types

**For:** GAP-003 (Session Overlays)

**Create:** `src/session/overlay.rs`

```rust
/// Base types for overlay architecture
pub trait SessionOverlay: Send + Sync {
    fn overlay_type(&self) -> OverlayType;
    fn persist(&self) -> bool;
}

pub struct ChannelOverlay { ... }
pub struct SpawnOverlay { ... }
```

**Why now:** Need type definitions before refactoring SimpleSession.

---

### 3.3 Event System Foundation

**For:** GAP-004 (Event Router)

**Create:** `src/orchestration/events.rs`

```rust
/// System event types
pub enum SystemEvent {
    File { ... },
    Webhook { ... },
    Internal { ... },
    Timer { ... },
}
```

**Why now:** Need event types before building router.

---

## 4. DEPENDENCY CLEANUP

### 4.1 Review Heavy Dependencies

**File:** `Cargo.toml`

| Dependency | Purpose | Action |
|------------|---------|--------|
| `scraper` (HTML parsing) | web_search tool | Move to MCP |
| `readability` | web content extraction | Move to MCP |
| `aes-gcm` | portable agent encryption | Keep |
| `argon2` | password hashing | Keep |
| `rpassword` | CLI password input | Keep |
| `keyring` | system keychain | Keep |

**Target:** Remove web-related deps from core (they go to MCP).

---

### 4.2 Feature Flags

**Add feature flags for optional components:**

```toml
[features]
default = ["cli", "http-channel"]
cli = []
http-channel = []
daemon = ["cron", "sqlite"]
gateway = ["libloading"]
full = ["cli", "http-channel", "daemon", "gateway", "all-providers"]

# Providers
all-providers = ["openai", "anthropic", "kimi", ...]
openai = []
anthropic = []
kimi = []
```

**Why:** Supports minimal core goal (~2MB).

---

## 5. IMPLEMENTATION ORDER

### Phase 1: Cleanup (Week 1)
1. Remove global `#[allow(dead_code)]`
2. Delete duplicate `src/agent/channels/`
3. Rename conflicting SessionRegistry types
4. Consolidate duplicate types

### Phase 2: Restructure (Week 1-2)
1. Create `src/orchestration/` module
2. Move cron/scheduler
3. Refactor `src/agent/` module structure
4. Clean up Tool trait

### Phase 3: Foundation Types (Week 2)
1. Add async execution primitives
2. Add session overlay types
3. Add event system types
4. These prepare for gap implementation

---

## 6. SUCCESS CRITERIA

- [ ] No global `#![allow(dead_code)]` in lib.rs
- [ ] Only one channel system (no duplicates)
- [ ] Clear, non-conflicting type names
- [ ] `src/orchestration/` exists with scheduler
- [ ] Tool trait cleaned up
- [ ] All tests pass
- [ ] `cargo clippy` clean (no warnings)
- [ ] Feature flags for optional components

---

## 7. RELATED DOCUMENTS

- [Architecture Gaps Index](./index.md)
- [GRAND_ARCHITECTURE.md](../GRAND_ARCHITECTURE.md)
