# Universal Tool Protocol vs MCP: Architecture Comparison

## Executive Summary

Both implementations use **JSON-RPC 2.0** over **stdio** but serve different purposes:

| Aspect | Universal Tools | MCP |
|--------|-----------------|-----|
| **Purpose** | Custom agent tools with reserved param injection | External tool ecosystem (servers) |
| **Scope** | Per-agent tools in `tools/` directory | Global/external servers |
| **Reserved Params** | ✅ Yes (session_id, agent_id, etc.) | ❌ No |
| **Server Lifecycle** | Process-per-call | Persistent/long-running |
| **Capabilities** | Tools only | Tools + Resources + Prompts |
| **Discovery** | Filesystem scan | Configuration-based |

---

## Protocol Comparison

### Message Structure

**Universal Tools (Simplified):**
```json
{
  "jsonrpc": "2.0",
  "id": "req_123",
  "method": "tool/execute",
  "params": {
    "tool": "calculator",
    "args": {"operation": "add", "a": 10, "b": 32},
    "context": {
      "session_id": "sess_abc",
      "agent_id": "agent_001"
    }
  }
}
```

**MCP (Full Spec):**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "calculator",
    "arguments": {"operation": "add", "a": 10, "b": 32}
  }
}
```

**Key Difference:** Universal tools inject reserved params via `context`; MCP has no native reserved param concept.

---

## Architecture Comparison

### Universal Tools

```
┌─────────────────────────────────────────────────────────────┐
│  Agent Runtime (Peko)                                     │
│  ┌─────────────────┐     ┌─────────────────────────────────┐│
│  │ UniversalTool   │────▶│ JSON-RPC + Context Injection    ││
│  │ Adapter         │     │                                 ││
│  │                 │     │ • Injects session_id            ││
│  │ • Loads manifest│     │ • Injects agent_id              ││
│  │ • Validates args│     │ • Adds workspace, run_id        ││
│  │ • Merges params │     │                                 ││
│  └─────────────────┘     └─────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
                            │ stdio (one-shot)
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  Tool Process (Python/Node/etc.)                             │
│  ┌─────────────────┐     ┌─────────────────────────────────┐│
│  │ Language Adapter│────▶│ User Function                   ││
│  │                 │     │                                 ││
│  │ • Parses JSON   │     │ def tool(args, session_id, ...):││
│  │ • Calls handler │     │   # Reserved params injected    ││
│  │ • Returns JSON  │     │   return result                 ││
│  └─────────────────┘     └─────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

### MCP

```
┌─────────────────────────────────────────────────────────────┐
│  Agent Runtime (Peko)                                     │
│  ┌─────────────────┐     ┌─────────────────────────────────┐│
│  │ McpManager      │────▶│ McpClient                       ││
│  │                 │     │                                 ││
│  │ • Manages       │     │ • Persistent connection         ││
│  │   server pool   │     │ • Initialize/call/ping          ││
│  │ • Lifecycle mgmt│     │ • List tools/resources          ││
│  │ • Config-based  │     │                                 ││
│  └─────────────────┘     └─────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
                            │ stdio/SSE (persistent)
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  MCP Server (External Process)                               │
│  ┌─────────────────┐     ┌─────────────────────────────────┐│
│  │ MCP Protocol    │────▶│ Server Implementation             ││
│  │                 │     │                                 ││
│  │ • Initialize    │     │ • Exposes multiple tools        ││
│  │ • ListTools     │     │ • Resources (files/data)        ││
│  │ • CallTool      │     │ • Prompts (templates)           ││
│  │ • Resources     │     │ • Long-running                  ││
│  └─────────────────┘     └─────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Code Structure Comparison

### Universal Tools (`src/tools/universal/`)

| File | Responsibility | Lines |
|------|----------------|-------|
| `protocol.rs` | JSON-RPC types | ~200 |
| `manifest.rs` | Manifest v2, reserved params | ~280 |
| `transport.rs` | Stdio communication | ~150 |
| `adapter.rs` | Tool trait implementation | ~230 |
| `discovery.rs` | Filesystem scanning | ~280 |
| **Total** | | **~1,140** |

**Characteristics:**
- ✅ Minimal, focused
- ✅ Process-per-call (isolation)
- ✅ Reserved param injection
- ✅ Simple manifest format

### MCP (`src/mcp/`)

| File | Responsibility | Lines |
|------|----------------|-------|
| `types.rs` | Full MCP protocol types | ~600 |
| `transport.rs` | Stdio + SSE transports | ~400 |
| `client.rs` | MCP client (initialize, call) | ~350 |
| `manager.rs` | Server lifecycle management | ~400 |
| `tool_proxy.rs` | Tool trait adapter | ~400 |
| `discovery.rs` | Server discovery | ~200 |
| `config.rs` | Configuration | ~150 |
| **Total** | | **~2,500** |

**Characteristics:**
- ✅ Full MCP spec compliance
- ✅ Persistent connections
- ✅ Multi-transport (stdio, SSE)
- ✅ Resources + Prompts support
- ❌ No reserved param injection
- ❌ More complex (lifecycle management)

---

## Feature Matrix

| Feature | Universal | MCP | Notes |
|---------|:---------:|:---:|:------|
| **JSON-RPC 2.0** | ✅ | ✅ | Both use same base protocol |
| **Stdio Transport** | ✅ | ✅ | Primary transport for both |
| **Reserved Params** | ✅ | ❌ | Universal only |
| **Process-per-call** | ✅ | ❌ | Universal: ephemeral; MCP: persistent |
| **Resources** | ❌ | ✅ | MCP only |
| **Prompts** | ❌ | ✅ | MCP only |
| **SSE Transport** | ❌ | ✅ | MCP only |
| **Server Management** | ❌ | ✅ | MCP has lifecycle mgmt |
| **Multi-tool Servers** | ❌ | ✅ | One MCP server = many tools |
| **Filesystem Discovery** | ✅ | ❌ | Universal scans `tools/`; MCP uses config |

---

## When to Use Each

### Use Universal Tools When:

1. **You need reserved param injection** (session_id, agent_id)
2. **Tools are agent-specific** (in `tools/` directory)
3. **You want simple deployment** (just drop files)
4. **Tools are lightweight** (quick execution)
5. **You need custom tool logic per agent**

**Example:** A calculator tool that logs which agent performed the calculation.

### Use MCP When:

1. **You need resources/prompts** (not just tools)
2. **Tools are shared across agents** (global services)
3. **You want persistent connections** (stateful servers)
4. **You need external ecosystem** (connect to existing MCP servers)
5. **Tools are complex/long-running** (browser automation, etc.)

**Example:** A browser automation server that maintains state across calls.

---

## Integration Points

### Current Integration

Both systems integrate via `ToolFactory`:

```rust
// ToolFactory::create_tools_async()

// 1. Built-in tools (sync)
let tools = create_builtin_tools();

// 2. Universal tools (agent-specific)
let universal = load_tools_from_directory(&tools_dir).await;
tools.extend(universal);

// 3. MCP tools (global/external)
let mcp = create_tool_proxies(mcp_manager).await;
tools.extend(mcp);
```

### Resolution Order

1. **Built-in** tools (highest priority)
2. **Universal** tools (agent-specific)
3. **MCP** tools (lowest priority, no conflicts)

---

## Potential Convergence

### Could Universal Tools become MCP-compatible?

**Yes, with modifications:**

1. **Add initialization handshake** (MCP requires `initialize` method)
2. **Support `tools/list` method** (discovery)
3. **Rename `tool/execute` to `tools/call`**
4. **Add `ping` method** (health checks)

**Trade-offs:**
- ✅ Would be compatible with MCP ecosystem
- ❌ Would lose reserved param injection (MCP spec doesn't support it)
- ❌ More complex protocol (initialization, capabilities)

### Recommendation

**Keep both systems separate:**

- **Universal Tools**: Lightweight, agent-specific, reserved params
- **MCP**: Full spec, external ecosystem, resources/prompts

They serve different use cases well. Convergence would sacrifice Universal's simplicity or MCP's ecosystem compatibility.

---

## Summary

| | Universal Tools | MCP |
|--|-----------------|-----|
| **Philosophy** | Simple, agent-centric | Full-featured, ecosystem-centric |
| **Complexity** | Low (~1,140 lines) | High (~2,500 lines) |
| **Key Feature** | Reserved param injection | Resources + Prompts |
| **Best For** | Custom agent tools | External tool ecosystem |
| **Status** | ✅ Production-ready | ✅ Production-ready |

Both implementations are **production-ready** and serve their respective use cases well.
