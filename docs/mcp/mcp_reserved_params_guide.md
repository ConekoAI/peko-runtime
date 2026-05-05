# MCP Reserved Parameter Injection Guide

## Overview

Pekobot extends MCP (Model Context Protocol) with **reserved parameter injection**, allowing runtime context (like `agent_id`, `session_id`) to be automatically injected into MCP tool calls. This enables:

- **Agent Isolation**: Memory and state automatically isolated per agent
- **Security**: LLM cannot spoof identity parameters
- **Audit**: Track which agent made each tool call
- **Multi-tenancy**: One MCP server serves multiple agents safely

## How It Works

### 1. Configure Reserved Parameters

In your `mcp.toml` configuration:

```toml
[[server]]
name = "my-server"
transport = "stdio"
command = "my-server-cmd"

[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
workspace = { source = "runtime", field = "workspace" }
```

### 2. LLM Sees Clean Schema

The tool schema exposed to the LLM does NOT include reserved parameters:

```json
{
  "name": "memory_store",
  "description": "Store a value in memory",
  "inputSchema": {
    "type": "object",
    "properties": {
      "key": { "type": "string" },
      "value": { "type": "string" }
    },
    "required": ["key", "value"]
  }
}
```

### 3. Server Receives Injected Context

When the LLM calls `memory_store({"key": "name", "value": "Alice"})`,
the MCP server receives:

```json
{
  "key": "name",
  "value": "Alice",
  "agent_id": "agent_abc123",
  "session_id": "sess_xyz789",
  "workspace": "/home/user/project"
}
```

## Configuration Reference

### Parameter Sources

#### 1. Runtime Context

Inject values from the current execution context:

```toml
[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
peer_id = { source = "runtime", field = "peer_id" }
workspace = { source = "runtime", field = "workspace" }
run_id = { source = "runtime", field = "run_id" }
tool_id = { source = "runtime", field = "tool_id" }
tool_name = { source = "runtime", field = "tool_name" }
```

Available runtime fields:
- `agent_id` - Current agent identifier
- `session_id` - Current session identifier  
- `peer_id` - Peer identifier (for distributed contexts)
- `workspace` - Workspace path
- `run_id` - Tool execution run identifier
- `tool_id` - Tool execution identifier
- `tool_name` - Name of the tool being called

#### 2. Environment Variables

Inject values from environment variables:

```toml
[server.reserved_parameters]
api_key = { source = "env", var = "MCP_API_KEY" }
database_url = { source = "env", var = "DATABASE_URL" }
```

#### 3. Static Values

Inject hardcoded values:

```toml
[server.reserved_parameters]
environment = { source = "static", value = "production" }
version = { source = "static", value = "1.0.0" }
```

### Complete Example

```toml
[[server]]
name = "analytics"
transport = "stdio"
command = "mcp-analytics"

[server.reserved_parameters]
# Runtime context
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
workspace = { source = "runtime", field = "workspace" }

# Environment
api_key = { source = "env", var = "ANALYTICS_API_KEY" }

# Static
environment = { source = "static", value = "production" }
```

## Use Cases

### 1. Isolated Memory Storage

Store/retrieve memories that are automatically isolated per agent:

```javascript
// MCP server code
const storageKey = `${args.agent_id}:${args.key}`;
await db.store(storageKey, args.value);
```

Each agent sees only their own data, even though they all use the same MCP server.

### 2. Audit Logging

Track every tool call with agent context:

```javascript
await auditLog.store({
  tool: "filesystem_write",
  path: args.path,
  agent_id: args.agent_id,
  session_id: args.session_id,
  timestamp: new Date()
});
```

### 3. Multi-tenant Database

One database MCP server serves all agents with automatic tenant isolation:

```javascript
const tenantTable = `data_${args.agent_id}`;
const result = await db.query(`SELECT * FROM ${tenantTable} WHERE ...`);
```

### 4. Rate Limiting

Enforce per-agent rate limits:

```javascript
const key = `rate_limit:${args.agent_id}`;
const count = await redis.incr(key);
if (count > 100) {
  throw new Error("Rate limit exceeded");
}
```

## MCP Server Implementation

To support reserved parameters, MCP servers should:

1. **Declare parameters as optional** in the tool schema:

```javascript
inputSchema: {
  type: "object",
  properties: {
    key: { type: "string" },
    value: { type: "string" },
    // Reserved params - not required
    agent_id: { type: "string" },
    session_id: { type: "string" }
  },
  required: ["key", "value"]  // Don't include reserved params
}
```

2. **Handle missing gracefully** - Params may be null if not configured:

```javascript
const agentId = args.agent_id || 'anonymous';
```

3. **Document the behavior** - Let users know about reserved parameter support

## Testing

### Test Reserved Parameter Injection

```bash
# Test with the memory example via agent conversation
pekobot send myagent "Store 'test' = 'hello' in memory"

# Verify isolation by checking stored values
pekobot send myagent "Retrieve the value for key 'test' from memory"
```

### Verify Schema Filtering

```bash
# List tools and verify reserved params are NOT in schema
pekobot ext info memory-server
```

## Troubleshooting

### Reserved params not injected

1. Check configuration is loaded:
   ```bash
   pekobot ext info my-server
   ```

2. Verify server has reserved_parameters:
   ```toml
   [server.reserved_parameters]
   agent_id = { source = "runtime", field = "agent_id" }
   ```

3. Check ToolContext has identity set (for programmatic usage)

### Server receives null values

- Runtime fields are only available when ToolContext has identity set
- For CLI testing without full agent context, values will be null
- This is expected - server should handle gracefully

### Schema still shows reserved params

- Schema filtering only removes from `properties` and `required`
- If server includes reserved params in `description`, they remain visible
- This is cosmetic - injection still works

## Comparison with Universal Tools

| Feature | MCP Tools | Universal Tools |
|---------|-----------|-----------------|
| Protocol | MCP (2024-11-05) | JSON-RPC 2.0 |
| Connection | Persistent | Process-per-call |
| Reserved params | Via adapter layer | Native support |
| Schema filtering | Yes | Yes |
| Sources | Runtime, Env, Static | Runtime, Env, Static |
| Use case | External servers | Agent-specific tools |

## Future Enhancements

Potential future improvements:

1. **Conditional injection** - Only inject based on tool name patterns
2. **Transform functions** - Hash or encode injected values
3. **Per-tool configuration** - Different reserved params per tool
4. **MCP spec extension** - Propose `x-reserved-context` standard

## See Also

- [MCP Reserved Parameters Proposal](./mcp_reserved_params_proposal.md) - Original design document
- [MCP Memory Server Example](../examples/mcp-memory-server/) - Example third-party MCP server
- [Universal Tools Documentation](./universal_tools.md) - Similar feature for native tools
