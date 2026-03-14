# GAP-008: Tool Migration to MCPs - Implementation Plan

**Priority:** 🟡 Medium  
**Status:** Planning  
**Target:** v0.7.0  
**Est. Effort:** 2-3 weeks  
**Last Updated:** 2026-03-14

---

## Reassessment Summary

### Current State (Post GAP-001)

✅ **MCP Infrastructure Complete**
- MCP Client, Manager, Transport all implemented
- Tool proxy system working
- CLI commands for MCP management
- Stdio and SSE transport supported

✅ **Tools Currently in Core (~2,400 lines)**
| Tool | Lines | Dependencies | Weight |
|------|-------|--------------|--------|
| `fetch` | 773 | reqwest, readability, scraper | Heavy |
| `browser` | 638 | (Playwright integration) | Heavy |
| `web_search` | 444 | scraper, readability | Medium |
| `memory_tool` | 305 | rusqlite | Medium |
| `http` | 252 | reqwest | Light |

✅ **Heavy Dependencies to Extract**
- `scraper = "0.19"` - HTML parsing (web_search, fetch)
- `readability = "0.3"` - Text extraction (web_search, fetch)

### Goal Achievement

**Current binary size:** ~8-10MB (estimate with all dependencies)  
**Target binary size:** ~2-3MB (minimal core)  
**Size reduction:** ~60-70%

---

## Implementation Strategy

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                    Pekobot Core (~2MB)                          │
├─────────────────────────────────────────────────────────────────┤
│  Essential Tools:                                               │
│  • filesystem (read/write/edit)                                │
│  • process (shell execution)                                   │
│  • apply_patch (search/replace)                                │
│  • agent_invoke (A2A messaging)                                │
│  • agent_spawn (subagent creation)                             │
│  • cron_tool (scheduling)                                      │
│  • session_* (introspection)                                   │
├─────────────────────────────────────────────────────────────────┤
│  MCP Manager (already implemented)                             │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ stdio/sse
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                 MCP Servers (separate processes)                │
├──────────────────────┬──────────────────┬───────────────────────┤
│     mcp-web          │   mcp-browser    │    mcp-memory         │
│  (~3MB with deps)    │ (~5MB with deps) │  (~2MB with deps)     │
├──────────────────────┼──────────────────┼───────────────────────┤
│  • web_search        │  • browser_nav   │  • memory_store       │
│  • fetch             │  • browser_click │  • memory_recall      │
│  • http              │  • browser_eval  │  • memory_search      │
└──────────────────────┴──────────────────┴───────────────────────┘
```

---

## Phase 1: Create MCP Servers (Week 1)

### 1.1 MCP Web Server

**Location:** `mcp-servers/mcp-web/`  
**Purpose:** Web search, HTTP requests, content fetching

**Structure:**
```
mcp-servers/mcp-web/
├── Cargo.toml
├── src/
│   ├── main.rs          # MCP server entry
│   ├── tools/
│   │   ├── mod.rs
│   │   ├── web_search.rs  # Extracted from pekobot
│   │   ├── fetch.rs       # Extracted from pekobot
│   │   └── http.rs        # Extracted from pekobot
│   └── lib.rs
└── README.md
```

**Cargo.toml:**
```toml
[package]
name = "mcp-web"
version = "1.0.0"
edition = "2021"

[dependencies]
# MCP SDK
mcp-sdk = { path = "../../mcp-sdk" }  # Or published crate

# Web dependencies (moved from pekobot core)
reqwest = { version = "0.11", features = ["json", "stream"] }
scraper = "0.19"
readability = "0.3"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.35", features = ["full"] }
anyhow = "1.0"
tracing = "0.1"
url = "2.5"
```

**Tools Provided:**
| Tool | Description | Schema |
|------|-------------|--------|
| `web_search` | Search web with Brave/DuckDuckGo | `{ query, count }` |
| `fetch` | Fetch and extract webpage content | `{ url, extract_text }` |
| `http` | General HTTP requests | `{ method, url, headers, body }` |

**Implementation Notes:**
- Extract existing code from `pekobot/src/tools/web_search.rs`, `fetch.rs`, `http.rs`
- Adapt to MCP tool interface
- Keep all existing functionality
- Add proper error handling for MCP protocol

### 1.2 MCP Browser Server

**Location:** `mcp-servers/mcp-browser/`  
**Purpose:** Browser automation via Playwright

**Structure:**
```
mcp-servers/mcp-browser/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── tools/
│   │   ├── mod.rs
│   │   └── browser.rs    # Extracted from pekobot
│   └── lib.rs
└── README.md
```

**Tools Provided:**
| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to URL |
| `browser_click` | Click element |
| `browser_type` | Type text into input |
| `browser_eval` | Evaluate JavaScript |
| `browser_extract` | Extract data from page |
| `browser_screenshot` | Take screenshot |

**Note:** Browser tool requires Playwright/Chrome installation. This is the heaviest MCP.

### 1.3 MCP Memory Server

**Location:** `mcp-servers/mcp-memory-sqlite/`  
**Purpose:** SQLite-based memory with embeddings

**Tools Provided:**
| Tool | Description |
|------|-------------|
| `memory_store` | Store information |
| `memory_recall` | Recall by ID |
| `memory_search` | Semantic search |
| `memory_delete` | Delete entry |
| `memory_list` | List all entries |

**Implementation:**
- Use extracted `pekobot/src/memory/` code
- Support multiple backends via feature flags (SQLite, Chroma, etc.)

---

## Phase 2: Core Refactoring (Week 1-2)

### 2.1 Make Dependencies Optional

**Cargo.toml changes:**
```toml
[features]
default = ["embedded-tools"]
embedded-tools = ["web-tools", "browser-tool", "memory-tool"]
web-tools = ["scraper", "readability"]
browser-tool = ["playwright"]  # if we add Rust playwright binding
memory-tool = []
minimal = []  # No embedded tools, MCP only

[dependencies]
# Optional web dependencies
scraper = { version = "0.19", optional = true }
readability = { version = "0.3", optional = true }
```

### 2.2 Tool Factory Refactoring

**Current:**
```rust
// Always creates built-in tools
pub fn create_tools(config: &ToolFactoryConfig) -> Vec<Arc<dyn Tool>> {
    tools.push(Arc::new(WebSearchTool::new()));  // Built-in
    tools.push(Arc::new(FetchTool::new()));      // Built-in
}
```

**New:**
```rust
pub async fn create_tools(config: &ToolFactoryConfig) -> Vec<Arc<dyn Tool>> {
    let mut tools = create_essential_tools(config);
    
    // Try MCP first, fallback to embedded if available
    if config.use_mcp {
        match load_mcp_web_tools().await {
            Ok(mcp_tools) => tools.extend(mcp_tools),
            Err(e) => {
                tracing::warn!("MCP web tools unavailable: {}", e);
                #[cfg(feature = "web-tools")]
                tools.extend(create_embedded_web_tools(config));
            }
        }
    } else {
        #[cfg(feature = "web-tools")]
        tools.extend(create_embedded_web_tools(config));
    }
    
    tools
}

fn create_essential_tools(config: &ToolFactoryConfig) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(FileSystemTool::new()),
        Arc::new(ProcessTool::new()),
        Arc::new(ApplyPatchTool::new()),
        Arc::new(AgentInvokeTool::new()),
        Arc::new(AgentSpawnTool::new()),
        Arc::new(CronTool::new()),
        // ... only essential tools
    ]
}
```

### 2.3 MCP Auto-Discovery

**New functionality:**
```rust
impl ToolFactory {
    /// Auto-discover and connect to MCP servers
    pub async fn discover_mcps(&mut self) -> Result<()> {
        let mcp_config = self.load_mcp_config()?;
        
        for (name, server_config) in mcp_config.servers {
            match self.connect_mcp(&name, &server_config).await {
                Ok(tools) => {
                    tracing::info!("Connected to MCP server '{}' with {} tools", name, tools.len());
                    self.mcp_tools.extend(tools);
                }
                Err(e) => {
                    tracing::warn!("Failed to connect to MCP server '{}': {}", name, e);
                    // Try auto-install
                    if self.config.auto_install_mcp {
                        self.install_and_start_mcp(&name).await?;
                    }
                }
            }
        }
        
        Ok(())
    }
}
```

---

## Phase 3: Migration Path (Week 2)

### 3.1 User Migration Guide

**For existing users with built-in tools:**

1. **Before upgrade:** Tools work as before (embedded)
2. **During upgrade:** Install MCP servers
   ```bash
   # Install MCP web server
   pekobot mcp install web
   
   # Verify installation
   pekobot mcp list
   # Should show: web [installed] [running]
   ```
3. **After upgrade:** MCP servers provide tools

**Migration command:**
```bash
# One-command migration
pekobot migrate-to-mcp

# This will:
# 1. Check current tool usage
# 2. Install required MCP servers
# 3. Update agent configurations
# 4. Verify everything works
```

### 3.2 Backward Compatibility

**During transition (v0.7.0):**
- Default: Use MCP if available, fallback to embedded
- Warning logged when using embedded tools
- Deprecation notice in docs

**Code:**
```rust
pub async fn get_web_tools(&self) -> Vec<Arc<dyn Tool>> {
    // 1. Try MCP first
    if let Some(mcp) = self.mcp_manager.get_client("web") {
        return mcp.list_tools().await;
    }
    
    // 2. Fallback to embedded (with warning)
    #[cfg(feature = "web-tools")]
    {
        tracing::warn!(
            "Using embedded web tools. Consider installing mcp-web: \" \
            "pekobot mcp install web"
        );
        return create_embedded_web_tools();
    }
    
    // 3. No tools available
    tracing::error!("Web tools not available. Install mcp-web or enable web-tools feature");
    vec![]
}
```

### 3.3 Configuration Migration

**Old config:**
```toml
[tools]
web_search = true
fetch = true
browser = true
```

**New config:**
```toml
[tools]
mode = "mcp"  # or "embedded" or "auto"

[tools.mcp]
auto_install = true

[[tools.mcp.servers]]
name = "web"
enabled = true

[[tools.mcp.servers]]
name = "browser"
enabled = true
```

---

## Phase 4: Distribution (Week 3)

### 4.1 MCP Server Distribution

**Option A: Separate crates.io packages**
```bash
cargo install mcp-web
cargo install mcp-browser
cargo install mcp-memory
```

**Option B: Bundled with pekobot**
```
pekobot/                              
├── Cargo.toml                        
├── src/                              
└── mcp-servers/                      
    ├── mcp-web/Cargo.toml           
    ├── mcp-browser/Cargo.toml       
    └── mcp-memory/Cargo.toml        
```

**Recommended:** Option B for v0.7.0, migrate to Option A in v0.8.0

### 4.2 Installation Methods

**Automatic (recommended):**
```bash
pekobot mcp install web
# Downloads, builds, and configures mcp-web
```

**Manual:**
```bash
cd mcp-servers/mcp-web
cargo build --release
# Copy binary to ~/.local/bin/
# Update pekobot config
```

---

## Implementation Timeline

| Week | Task | Deliverable |
|------|------|-------------|
| **Week 1** | Create MCP servers | `mcp-web`, `mcp-browser`, `mcp-memory` crates |
| **Week 1** | Extract tools | Move tool code from pekobot to MCP servers |
| **Week 2** | Core refactoring | Optional dependencies, feature flags |
| **Week 2** | Tool factory | MCP discovery, fallback logic |
| **Week 2** | Migration path | `migrate-to-mcp` command |
| **Week 3** | Testing | E2E tests for MCP integration |
| **Week 3** | Documentation | Migration guide, API docs |

---

## Success Criteria

- [ ] MCP `web` server provides `web_search`, `fetch`, `http` tools
- [ ] MCP `browser` server provides browser automation
- [ ] MCP `memory` server provides memory operations
- [ ] Core compiles without `scraper` and `readability` (minimal feature)
- [ ] Binary size reduced by >50% (from ~8MB to ~3MB)
- [ ] Existing agents work without modification (backward compatible)
- [ ] Migration command works: `pekobot migrate-to-mcp`
- [ ] Auto-installation works for missing MCPs
- [ ] All tests pass with both embedded and MCP modes

---

## Risks and Mitigation

| Risk | Impact | Mitigation |
|------|--------|------------|
| MCP server startup latency | Medium | Implement connection pooling, lazy loading |
| Breaking existing workflows | High | Maintain backward compatibility, deprecation warnings |
| Complex installation | Medium | Provide `migrate-to-mcp` command, clear docs |
| MCP server crashes | Medium | Implement health checks, automatic restart |
| Feature parity | Low | Test all tool functionality in both modes |

---

## Testing Strategy

### Unit Tests
```rust
#[test]
fn test_mcp_web_search() {
    let mcp = McpWebServer::new();
    let result = mcp.call_tool("web_search", json!({"query": "rust"})).unwrap();
    assert!(result.get("results").is_some());
}
```

### Integration Tests
```bash
# Test with MCP
cargo test --features mcp-only

# Test with embedded
cargo test --features embedded-tools

# Test migration
./test_scripts/test_mcp_migration.sh
```

### E2E Tests
```bash
# Full workflow test
pekobot mcp install web
pekobot agent create test-agent
pekobot agent start test-agent -M "Search for rust programming"
```

---

## References

- [GAP-001: MCP Support](./GAP-001-mcp-support.md) - MCP infrastructure
- [MCP Specification](https://modelcontextprotocol.io/) - Official MCP spec
- [GRAND_ARCHITECTURE.md - Tools](../GRAND_ARCHITECTURE.md#451-tools-atomic-stateless)
- Current tools: `src/tools/`
- MCP implementation: `src/mcp/`
