# GAP-008: Tool Migration to Registry/MCPs

**Priority:** 🟡 Medium  
**Status:** Open  
**Target:** v0.7.0  
**Est. Effort:** 2-3 weeks  

---

## Problem Statement

The Grand Architecture specifies a minimal core with tools externalized to MCPs. Currently, many heavy tools are built into the core:

| Tool | Current | Target |
|------|---------|--------|
| `web_search` | Built-in | MCP `web` |
| `fetch`/`http` | Built-in | MCP `web` |
| `browser` | Built-in | MCP `browser` |
| `memory_*` | Built-in (disabled) | MCP `memory-*` |
| `tts`, `image` | Not present | MCP `media` |

This bloats the core and violates the ~2MB goal.

---

## Current State

```rust
// src/tools/factory.rs - All built-in
impl ToolFactory {
    pub fn create_tools(config: &ToolFactoryConfig) -> Vec<Arc<dyn Tool>> {
        tools.push(Arc::new(WebSearchTool::new(ws_config)));
        tools.push(Arc::new(BrowserTool::new(vec![], None)));
        tools.push(Arc::new(FetchTool::new(fetch_config)));
        tools.push(Arc::new(HttpTool::new()?));
        // ... all in core
    }
}
```

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.5.1](../GRAND_ARCHITECTURE.md#451-tools-atomic-stateless):

```rust
// Minimal core tools only
pub fn create_essential_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTool),    // Filesystem read
        Arc::new(WriteTool),   // Filesystem write
        Arc::new(EditTool),    // Search/replace edit
        Arc::new(ExecTool),    // Shell execution
        Arc::new(ProcessTool), // Process management
        Arc::new(ApplyPatchTool),
        Arc::new(AgentSendTool),   // Agent messaging
        Arc::new(AgentSpawnTool),  // Agent spawning
        Arc::new(CronTool),        // Scheduling
        Arc::new(SessionListTool), // Session introspection
        // Everything else via MCP
    ]
}
```

---

## Scope

### In Scope
- Extract `web_search` to MCP
- Extract `browser` to MCP
- Extract `fetch`/`http` to MCP `web`
- Extract memory tools to MCP `memory-*`
- Migration guide for users

### Out of Scope (Future)
- `media` MCP (TTS, image generation)
- Gateway/Node tools to infrastructure MCP
- Platform-specific actions to channels

---

## Goals

1. **Web MCP**: Combine `web_search`, `fetch`, `http` into single MCP
2. **Browser MCP**: Extract browser automation
3. **Memory MCPs**: Multiple memory backend options
4. **Minimal Core**: Core only has essential primitives
5. **Backward Compatibility**: Smooth migration path

---

## Proposed Implementation

### MCP: web
```toml
# mcp-web/manifest.toml
[mcp]
name = "web"
version = "1.0.0"
description = "Web search and HTTP capabilities"

[[tools]]
name = "web_search"
description = "Search the web"

[[tools]]
name = "fetch"
description = "Fetch a URL"

[[tools]]
name = "http"
description = "Make HTTP requests"
```

```rust
// mcp-web/src/main.rs
async fn main() {
    let server = McpServer::new("web")
        .with_tool(WebSearchTool::new())
        .with_tool(FetchTool::new())
        .with_tool(HttpTool::new());

    server.run_stdio().await;
}
```

### MCP: browser
```toml
# mcp-browser/manifest.toml
[mcp]
name = "browser"
version = "1.0.0"
description = "Browser automation via headless Chrome"

[[tools]]
name = "browser_navigate"
description = "Navigate to URL"

[[tools]]
name = "browser_click"
description = "Click element"

[[tools]]
name = "browser_extract"
description = "Extract data from page"
```

### MCP: memory-sqlite
```toml
# mcp-memory-sqlite/manifest.toml
[mcp]
name = "memory-sqlite"
version = "1.0.0"
description = "SQLite-based memory with embeddings"

[[tools]]
name = "memory_store"
description = "Store information in memory"

[[tools]]
name = "memory_recall"
description = "Recall information from memory"

[[tools]]
name = "memory_search"
description = "Semantic search in memory"
```

### Migration Path

#### Phase 1: Create MCPs (side-by-side)
- Create MCP implementations
- Keep built-ins as fallback
- Add deprecation warnings

#### Phase 2: Deprecate built-ins
```rust
// In tool factory
if config.enable_web_search {
    // Try MCP first
    if let Some(mcp_client) = mcp_manager.get_client("web") {
        tools.extend(mcp_client.list_tools().await?);
    } else {
        // Fallback to built-in with warning
        warn!("Using deprecated built-in web_search. Install mcp-web for better performance.");
        tools.push(Arc::new(WebSearchTool::new()));
    }
}
```

#### Phase 3: Remove built-ins
- Remove from core
- Install via registry
- Update templates

### Auto-Installation
```rust
// When agent config requests web_search but MCP not installed
pub async fn ensure_mcp(&self, mcp_name: &str) -> Result<McpClient> {
    if let Some(client) = self.mcp_manager.get_client(mcp_name) {
        return Ok(client);
    }

    // Auto-install from registry
    if self.config.auto_install_mcp {
        self.registry.install(&format!("mcp-{}", mcp_name)).await?;
        self.mcp_manager.start_server(mcp_name).await?;
    }

    self.mcp_manager.get_client(mcp_name)
        .ok_or("MCP not available")
}
```

---

## Dependencies

- **Blocked by:** GAP-001 (MCP support required)
- **Related to:** GAP-010 (Channel plugins follow same pattern)

---

## Success Criteria

- [ ] MCP `web` provides search/fetch/http
- [ ] MCP `browser` provides automation
- [ ] MCP `memory-sqlite` provides memory
- [ ] Core size reduced by removing these tools
- [ ] Existing agents work with migrated tools
- [ ] Auto-installation works for missing MCPs

---

## References

- [GRAND_ARCHITECTURE.md - Not Built-in](../GRAND_ARCHITECTURE.md#451-tools-atomic-stateless)
- [GRAND_ARCHITECTURE.md - MCPs](../GRAND_ARCHITECTURE.md#452-mcps-bundled-stateful)
- Current tools: `src/tools/`
