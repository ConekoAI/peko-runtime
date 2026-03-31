# MCP Memory Server with Reserved Parameters

This example demonstrates MCP tools with reserved parameter injection for agent-isolated memory storage.

## Overview

This MCP server provides simple key-value memory storage that is automatically isolated per agent using reserved parameter injection.

## How It Works

1. **Tool Definition**: The `memory_store` tool declares optional `agent_id` and `session_id` parameters
2. **Pekobot Configuration**: Configure reserved parameters to inject `agent_id` from runtime context
3. **Auto-Isolation**: Keys are automatically prefixed with the agent ID, ensuring isolation

## Files

- `server.js` - MCP server implementation
- `mcp-config.toml` - Pekobot MCP configuration with reserved parameters

## Configuration

```toml
[[server]]
name = "memory"
transport = "stdio"
command = "node"
args = ["examples/mcp-memory-server/server.js"]

[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
```

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
# Start Pekobot with the example config
cargo run --bin pekobot -- --mcp-config examples/mcp-memory-server/mcp-config.toml

# Or test the server directly
pekobot tool test memory memory_store '{"key": "test", "value": "hello"}'
```
