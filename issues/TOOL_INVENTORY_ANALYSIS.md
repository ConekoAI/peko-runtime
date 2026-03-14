# Built-in Tools Inventory Analysis

**Date:** 2026-03-14  
**Purpose:** Compare provisioned tools (GRAND_ARCHITECTURE.md) vs actual implementation

---

## 1. GRAND_ARCHITECTURE.md Specification

### Three-Tier Capability Model

| Tier | Scope | Examples | Source |
|------|-------|----------|--------|
| **Tools** | Atomic, stateless | `read`, `write`, `agent_send`, `agent_spawn` | Built-in, package |
| **MCPs** | Bundled, stateful | `browser`, `database`, `memory-*` | Package, shared |
| **Skills** | Workflows | `coding_assistant`, `research_pipeline` | Package |

### Built-in Core Primitives (Specified)

From GRAND_ARCHITECTURE.md section 4.5:
- `read` - Filesystem read
- `write` - Filesystem write  
- `agent_send` - Agent messaging
- `agent_spawn` - Agent spawning
- Filesystem operations

---

## 2. Current Built-in Tools Inventory

### 2.1 Essential/Primitive Tools (✅ Aligned)

| Tool | File | Lines | Purpose | Status |
|------|------|-------|---------|--------|
| **FileSystemTool** | filesystem.rs | 461 | read, write, edit files | ✅ Core primitive |
| **AgentInvokeTool** | agent_invoke.rs | 901 | agent_send (A2A messaging) | ✅ Core primitive |
| **AgentSpawnToolV2** | agent_spawn_v2.rs | 508 | agent_spawn | ✅ Core primitive |
| **ProcessTool** | process.rs | 509 | Shell execution | ✅ Core primitive |
| **ApplyPatchTool** | apply_patch.rs | 634 | Search/replace edit | ✅ Essential |

**Subtotal:** 5 tools, ~3,013 lines

### 2.2 Session Management Tools (✅ Aligned)

| Tool | File | Lines | Purpose | Status |
|------|------|-------|---------|--------|
| **SessionsListTool** | session_introspection.rs | 611 | List sessions | ✅ Session mgmt |
| **SessionsHistoryTool** | session_introspection.rs | (shared) | Session history | ✅ Session mgmt |
| **SessionStatusTool** | session_introspection.rs | (shared) | Session status | ✅ Session mgmt |
| **SessionMessagingTool** | session_messaging.rs | 276 | Cross-session messaging | ✅ Session mgmt |

**Subtotal:** 4 tools, ~611 lines (shared file)

### 2.3 Agent Management Tools (✅ Aligned)

| Tool | File | Lines | Purpose | Status |
|------|------|-------|---------|--------|
| **AgentManagement** (multiple) | agent_management.rs | 497 | List, info, broadcast | ✅ Agent mgmt |
| **AgentSpawnTool** | agent_management.rs | (shared) | Legacy spawn | ✅ Agent mgmt |

**Subtotal:** 2 tools, ~497 lines

### 2.4 Scheduling Tools (✅ Aligned)

| Tool | File | Lines | Purpose | Status |
|------|------|-------|---------|--------|
| **CronTool** | cron_tool.rs | 557 | Scheduled jobs | ✅ Essential |

**Subtotal:** 1 tool, ~557 lines

### 2.5 Heavy/Web Tools (⚠️ Should be MCP per Architecture)

| Tool | File | Lines | Dependencies | Status |
|------|------|-------|--------------|--------|
| **WebSearchTool** | web_search.rs | 444 | scraper, readability | ⚠️ Heavy |
| **FetchTool** | fetch.rs | 773 | scraper, readability | ⚠️ Heavy |
| **BrowserTool** | browser.rs | 638 | Playwright (external) | ⚠️ Heavy |
| **HttpTool** | http.rs | 252 | reqwest | ⚠️ Medium |

**Subtotal:** 4 tools, ~2,107 lines, **heavy dependencies**

### 2.6 Memory Tools (⚠️ Should be MCP per Architecture)

| Tool | File | Lines | Dependencies | Status |
|------|------|-------|--------------|--------|
| **MemoryTool** | memory_tool.rs | 305 | rusqlite | ⚠️ Stateful |

**Subtotal:** 1 tool, ~305 lines

### 2.7 Utility/Demo Tools (✅ Optional)

| Tool | File | Lines | Purpose | Status |
|------|------|-------|---------|--------|
| **MessageTool** | message_tool.rs | 421 | Channel messaging | ✅ Utility |
| **ProgressDemoTool** | progress_demo.rs | 317 | Progress demo | ✅ Demo only |

**Subtotal:** 2 tools, ~738 lines

---

## 3. Comparison Summary

### Tools by Architecture Alignment

```
┌────────────────────────────────────────────────────────────────┐
│                    CURRENT TOOL ARCHITECTURE                    │
├────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ✅ CORE PRIMITIVES (Keep in Core)                             │
│  ├─ FileSystemTool (read/write/edit)                           │
│  ├─ ProcessTool (shell execution)                              │
│  ├─ AgentInvokeTool (agent_send)                               │
│  ├─ AgentSpawnToolV2 (agent_spawn)                             │
│  └─ ApplyPatchTool (search/replace)                            │
│                                                                 │
│  ✅ SESSION & AGENT MGMT (Keep in Core)                        │
│  ├─ Sessions*Tool (introspection)                              │
│  ├─ SessionMessagingTool                                       │
│  ├─ AgentManagement*                                           │
│  └─ CronTool (scheduling)                                      │
│                                                                 │
│  ⚠️  HEAVY TOOLS (Migrate to MCP per GRAND_ARCHITECTURE)       │
│  ├─ WebSearchTool ───────┐                                     │
│  ├─ FetchTool ───────────┼──→ MCP: web                        │
│  ├─ HttpTool ────────────┘                                     │
│  ├─ BrowserTool ───────────→ MCP: browser                      │
│  └─ MemoryTool ────────────→ MCP: memory                       │
│                                                                 │
│  ✅ OPTIONAL UTILITIES                                         │
│  ├─ MessageTool (channel-specific)                             │
│  └─ ProgressDemoTool (demo)                                    │
│                                                                 │
└────────────────────────────────────────────────────────────────┘
```

---

## 4. Gap Analysis

### 4.1 Tools Fully Aligned ✅

All core primitives are implemented:
- ✅ `read`/`write` → FileSystemTool
- ✅ `agent_send` → AgentInvokeTool  
- ✅ `agent_spawn` → AgentSpawnToolV2
- ✅ Shell execution → ProcessTool
- ✅ Session management → Sessions*Tool
- ✅ Scheduling → CronTool

### 4.2 Tools Misaligned ⚠️

Per GRAND_ARCHITECTURE.md, these should **NOT** be built-in:

| Tool | Current | Target | Reason |
|------|---------|--------|--------|
| WebSearchTool | Built-in | MCP `web` | Heavy deps (scraper, readability) |
| FetchTool | Built-in | MCP `web` | Heavy deps (scraper, readability) |
| HttpTool | Built-in | MCP `web` | Network I/O, not primitive |
| BrowserTool | Built-in | MCP `browser` | External Chrome/Playwright |
| MemoryTool | Built-in | MCP `memory-*` | Stateful, multiple backends |

### 4.3 Missing Tools ❌

| Tool | Priority | Description |
|------|----------|-------------|
| `write` (explicit) | Low | FileSystemTool has write, but not explicit `write` tool |
| `edit` (explicit) | Low | ApplyPatchTool provides this, but named differently |

---

## 5. Dependency Analysis

### Heavy Dependencies to Extract

```toml
# Current in core Cargo.toml
scraper = "0.19"        # HTML parsing (web_search, fetch)
readability = "0.3"     # Text extraction (web_search, fetch)

# Size impact:
# - scraper + readability + deps: ~2-3MB
# - Without them: ~5-6MB reduction
```

### Core Dependencies (Keep)

```toml
# Essential for core primitives
rusqlite = { version = "0.30", features = ["bundled"] }  # Session storage
tokio = { version = "1.35", features = ["full"] }        # Async runtime
serde = { version = "1.0", features = ["derive"] }       # Serialization
```

---

## 6. Recommendation

### Option A: Full Architecture Compliance (Recommended)

**Migrate to MCPs:**
1. **mcp-web**: WebSearchTool + FetchTool + HttpTool
2. **mcp-browser**: BrowserTool
3. **mcp-memory**: MemoryTool

**Core keeps:**
- FileSystemTool
- ProcessTool
- AgentInvokeTool
- AgentSpawnToolV2
- ApplyPatchTool
- CronTool
- Session* tools
- AgentManagement tools

**Benefits:**
- Binary size: ~8MB → ~3MB
- Faster compilation
- Plugin architecture
- User choice

### Option B: Minimal Core + Optional Features

Add Cargo features:
```toml
[features]
default = ["core-tools"]
core-tools = []  # Only primitives
web-tools = ["scraper", "readability"]  # Optional
browser-tool = []  # Optional
memory-tool = []  # Optional (uses sqlite)
```

**Benefits:**
- Build-time flexibility
- Air-gapped environments can embed tools
- Smaller minimal builds

### Option C: Status Quo

Keep all tools built-in.

**Downsides:**
- Violates GRAND_ARCHITECTURE
- Bloated binary
- Slow compilation
- Harder to maintain

---

## 7. Tool Factory Configuration Matrix

```rust
/// Current: All tools built-in
ToolFactoryConfig {
    enable_filesystem: true,      // ✅ Core
    enable_process: true,         // ✅ Core
    enable_apply_patch: true,     // ✅ Core
    
    // These should be MCP-based per architecture:
    enable_http: true,            // ⚠️ MCP candidate
    enable_fetch: true,           // ⚠️ MCP candidate
    enable_browser: true,         // ⚠️ MCP candidate
    enable_web_search: true,      // ⚠️ MCP candidate
    enable_memory: true,          // ⚠️ MCP candidate
    
    // These are session/agent management:
    enable_session_tools: true,   // ✅ Core
    enable_cron: true,            // ✅ Core
}
```

---

## 8. Action Items

### Immediate (No Code Changes)
- [ ] Document current tool architecture
- [ ] Add feature flags for heavy tools
- [ ] Update GRAND_ARCHITECTURE.md if needed

### Short Term (Week 1-2)
- [ ] Make scraper/readability optional dependencies
- [ ] Add `web-tools` Cargo feature
- [ ] Test minimal build without web tools

### Medium Term (Week 3-4)
- [ ] Create `mcp-web` server
- [ ] Migrate WebSearchTool, FetchTool, HttpTool
- [ ] Test MCP integration

### Long Term (v0.8.0)
- [ ] Create `mcp-browser` server
- [ ] Create `mcp-memory` server
- [ ] Full architecture compliance

---

## References

- [GRAND_ARCHITECTURE.md](../GRAND_ARCHITECTURE.md) - Official architecture spec
- [GAP-008-Implementation-Plan.md](./GAP-008-Implementation-Plan.md) - MCP migration plan
- [src/tools/](../src/tools/) - Current tool implementations
- [src/tools/factory.rs](../src/tools/factory.rs) - Tool creation logic
