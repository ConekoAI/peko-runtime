# GAP-001: MCP (Model Context Protocol) Support

**Priority:** 🔴 Critical  
**Status:** ✅ **Completed** (2026-03-11)  
**Target:** v0.5.0  
**Est. Effort:** 2-3 weeks → **Actual: ~3 days**  

---

## Problem Statement

The Grand Architecture specifies a three-tier capability model where MCPs (Model Context Protocols) provide bundled, stateful services. Previously, Pekobot had **zero MCP implementation** - all capabilities were either built-in tools or not implemented.

This blocked the architecture's goal of:
- Externalizing complex capabilities (browser, database, email, memory)
- Keeping the core minimal (~2MB)
- Enabling pluggable capability ecosystems

---

## Implementation Summary

### Delivered

| Component | Status | File(s) | Lines |
|-----------|--------|---------|-------|
| Core Types | ✅ Complete | `src/mcp/types.rs` | 588 |
| Transport Layer | ✅ Complete | `src/mcp/transport.rs` | 817 |
| MCP Client | ✅ Complete | `src/mcp/client.rs` | 559 |
| Configuration | ✅ Complete | `src/mcp/config.rs` | 371 |
| MCP Manager | ✅ Complete | `src/mcp/manager.rs` | 543 |
| Tool Proxy | ✅ Complete | `src/mcp/tool_proxy.rs` | 386 |
| CLI Commands | ✅ Complete | `src/commands/mcp.rs` | 524 |
| Integration Tests | ✅ Complete | `src/mcp/mod.rs` | 111 |
| **Total** | | | **~3,900** |

### Features Implemented

- ✅ **Stdio Transport** - Local subprocess communication with JSON-RPC over stdin/stdout
- ✅ **SSE Transport** - HTTP+SSE for remote MCP servers with reconnection
- ✅ **In-Memory Transport** - For testing (paired channels)
- ✅ **MCP Client** - Full protocol implementation with:
  - Initialize handshake with version negotiation
  - Tool discovery (`tools/list`)
  - Tool invocation (`tools/call`)
  - Resource listing and reading
  - Prompt listing and retrieval
  - Capability checking
- ✅ **MCP Manager** - Lifecycle management:
  - Start/stop/restart servers
  - Health monitoring with automatic reconnection
  - Auto-start on initialization
  - Connection pooling
- ✅ **Tool Proxy** - Seamless integration with Pekobot's `Tool` trait
- ✅ **TOML Configuration** - Persistent server configuration
- ✅ **CLI Commands** - Full server management:
  - `pekobot mcp list` - List configured servers
  - `pekobot mcp show <name>` - Show server details
  - `pekobot mcp add <name> ...` - Add a new server
  - `pekobot mcp remove <name>` - Remove a server
  - `pekobot mcp start|stop|restart <name>` - Lifecycle control
  - `pekobot mcp test <name>` - Test connection
  - `pekobot mcp tools` - List available tools
  - `pekobot mcp config` - View/edit configuration

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Pekobot Agent                           │
│                   (uses Vec<Arc<dyn Tool>>)                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      Tool Collection                        │
│  ┌─────────────────┐  ┌─────────────────────────────────┐   │
│  │ Built-in Tools  │  │ MCP Tool Proxies                │   │
│  │ - FileSystem    │  │ (dynamically discovered)        │   │
│  │ - Http          │  │                                 │   │
│  │ - Process       │  │                                 │   │
│  └─────────────────┘  └─────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      MCP Manager                            │
│         (lifecycle, health monitoring, routing)             │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌──────────────┐      ┌──────────────┐      ┌──────────────┐
│ MCP Client   │      │ MCP Client   │      │ MCP Client   │
│ (stdio)      │      │ (stdio)      │      │ (SSE)        │
└──────────────┘      └──────────────┘      └──────────────┘
        │                     │                     │
        ▼                     ▼                     ▼
┌──────────────┐      ┌──────────────┐      ┌──────────────┐
│ filesystem   │      │ browser      │      │ remote-tools │
│ server       │      │ server       │      │ server       │
└──────────────┘      └──────────────┘      └──────────────┘
```

### Test Results

```
Running 28 tests:
test mcp::types::tests::test_call_tool_request_serialization ... ok
test mcp::types::tests::test_client_capabilities_default ... ok
test mcp::types::tests::test_error_response ... ok
test mcp::types::tests::test_initialize_request ... ok
test mcp::types::tests::test_initialize_response_serialization ... ok
test mcp::types::tests::test_json_rpc_message_id_variants ... ok
test mcp::types::tests::test_list_tools_response_serialization ... ok
test mcp::client::tests::test_initialize_success ... ok
test mcp::client::tests::test_list_tools_success ... ok
test mcp::client::tests::test_call_tool_success ... ok
test mcp::client::tests::test_client_with_uninitialized ... ok
test mcp::transport::tests::test_in_memory_transport ... ok
test mcp::transport::tests::test_stdio_transport_mock ... ok
test mcp::config::tests::test_mcp_config_serialization ... ok
test mcp::config::tests::test_server_config_validation ... ok
test mcp::config::tests::test_default_values ... ok
test mcp::manager::tests::test_manager_new ... ok
test mcp::manager::tests::test_add_server ... ok
test mcp::manager::tests::test_get_client ... ok
test mcp::manager::tests::test_start_stop_server ... ok
test mcp::manager::tests::test_health_check ... ok
test mcp::tool_proxy::tests::test_tool_proxy_basic ... ok
test mcp::tool_proxy::tests::test_tool_proxy_with_args ... ok
test mcp::tool_proxy::tests::test_create_tool_proxies ... ok
test mcp::tool_proxy::tests::test_tool_with_context ... ok
test mcp::tool_proxy::tests::test_multiple_tools ... ok
test mcp::integration_tests::test_end_to_end_tool_flow ... ok
test mcp::integration_tests::test_server_lifecycle ... ok

test result: ok. 28 passed; 0 failed
```

### Configuration Example

```toml
# ~/.pekobot/mcp.toml
[[server]]
name = "filesystem"
transport = "stdio"
command = "mcp-filesystem-server"
args = ["/home/user/documents"]
auto_start = true

[[server]]
name = "remote-tools"
transport = "sse"
endpoint = "https://tools.example.com/mcp"
auto_start = false
```

### Usage Example

```rust
use pekobot::mcp::{McpConfig, McpManager, McpServerConfig};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load MCP configuration
    let mut config = McpConfig::new();
    config.add_server(McpServerConfig::stdio(
        "filesystem",
        "mcp-filesystem-server",
        vec!["/home/user/docs".to_string()],
    ));

    // Create and initialize manager
    let manager = Arc::new(RwLock::new(McpManager::new(config)));
    manager.read().await.init().await?;

    // Get tools (both built-in and MCP)
    let mut tools = pekobot::tools::ToolFactory::create_full_tools(".".into());
    let mcp_tools = manager.read().await.get_tools().await;
    tools.extend(mcp_tools);

    // Use with agent...
    
    // Shutdown
    manager.read().await.shutdown().await?;
    Ok(())
}
```

---

## Success Criteria

| Criterion | Status | Notes |
|-----------|--------|-------|
| Can connect to MCP server via stdio | ✅ | Full implementation with subprocess management |
| Can discover and invoke tools | ✅ | `list_tools()` and `call_tool()` working |
| Can list MCP resources | ✅ | `list_resources()` and `read_resource()` implemented |
| Can use prompt templates | ✅ | `list_prompts()` and `get_prompt()` implemented |
| Agent can use MCP tools via natural language | ✅ | Full integration with ToolFactory |
| Browser functionality via MCP | ✅ | Ready - requires external `mcp-browser` server |
| Memory backend via MCP | ✅ | Ready - requires external `mcp-memory-*` server |

---

## Quick Start / Integration Testing

### Full E2E Workflow

```bash
# 1. Install an MCP server
npm install -g @modelcontextprotocol/server-everything

# 2. Add to Pekobot (one command)
pekobot mcp add everything --transport stdio --command npx --args="-y" --args="@modelcontextprotocol/server-everything"

# 3. Use MCP tools via natural language (no manual server start needed!)
pekobot agent start myagent -M "Use the echo tool to say 'Hello World'"
pekobot agent start myagent -M "Use the add tool to calculate 23 + 19"
```

### Manual Testing

```bash
# Test connection
pekobot mcp test everything

# List available MCP tools
pekobot mcp tools
```

**Tools provided by server-everything:**
- `echo` - Echoes back input
- `add` - Adds two numbers
- `getTinyImage` - Returns a tiny image
- `longRunningOperation` - Tests progress notifications
- `samplesLLM` - Tests sampling capability
- `getResourceReference` - Tests resource references

### Other Official Test Servers

```bash
# Filesystem server
npm install -g @modelcontextprotocol/server-filesystem
pekobot mcp add fs stdio --command mcp-server-filesystem --args /home/user/docs

# GitHub server
npm install -g @modelcontextprotocol/server-github
pekobot mcp add github stdio --command mcp-server-github --env GITHUB_TOKEN=xxx

# PostgreSQL server
npm install -g @modelcontextprotocol/server-postgres
pekobot mcp add pg stdio --command mcp-server-postgres --args postgresql://localhost/db
```

### E2E Test Script

Run the full integration test:

```bash
./test_scripts/mcp/test_mcp_e2e.sh
```

---

## Documentation

- **User Guide**: `docs/MCP.md` - Complete usage guide and API reference
- **Protocol Spec**: Implements MCP version `2024-11-05`

---

## Dependencies

- **Blocks:** GAP-008 (Tool Migration) - Now unblocked
- **Related to:** GAP-002 (Async execution for MCP calls) - MCP manager handles async

---

## References

- [MCP Specification](https://modelcontextprotocol.io/specification/)
- [GRAND_ARCHITECTURE.md - MCPs](../GRAND_ARCHITECTURE.md#452-mcps-bundled-stateful)
- [MCP Servers Repository](https://github.com/modelcontextprotocol/servers)
- [server-everything npm package](https://www.npmjs.com/package/@modelcontextprotocol/server-everything)
