# GAP-001: MCP (Model Context Protocol) Support

**Priority:** 🔴 Critical  
**Status:** Open  
**Target:** v0.5.0  
**Est. Effort:** 2-3 weeks  

---

## Problem Statement

The Grand Architecture specifies a three-tier capability model where MCPs (Model Context Protocols) provide bundled, stateful services. Currently, Pekobot has **zero MCP implementation** - all capabilities are either built-in tools or not implemented.

This blocks the architecture's goal of:
- Externalizing complex capabilities (browser, database, email, memory)
- Keeping the core minimal (~2MB)
- Enabling pluggable capability ecosystems

---

## Current State

```rust
// Current: Tools are hardcoded in ToolFactory
tools.push(Arc::new(BrowserTool::new(vec![], None)));
tools.push(Arc::new(WebSearchTool::new(ws_config)));
// ... all built-in
```

Grep results: `MCP|mcp` → 0 matches

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.5.2](../GRAND_ARCHITECTURE.md#452-mcps-bundled-stateful):

```rust
// Target: MCPs are external services discovered via registry
let mcp_client = McpClient::connect("mcp://localhost:8080").await?;
let tools = mcp_client.list_tools().await?;
```

---

## Scope

### In Scope
- MCP client implementation (stdio + SSE transports)
- MCP server lifecycle management (start/stop/restart)
- Tool discovery from MCP servers
- Resource management (MCP resources)
- Prompt templates from MCP servers
- Basic MCP registry (local config)

### Out of Scope (Future)
- Remote MCP registry (Pekohub integration)
- MCP server sandboxing
- MCP authentication/authorization

---

## Goals

1. **MCP Client Core**: Implement MCP protocol client supporting stdio and SSE transports
2. **MCP Manager**: Lifecycle management for MCP servers (start, stop, health check)
3. **Tool Proxy**: Expose MCP tools as Pekobot Tool trait
4. **Migration Path**: Move current built-ins to MCPs:
   - `BrowserTool` → `mcp-browser`
   - `WebSearchTool` → `mcp-web`
   - `MemoryTool` → `mcp-memory-*`

---

## Proposed Implementation

### Phase 1: MCP Client Foundation
```rust
pub struct McpClient {
    transport: Box<dyn McpTransport>,
    server_info: ServerInfo,
}

#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn send(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse>;
    async fn close(&self) -> Result<()>;
}

pub struct StdioTransport { /* ... */ }
pub struct SseTransport { /* ... */ }
```

### Phase 2: MCP Manager
```rust
pub struct McpManager {
    servers: HashMap<String, McpServerHandle>,
    tool_proxy: Arc<dyn Tool>, // Exposes MCP tools as Pekobot tools
}

impl McpManager {
    pub async fn start_server(&self, config: McpServerConfig) -> Result<()>;
    pub async fn stop_server(&self, name: &str) -> Result<()>;
    pub fn list_tools(&self) -> Vec<McpTool>;
}
```

### Phase 3: Integration
```toml
# config.toml
[[mcps]]
name = "browser"
command = "mcp-browser"
args = ["--headless"]

[[mcps]]
name = "memory"
command = "mcp-memory-sqlite"
args = ["--db", "~/.pekobot/memory.db"]
```

---

## Dependencies

- **Blocks:** GAP-008 (Tool Migration)
- **Related to:** GAP-002 (Async execution for MCP calls)

---

## Success Criteria

- [ ] Can connect to an MCP server via stdio
- [ ] Can discover and invoke tools from MCP server
- [ ] Can list MCP resources
- [ ] Can use prompt templates from MCP
- [ ] Browser functionality works via MCP
- [ ] At least one memory backend works via MCP

---

## References

- [MCP Specification](https://modelcontextprotocol.io/specification/)
- [GRAND_ARCHITECTURE.md - MCPs](../GRAND_ARCHITECTURE.md#452-mcps-bundled-stateful)
- Current tool factory: `src/tools/factory.rs`
