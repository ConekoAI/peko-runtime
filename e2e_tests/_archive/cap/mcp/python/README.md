# MCP Reserved Parameter Injection E2E Test

This E2E test demonstrates how Pekobot's MCP integration supports **reserved parameter injection** - a feature that allows runtime context (like `agent_id`, `session_id`) to be automatically injected into MCP tool calls, hidden from the LLM.

## Overview

The test verifies:
1. **MCP Server Management via CLI** - Add servers with reserved parameters using `pekobot cap mcp add`
2. **Tool Enablement via CLI** - Enable MCP tools for agents using `pekobot cap enable`
3. **Reserved Parameter Injection** - Runtime values are injected into tool calls
4. **Schema Filtering** - Reserved params are hidden from the LLM (not in tool schema)
5. **Agent Isolation** - MCP tools can use injected identity for secure multi-tenancy

## Quick Start

```powershell
# Set your API key
$env:MINIMAX_API_KEY = "your-api-key"

# Run the E2E test
.\e2e_tests\cap\mcp\python\test.ps1
```

## Improved CLI Workflow

This test demonstrates the improved MCP CLI workflow:

### 1. Add MCP Server with Reserved Parameters

```bash
pekobot cap mcp add identity \
    --transport stdio \
    --command python \
    --args ./mcp_server.py \
    --reserved "agent_id=runtime:agent_id" \
    --reserved "session_id=runtime:session_id"
```

Reserved parameter formats:
- `name=runtime:field` - Injects from ToolContext (e.g., `agent_id=runtime:agent_id`)
- `name=env:VAR_NAME` - Injects from environment variable (e.g., `api_key=env:API_KEY`)
- `name=static:value` - Injects static value (e.g., `env=static:production`)

### 2. Create Agent

```bash
pekobot agent create myagent --provider minimax
```

### 3. Enable MCP Tools for Agent

```bash
# Enable specific MCP tools
pekobot cap enable default/myagent echo_identity
pekobot cap enable default/myagent store_memory
pekobot cap enable default/myagent retrieve_memory
```

### 4. Verify and Test

```bash
# Check MCP server status
pekobot cap mcp list
pekobot cap mcp show identity

# Check agent capabilities
pekobot cap status default/myagent

# Test MCP server connection
pekobot cap mcp test identity

# Use the agent
pekobot send myagent "Use the echo_identity tool with message 'Hello'"
```

### 5. Cleanup

```bash
# Remove MCP server
pekobot cap mcp remove identity --force

# Delete agent
pekobot agent delete myagent --force
```

## Files

| File | Purpose |
|------|---------|
| `test.ps1` | Main E2E test script using CLI commands |
| `mcp_server.py` | Python MCP server demonstrating reserved params |
| `mcp-config.toml` | Example MCP configuration (legacy - now configured via CLI) |
| `README.md` | This file |

## MCP Server Implementation

The test uses a custom Python MCP server (`mcp_server.py`) that:

1. **Implements MCP Protocol** - JSON-RPC 2.0 over stdio
2. **Declares Tools** - Three tools demonstrating different aspects:
   - `echo_identity` - Shows injected identity parameters
   - `store_memory` - Uses agent isolation for storage keys
   - `retrieve_memory` - Retrieves from agent-isolated storage

3. **Handles Reserved Params** - Tools accept but don't require `agent_id`/`session_id`:
   ```python
   # Tool receives these injected by Pekobot
   agent_id = args.get("agent_id") or "not_injected"
   session_id = args.get("session_id") or "not_injected"
   ```

## How Reserved Parameter Injection Works

### Configuration (via CLI)
```bash
pekobot cap mcp add identity \
    --transport stdio \
    --command python \
    --args ./mcp_server.py \
    --reserved "agent_id=runtime:agent_id" \
    --reserved "session_id=runtime:session_id"
```

This creates the following configuration in `~/.pekobot/mcp.toml`:
```toml
[[server]]
name = "identity"
transport = "stdio"
command = "python"
args = ["./mcp_server.py"]

[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
```

### What the LLM Sees
```json
{
  "name": "echo_identity",
  "description": "Echo back the injected identity parameters...",
  "inputSchema": {
    "type": "object",
    "properties": {
      "message": { "type": "string" }
    },
    "required": ["message"]
  }
}
```
Note: `agent_id` and `session_id` are **NOT** in the schema!

### What the Server Receives
```json
{
  "message": "Hello MCP",
  "agent_id": "agent_abc123",      // Injected by Pekobot
  "session_id": "sess_xyz789"      // Injected by Pekobot
}
```

### Architecture Flow
```
┌──────────┐    ┌─────────────────────┐    ┌──────────────────┐    ┌─────────────┐
│   LLM    │───▶│ InjectableMcpTool   │───▶│   MCP Server     │───▶│  Tool Logic │
│          │    │ Proxy (Pekobot)     │    │ (Python)         │    │             │
└──────────┘    └─────────────────────┘    └──────────────────┘    └─────────────┘
       │                │                           │
       │                │ Injects:                  │ Receives:
       │                │   agent_id                │   message
       │ Sees:          │   session_id              │   agent_id
       │   message      │                           │   session_id
       │   (no agent_id)│                           │
```

## Expected Output

The test should output something like:

```
✓ MCP server 'identity' added successfully
✓ Agent created
✓ MCP tools enabled for agent
✓ Agent response mentions identity/injection
✓ MCP echo_identity tool was invoked (found in session)
✓ Memory storage and retrieval works correctly
✅ MCP Reserved Parameter Injection E2E test completed!
```

## Comparison: Old vs New Workflow

| Task | Old Workflow | New Workflow |
|------|--------------|--------------|
| Add MCP server | Copy config file manually | `pekobot cap mcp add` |
| Configure reserved params | Edit TOML file | `--reserved` flag |
| Enable tools for agent | Edit agent config.toml | `pekobot cap enable` |
| Check status | N/A | `pekobot cap status` |
| Remove server | Delete config file | `pekobot cap mcp remove` |

## Troubleshooting

### MCP Server Not Starting
- Check Python is installed: `python --version`
- Check the mcp_server.py path is correct
- Try manually: `python mcp_server.py`

### Reserved Params Not Injected
- Verify `pekobot cap mcp show <server>` shows reserved_parameters
- Check that agent_id and session_id are configured with `source = "runtime"`
- Verify the MCP tool schema includes optional agent_id/session_id params

### Agent Doesn't See MCP Tools
- Use `pekobot cap status default/myagent` to check enabled tools
- Ensure MCP server is running: `pekobot cap mcp test <server>`
- Check that tool names match exactly (case-sensitive)

## See Also

- [Universal Tool E2E Test](../tool/custom/python/) - Compare with Universal Tool workflow
- [MCP Documentation](../../../../docs/)
