# MCP Memory Server with Reserved Parameters

This example demonstrates MCP tools with reserved parameter injection for agent-isolated memory storage.

## Overview

This MCP server provides simple key-value memory storage that is automatically isolated per agent using reserved parameter injection.

## How It Works

1. **Tool Definition**: The `memory_store` tool declares optional `agent_id` and `session_id` parameters
2. **Manifest Configuration**: The extension manifest declares `extension_type: "mcp"` and embeds the server's startup command
3. **Auto-Isolation**: Keys are automatically prefixed with the agent ID, ensuring isolation

## Files

- `server.js` - MCP server implementation (the @modelcontextprotocol/sdk server with `memory_store`, `memory_retrieve`, `memory_list`, `memory_delete` tools)
- `manifest.yaml` - Peko extension manifest (ADR-024 unified format) that wires the server into the extension framework

## Configuration

The extension is configured via `manifest.yaml`:

```yaml
id: "mcp-memory-server"
name: "MCP Memory Server"
version: "1.0.0"
description: "Agent-isolated key-value memory storage"
extension_type: "mcp"

mcp_servers:
  mcp-memory-server:
    command: "node"
    args: ["examples/mcp-memory-server/server.js"]
    auto_start: true
```

The `auto_start: true` flag tells Peko to start the server process automatically when the extension is enabled. Reserved parameter injection (`agent_id`, `session_id`) is a Peko runtime feature — see `docs/mcp/mcp_reserved_params_guide.md` for the full mechanism.

## What the LLM Sees

```json
{
  "name": "memory_store",
  "description": "Store a value in memory",
  "inputSchema": {
    "type": "object",
    "properties": {
      "key": { "type": "string", "description": "Memory key" },
      "value": { "type": "string", "description": "Value to store" }
    },
    "required": ["key", "value"]
  }
}
```

Note: `agent_id` and `session_id` are NOT visible to the LLM!

## What the Server Receives

When agent `agent_abc123` calls `memory_store({"key": "user_name", "value": "Alice"})`,
the server receives:

```json
{
  "key": "user_name",
  "value": "Alice",
  "agent_id": "agent_abc123",
  "session_id": "sess_xyz789"
}
```

## Running

```bash
# 1. Install the extension (one-time, per host)
peko ext install examples/mcp-memory-server

# 2. Start the MCP server runtime
peko ext start mcp-memory-server

# 3. Verify it's running
peko ext list
# Expected output includes:
#   mcp-memory-server   mcp   enabled   ...
```

Once the server is running, any agent that has the `mcp-memory-server` extension enabled can call `memory_store`, `memory_retrieve`, `memory_list`, and `memory_delete` — the LLM sees only the user-facing parameters, and `agent_id` / `session_id` are injected automatically by the Peko runtime.

To test the server in isolation (outside Peko), you can run it directly with any MCP client:

```bash
node examples/mcp-memory-server/server.js
```

## See also

- `docs/mcp/MCP.md` — full MCP integration guide
- `docs/mcp/mcp_reserved_params_guide.md` — how reserved parameter injection works
- `src/extensions/framework/scaffold/templates/mcp/manifest.yaml` — canonical MCP manifest template
